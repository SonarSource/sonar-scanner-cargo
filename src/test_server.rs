/*
 * SonarScanner for Cargo
 * Copyright (C) SonarSource Sàrl
 * mailto:info AT sonarsource DOT com
 *
 * You can redistribute and/or modify this program under the terms of
 * the Sonar Source-Available License Version 1, as published by SonarSource Sàrl.
 *
 * This program is distributed in the hope that it will be useful,
 * but WITHOUT ANY WARRANTY; without even the implied warranty of
 * MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.
 * See the Sonar Source-Available License for more details.
 *
 * You should have received a copy of the Sonar Source-Available License
 * along with this program; if not, see https://sonarsource.com/license/ssal/
 */
//! A real HTTP server for the tests.
//!
//! The provisioning tests run against this rather than against a mocked client, because the things
//! that can go wrong — a header that is or is not sent, a redirect to another origin, a body read in
//! chunks — are properties of the real client and the real socket, and a mock would assert our
//! assumptions instead of the behaviour. Two servers can be started to obtain two distinct origins,
//! which is how the credential-leak tests are written.

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

/// A request as the server received it.
#[derive(Debug, Clone)]
pub struct Request {
    pub method: String,
    pub path: String,
    /// Header names are lowercased, because HTTP header names are case-insensitive.
    pub headers: BTreeMap<String, String>,
}

impl Request {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.get(&name.to_ascii_lowercase()).map(String::as_str)
    }
}

/// A response for the handler to return.
#[derive(Debug, Clone)]
pub struct Response {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
    /// Bytes promised by `Content-Length` and never sent, see [`Response::stalling`].
    pub unsent: usize,
    /// Pause between two bytes of the body, see [`Response::paced`].
    pub pace: Option<Duration>,
}

impl Response {
    pub fn status(status: u16) -> Self {
        Response { status, headers: Vec::new(), body: Vec::new(), unsent: 0, pace: None }
    }

    pub fn json(body: &str) -> Self {
        Response::status(200).with_header("Content-Type", "application/json").with_body(body.as_bytes())
    }

    pub fn text(body: &str) -> Self {
        Response::status(200).with_header("Content-Type", "text/plain").with_body(body.as_bytes())
    }

    pub fn bytes(body: &[u8]) -> Self {
        Response::status(200).with_header("Content-Type", "application/octet-stream").with_body(body)
    }

    /// A 302 to `location`, which is what the artifact endpoints do when they hand off to a CDN.
    pub fn redirect(location: &str) -> Self {
        Response::status(302).with_header("Location", location)
    }

    pub fn with_header(mut self, name: &str, value: &str) -> Self {
        self.headers.push((name.to_string(), value.to_string()));
        self
    }

    pub fn with_body(mut self, body: &[u8]) -> Self {
        self.body = body.to_vec();
        self
    }

    /// Promise `unsent` more bytes than the body carries and then wait, which is what a peer that
    /// goes quiet part-way through a response looks like to a client.
    pub fn stalling(mut self, unsent: usize) -> Self {
        self.unsent = unsent;
        self
    }

    /// Send the body one byte at a time, `gap` apart, as a slow link does.
    pub fn paced(mut self, gap: Duration) -> Self {
        self.pace = Some(gap);
        self
    }
}

type Handler = Box<dyn Fn(&Request) -> Response + Send + Sync>;

pub struct TestServer {
    address: SocketAddr,
    requests: Arc<Mutex<Vec<Request>>>,
    stopping: Arc<AtomicBool>,
    accept_loop: Option<JoinHandle<()>>,
}

impl TestServer {
    /// Start a server on a free loopback port, answering every request through `handler`.
    pub fn start(handler: impl Fn(&Request) -> Response + Send + Sync + 'static) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("failed to bind a test server");
        let address = listener.local_addr().expect("no local address");
        let requests = Arc::new(Mutex::new(Vec::new()));
        let stopping = Arc::new(AtomicBool::new(false));

        let handler: Handler = Box::new(handler);
        let accept_loop = std::thread::spawn({
            let requests = Arc::clone(&requests);
            let stopping = Arc::clone(&stopping);
            move || {
                for connection in listener.incoming() {
                    if stopping.load(Ordering::SeqCst) {
                        return;
                    }
                    let Ok(mut connection) = connection else { continue };
                    if let Some(request) = read_request(&mut connection) {
                        let response = handler(&request);
                        requests.lock().expect("poisoned").push(request);
                        let _ = write_response(&mut connection, &response);
                    }
                }
            }
        });

        TestServer { address, requests, stopping, accept_loop: Some(accept_loop) }
    }

    /// A server that answers every request with the same response.
    pub fn always(response: Response) -> Self {
        TestServer::start(move |_| response.clone())
    }

    pub fn base_url(&self) -> String {
        format!("http://{}", self.address)
    }

    pub fn url(&self, path: &str) -> String {
        format!("{}{path}", self.base_url())
    }

    /// Every request received so far, in order.
    pub fn requests(&self) -> Vec<Request> {
        self.requests.lock().expect("poisoned").clone()
    }

    /// The last request, for the common case of a single call.
    pub fn last_request(&self) -> Request {
        self.requests().pop().expect("the server received no request")
    }
}

impl Drop for TestServer {
    fn drop(&mut self) {
        // `incoming()` blocks in `accept`, so unblock it with one connection after raising the flag.
        self.stopping.store(true, Ordering::SeqCst);
        let _ = TcpStream::connect(self.address);
        if let Some(accept_loop) = self.accept_loop.take() {
            let _ = accept_loop.join();
        }
    }
}

fn read_request(connection: &mut TcpStream) -> Option<Request> {
    let mut reader = BufReader::new(connection.try_clone().ok()?);

    let mut request_line = String::new();
    if reader.read_line(&mut request_line).ok()? == 0 {
        return None; // The connection was opened and closed, as `Drop` does to unblock `accept`.
    }
    let mut parts = request_line.split_whitespace();
    let method = parts.next()?.to_string();
    let path = parts.next()?.to_string();

    let mut headers = BTreeMap::new();
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line).ok()? == 0 {
            break;
        }
        let line = line.trim_end();
        if line.is_empty() {
            break;
        }
        if let Some((name, value)) = line.split_once(':') {
            headers.insert(name.trim().to_ascii_lowercase(), value.trim().to_string());
        }
    }

    // Nothing asserts on a request body yet, but it has to leave the socket for the exchange to be
    // a well-formed one.
    let length: usize = headers.get("content-length").and_then(|value| value.parse().ok()).unwrap_or(0);
    if length > 0 {
        reader.read_exact(&mut vec![0; length]).ok()?;
    }

    Some(Request { method, path, headers })
}

fn write_response(connection: &mut TcpStream, response: &Response) -> std::io::Result<()> {
    let mut head = format!("HTTP/1.1 {} {}\r\n", response.status, reason(response.status));
    for (name, value) in &response.headers {
        head.push_str(&format!("{name}: {value}\r\n"));
    }
    // One request per connection: closing is simpler than honouring keep-alive, and the client
    // pools connections either way.
    head.push_str(&format!("Content-Length: {}\r\nConnection: close\r\n\r\n", response.body.len() + response.unsent));

    connection.write_all(head.as_bytes())?;
    match response.pace {
        None => connection.write_all(&response.body)?,
        Some(gap) => {
            for byte in &response.body {
                std::thread::sleep(gap);
                connection.write_all(&[*byte])?;
                connection.flush()?;
            }
        }
    }
    connection.flush()?;

    if response.unsent > 0 {
        // The promised bytes never come. Waiting for the client to give up and close its end keeps
        // the stall no longer than the test needs it, without a sleep to guess at.
        let _ = connection.read(&mut [0u8; 1]);
    }
    Ok(())
}

fn reason(status: u16) -> &'static str {
    match status {
        200 => "OK",
        204 => "No Content",
        302 => "Found",
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        500 => "Internal Server Error",
        503 => "Service Unavailable",
        _ => "Status",
    }
}
