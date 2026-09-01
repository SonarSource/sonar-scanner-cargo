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
//! The HTTP client used for every Sonar API call and every artifact download.
//!
//! Two properties of this module are security-relevant and are asserted by the tests rather than
//! left to the underlying library's defaults:
//!
//! * The authentication token is attached **only** to requests whose origin is the resolved
//!   `sonar.host.url` or `sonar.scanner.apiBaseUrl`. Artifact metadata routinely points at a
//!   third-party CDN, and the token has no business being sent there.
//! * A redirect that leaves the origin drops the credential, which is the resolution of the conflict
//!   between "auth survives redirects" and "never leak the token" recorded as OQ-6 in the
//!   implementation plan. Redirects are therefore followed by this module, not by the agent.
//!
//! TLS verification uses the platform's trust store, not a bundled root set, so that a corporate
//! TLS-intercepting proxy works as soon as its root is trusted by the machine.

use std::io::Write;
use std::path::PathBuf;
use std::time::Duration;

use log::{debug, warn};
use serde::de::DeserializeOwned;
use thiserror::Error;
use ureq::tls::{Certificate, ClientCert, PrivateKey, RootCerts, TlsConfig, TlsProvider};
use ureq::{Agent, Body, Proxy, ProxyProtocol, http::Response};
use url::Url;

use crate::config::{Properties, TOKEN, USER_HOME};
use crate::endpoint::Endpoint;

/// Seconds to wait for the TCP connection and the TLS handshake.
pub const CONNECT_TIMEOUT: &str = "sonar.scanner.connectTimeout";
/// Seconds to wait for the response head once the request has been sent, and for the body of
/// everything but a download.
pub const SOCKET_TIMEOUT: &str = "sonar.scanner.socketTimeout";
/// Seconds allowed for a whole call, body included. `0` means unlimited.
pub const RESPONSE_TIMEOUT: &str = "sonar.scanner.responseTimeout";

pub const PROXY_HOST: &str = "sonar.scanner.proxyHost";
pub const PROXY_PORT: &str = "sonar.scanner.proxyPort";
pub const PROXY_USER: &str = "sonar.scanner.proxyUser";
pub const PROXY_PASSWORD: &str = "sonar.scanner.proxyPassword";

const DEFAULT_CONNECT_TIMEOUT: u64 = 5;
const DEFAULT_SOCKET_TIMEOUT: u64 = 60;
const DEFAULT_RESPONSE_TIMEOUT: u64 = 0;
/// Default proxy port, as in the Java library: HTTP's, whatever the target scheme is.
const DEFAULT_PROXY_PORT: u16 = 80;

/// A sanity ceiling on a download. The JRE and the engine are tens of megabytes; anything past this
/// is a misconfigured URL rather than an artifact, and the read is aborted instead of filling a disk.
const MAX_DOWNLOAD_SIZE: u64 = 1 << 30;

/// Cap on an API response body. `ureq` defaults to 10 MB, which the artifact *metadata* never
/// approaches, but being explicit keeps the limit from moving under us on an upgrade.
const MAX_BODY_SIZE: u64 = 10 * 1024 * 1024;

/// Redirect hops allowed before giving up, matching the agent's own default.
const MAX_REDIRECTS: usize = 10;

const JSON: &str = "application/json";
const OCTET_STREAM: &str = "application/octet-stream";

/// What a call is fetching, which decides how its response is bounded in time.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Transfer {
    /// An API response of a few hundred bytes, for which the socket timeout can bound the whole
    /// exchange.
    Metadata,
    /// An artifact, where the socket timeout bounds each read but not the transfer as a whole. Tens
    /// of megabytes over a slow link legitimately take longer than any per-call budget, and only
    /// `sonar.scanner.responseTimeout` puts a ceiling on the total.
    Artifact,
}

pub type Result<T> = std::result::Result<T, HttpError>;

#[derive(Debug, Error)]
pub enum HttpError {
    /// Carries the whole user-facing sentence, because the wording is specified per product.
    #[error("{0}")]
    Unauthorized(String),
    #[error("{0}")]
    Forbidden(String),
    #[error("GET {url} returned HTTP {status}")]
    Status { status: u16, url: String },
    #[error("Gave up after {MAX_REDIRECTS} redirects, the last one to {url}")]
    TooManyRedirects { url: String },
    #[error("{0}")]
    Tls(#[from] crate::tls::TlsError),
    #[error("The client certificate key from the keystore could not be used")]
    ClientKey {
        #[source]
        source: ureq::Error,
    },
    #[error("Failed to call {url}")]
    Transport {
        url: String,
        #[source]
        source: ureq::Error,
    },
    #[error("Failed to read the response from {url}")]
    ReadResponse {
        url: String,
        #[source]
        source: ureq::Error,
    },
    /// A failure part-way through a download: either the transfer or the write to the cache.
    #[error("Failed to download {url}")]
    Download {
        url: String,
        #[source]
        source: std::io::Error,
    },
    #[error("Failed to parse the response from {url} as JSON")]
    Json {
        url: String,
        #[source]
        source: serde_json::Error,
    },
    #[error("Invalid proxy configuration: {message}")]
    Proxy {
        message: String,
        #[source]
        source: ureq::Error,
    },
}

pub struct HttpClient {
    agent: Agent,
    token: Option<String>,
    /// The origins allowed to receive the token: those of the host URL and of the API base URL.
    trusted_origins: Vec<String>,
    /// How to name the endpoint in an authentication failure, e.g. `SonarQube Cloud [us]`.
    product: String,
    /// Where a user goes to fix a rejected token. Carries no trailing slash: [`Endpoint`] strips it.
    host_url: String,
}

/// Build the TLS configuration, honouring `sonar.scanner.truststorePath` and `keystorePath`.
///
/// `RootCerts` is an enum, so "the platform verifier, plus these" cannot be asked for: naming
/// specific roots switches the built-in ones off. [`crate::tls`] therefore returns the platform's
/// own roots alongside the truststore's, and this only leaves `PlatformVerifier` in place when there
/// is no truststore at all — which is the common case, and the one where the verifier's
/// per-certificate trust decisions are worth keeping.
fn tls_config(properties: &Properties) -> Result<TlsConfig> {
    let user_home = PathBuf::from(properties.get(USER_HOME).unwrap_or_default());
    let stores = crate::tls::resolve(properties, &user_home)?;

    let mut builder = TlsConfig::builder().provider(TlsProvider::NativeTls);
    builder = match &stores.roots {
        Some(roots) => {
            builder.root_certs(RootCerts::from(roots.iter().map(|der| Certificate::from_der(der).to_owned())))
        }
        None => builder.root_certs(RootCerts::PlatformVerifier),
    };
    if let Some(identity) = &stores.client_identity {
        let chain: Vec<Certificate<'static>> =
            identity.chain.iter().map(|der| Certificate::from_der(der).to_owned()).collect();
        // The key goes in as PEM even though we hold DER, because `PrivateKey::from_der` takes a
        // `KeyKind` that `ureq` does not re-export — it is unnameable here, so that constructor
        // cannot be called. `PRIVATE KEY` is the PKCS#8 label, which is how a PKCS#12 file stores
        // the key once decrypted, and PKCS#8 is the only kind the native-tls backend accepts.
        let key = PrivateKey::from_pem(pem(&identity.key, "PRIVATE KEY").as_bytes())
            .map_err(|source| HttpError::ClientKey { source })?;
        builder = builder.client_cert(Some(ClientCert::new_with_certs(&chain, key)));
    }
    Ok(builder.build())
}

/// DER to PEM, 64 characters to a line as the format requires.
fn pem(der: &[u8], label: &str) -> String {
    use base64::Engine;
    let encoded = base64::engine::general_purpose::STANDARD.encode(der);
    let mut out = format!("-----BEGIN {label}-----\n");
    for line in encoded.as_bytes().chunks(64) {
        out.push_str(std::str::from_utf8(line).expect("base64 is ASCII"));
        out.push('\n');
    }
    out.push_str(&format!("-----END {label}-----\n"));
    out
}

impl HttpClient {
    pub fn new(properties: &Properties, endpoint: &Endpoint) -> Result<Self> {
        let socket_timeout = seconds(properties, SOCKET_TIMEOUT, DEFAULT_SOCKET_TIMEOUT);
        let mut builder = Agent::config_builder()
            .user_agent(format!("cargo-sonar-scanner/{}", env!("CARGO_PKG_VERSION")))
            // Statuses are inspected here so that each one gets the message the guidelines specify.
            .http_status_as_error(false)
            // Redirects are followed by `get`, see there.
            .max_redirects(0)
            .max_redirects_will_error(false)
            .timeout_connect(Some(seconds(properties, CONNECT_TIMEOUT, DEFAULT_CONNECT_TIMEOUT)))
            // Two bounds, because `ureq` keeps the head deadline in force while the body is read: the
            // first is the wait for the response head *and* the whole exchange, the second is a wait
            // per read, recomputed as the body arrives. An artifact download lifts the first one, see
            // [`Transfer`].
            .timeout_recv_response(Some(socket_timeout))
            .timeout_recv_body(Some(socket_timeout))
            .timeout_global(optional_seconds(properties, RESPONSE_TIMEOUT, DEFAULT_RESPONSE_TIMEOUT))
            .tls_config(tls_config(properties)?);
        // Left alone, the agent picks the proxy up from the environment; only override it when the
        // properties actually configure one.
        if let Some(proxy) = configured_proxy(properties)? {
            builder = builder.proxy(Some(proxy));
        }

        let token = properties.get_non_blank(TOKEN).map(str::to_string);
        if token.is_some()
            && (properties.get_non_blank("sonar.login").is_some()
                || properties.get_non_blank("sonar.password").is_some())
        {
            warn!(
                "Both 'sonar.token' and the deprecated 'sonar.login'/'sonar.password' are set. \
                 Only 'sonar.token' is used to authenticate."
            );
        }

        let trusted_origins =
            [endpoint.host_url.as_str(), endpoint.api_base_url.as_str()].iter().filter_map(|url| origin(url)).collect();

        Ok(HttpClient {
            agent: builder.build().new_agent(),
            token,
            trusted_origins,
            product: endpoint.product(),
            host_url: endpoint.host_url.clone(),
        })
    }

    /// GET `url`, returning the body as text.
    ///
    /// No specific media type is requested — the agent's default `Accept: */*` stands — because the
    /// version endpoints answer with plain text.
    pub fn get_string(&self, url: &str) -> Result<String> {
        let mut response = self.get(url, None, Transfer::Metadata)?;
        response
            .body_mut()
            .with_config()
            .limit(MAX_BODY_SIZE)
            .read_to_string()
            .map_err(|source| HttpError::ReadResponse { url: url.to_string(), source })
    }

    /// GET `url` and deserialize the JSON body.
    pub fn get_json<T: DeserializeOwned>(&self, url: &str) -> Result<T> {
        let mut response = self.get(url, Some(JSON), Transfer::Metadata)?;
        let body = response
            .body_mut()
            .with_config()
            .limit(MAX_BODY_SIZE)
            .read_to_string()
            .map_err(|source| HttpError::ReadResponse { url: url.to_string(), source })?;
        serde_json::from_str(&body).map_err(|source| HttpError::Json { url: url.to_string(), source })
    }

    /// GET `url` and stream the body into `sink`, returning the number of bytes written.
    ///
    /// Streaming matters: the JRE and the engine are large enough that buffering them whole before
    /// writing would double the peak memory for no gain.
    pub fn download(&self, url: &str, sink: &mut dyn Write) -> Result<u64> {
        let mut response = self.get(url, Some(OCTET_STREAM), Transfer::Artifact)?;
        let mut reader = response.body_mut().with_config().limit(MAX_DOWNLOAD_SIZE).reader();
        std::io::copy(&mut reader, sink).map_err(|source| HttpError::Download { url: url.to_string(), source })
    }

    /// The one place a request is issued, so that authentication and status mapping cannot diverge
    /// between callers.
    ///
    /// Redirects are followed here rather than by the agent, because the agent's `SameHost` rule
    /// compares the host and the scheme but not the port, and the credential rule this module owes
    /// its callers is per *origin*. Following them ourselves means every hop goes through
    /// [`Self::credential_for`], one rule, one place.
    fn get(&self, url: &str, accept: Option<&str>, transfer: Transfer) -> Result<Response<Body>> {
        let mut url = url.to_string();
        for _ in 0..MAX_REDIRECTS {
            let response = self.call(&url, accept, transfer)?;
            if !response.status().is_redirection() {
                return self.check(&url, response);
            }
            let location = response
                .headers()
                .get("location")
                .and_then(|location| location.to_str().ok())
                .and_then(|location| resolve_location(&url, location));
            match location {
                // A redirect we cannot follow is reported as the status it is, not silently.
                None => return self.check(&url, response),
                Some(next) => {
                    debug!("Following the redirect to {next}");
                    url = next;
                }
            }
        }
        Err(HttpError::TooManyRedirects { url })
    }

    fn call(&self, url: &str, accept: Option<&str>, transfer: Transfer) -> Result<Response<Body>> {
        let mut request = self.agent.get(url);
        if transfer == Transfer::Artifact {
            // Left in place, the head deadline would cap the transfer as a whole at the budget of a
            // metadata call. Dropping it leaves the per-read timeout to catch a peer that goes quiet,
            // and the head of this one call unbounded unless 'sonar.scanner.responseTimeout' is set.
            request = request.config().timeout_recv_response(None).build();
        }
        if let Some(accept) = accept {
            request = request.header("Accept", accept);
        }
        match self.credential_for(url) {
            Some(token) => request = request.header("Authorization", format!("Bearer {token}")),
            None if self.token.is_some() => {
                debug!("Not authenticating the request to {url}: it is not an origin of the Sonar endpoint");
            }
            None => {}
        }

        let response = request.call().map_err(|source| HttpError::Transport { url: url.to_string(), source })?;
        debug!("GET {url} -> {}", response.status());
        Ok(response)
    }

    /// Map a final response onto the errors the guidelines specify, per product.
    fn check(&self, url: &str, response: Response<Body>) -> Result<Response<Body>> {
        let status = response.status().as_u16();
        match status {
            200..=299 => Ok(response),
            401 => Err(HttpError::Unauthorized(format!(
                "Unable to authenticate on {}. Please check your token or generate a new one at {}/account/security",
                self.product, self.host_url
            ))),
            403 if self.product.starts_with("SonarQube Cloud") => Err(HttpError::Forbidden(format!(
                "You don't have permission to execute an analysis in any organization on {}.",
                self.product
            ))),
            403 => Err(HttpError::Forbidden(
                "You don't have permission to execute an analysis on this SonarQube Server instance.".to_string(),
            )),
            _ => Err(HttpError::Status { status, url: url.to_string() }),
        }
    }

    /// The token, but only for a URL on an origin of the Sonar endpoint.
    fn credential_for(&self, url: &str) -> Option<&str> {
        let origin = origin(url)?;
        self.trusted_origins.contains(&origin).then_some(self.token.as_deref())?
    }
}

/// Turn a `Location` header into an absolute URL. Servers are entitled to send a relative one, and
/// the artifact endpoints of a reverse-proxied SonarQube Server do.
fn resolve_location(base: &str, location: &str) -> Option<String> {
    // An empty reference resolves to the base URL itself, which as a redirect target is a loop.
    if location.is_empty() {
        return None;
    }
    Url::parse(base).ok()?.join(location).ok().map(String::from)
}

/// The origin of `url` — scheme, host and non-default port — as the standard serializes it, so that
/// `https://sq.example.com` and `https://SQ.example.com:443/` compare equal.
///
/// A scheme with no host has no origin to compare, and `None` denies it the token rather than letting
/// it match anything.
fn origin(url: &str) -> Option<String> {
    let origin = Url::parse(url).ok()?.origin();
    origin.is_tuple().then(|| origin.ascii_serialization())
}

fn seconds(properties: &Properties, key: &str, default: u64) -> Duration {
    Duration::from_secs(number(properties, key, default))
}

/// Like [`seconds`], except that `0` disables the timeout rather than expiring immediately.
fn optional_seconds(properties: &Properties, key: &str, default: u64) -> Option<Duration> {
    match number(properties, key, default) {
        0 => None,
        value => Some(Duration::from_secs(value)),
    }
}

/// A numeric property, falling back to `default` when it is absent or unparseable — a bad timeout
/// is not worth failing an analysis over, but it is worth a warning.
fn number(properties: &Properties, key: &str, default: u64) -> u64 {
    match properties.get_non_blank(key) {
        None => default,
        Some(value) => value.parse().unwrap_or_else(|_| {
            warn!("Ignoring '{key}={value}': expected a number of seconds. Using {default}.");
            default
        }),
    }
}

fn configured_proxy(properties: &Properties) -> Result<Option<Proxy>> {
    let Some(host) = properties.get_non_blank(PROXY_HOST) else {
        return Ok(None);
    };
    let port = match properties.get_non_blank(PROXY_PORT) {
        None => DEFAULT_PROXY_PORT,
        Some(value) => value.parse().unwrap_or_else(|_| {
            warn!("Ignoring '{PROXY_PORT}={value}': expected a port number. Using {DEFAULT_PROXY_PORT}.");
            DEFAULT_PROXY_PORT
        }),
    };

    let mut builder = Proxy::builder(ProxyProtocol::Http).host(host).port(port);
    if let Some(user) = properties.get_non_blank(PROXY_USER) {
        builder = builder.username(user);
    }
    if let Some(password) = properties.get_non_blank(PROXY_PASSWORD) {
        builder = builder.password(password);
    }
    debug!("Using the configured proxy {host}:{port}");
    builder
        .build()
        .map(Some)
        .map_err(|source| HttpError::Proxy { message: format!("cannot use '{host}:{port}'"), source })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_server::{Response as TestResponse, TestServer};
    use serde::Deserialize;

    /// A client trusting `server` as both the host and the API base URL, as a Server endpoint does.
    fn client_for(server: &TestServer) -> HttpClient {
        client_with(server, &[(TOKEN, "s3cr3t")])
    }

    fn client_with(server: &TestServer, properties: &[(&str, &str)]) -> HttpClient {
        let properties: Properties =
            properties.iter().map(|(key, value)| ((*key).to_string(), (*value).to_string())).collect();
        let endpoint = Endpoint {
            host_url: server.base_url(),
            api_base_url: format!("{}/api/v2", server.base_url()),
            is_cloud: false,
            region: String::new(),
        };
        HttpClient::new(&properties, &endpoint).unwrap()
    }

    fn cloud_client(host: &str, region: &str) -> HttpClient {
        let endpoint = Endpoint {
            host_url: host.to_string(),
            api_base_url: host.replace("https://", "https://api."),
            is_cloud: true,
            region: region.to_string(),
        };
        HttpClient::new(&Properties::new(), &endpoint).unwrap()
    }

    #[test]
    fn authenticates_pre_emptively_with_a_bearer_token() {
        let server = TestServer::always(TestResponse::json(r#"{"version":"2025.1"}"#));
        let client = client_for(&server);

        client.get_string(&server.url("/api/v2/analysis/version")).unwrap();

        let request = server.last_request();
        assert_eq!(request.method, "GET");
        assert_eq!(request.header("authorization"), Some("Bearer s3cr3t"));
        assert_eq!(request.path, "/api/v2/analysis/version");
        assert_eq!(request.header("accept"), Some("*/*"), "a plain-text endpoint gets no specific Accept");
    }

    #[test]
    fn sends_no_authorization_header_without_a_token() {
        let server = TestServer::always(TestResponse::text("ok"));
        let client = client_with(&server, &[]);

        client.get_string(&server.url("/whatever")).unwrap();

        assert_eq!(server.last_request().header("authorization"), None);
    }

    /// The artifact endpoints hand out CDN URLs. The token must not follow.
    #[test]
    fn never_sends_the_token_to_a_foreign_origin() {
        let sonar = TestServer::always(TestResponse::text("ok"));
        let cdn = TestServer::always(TestResponse::bytes(b"artifact"));
        let client = client_for(&sonar);

        let mut downloaded = Vec::new();
        client.download(&cdn.url("/jre.tar.gz"), &mut downloaded).unwrap();

        assert_eq!(downloaded, b"artifact");
        assert_eq!(cdn.last_request().header("authorization"), None);
    }

    #[test]
    fn keeps_the_token_across_a_same_origin_redirect() {
        let server = TestServer::start(|request| match request.path.as_str() {
            "/api/v2/analysis/jres/1" => TestResponse::redirect("/artifacts/jre.tar.gz"),
            _ => TestResponse::bytes(b"jre bytes"),
        });
        let client = client_for(&server);

        let mut downloaded = Vec::new();
        client.download(&server.url("/api/v2/analysis/jres/1"), &mut downloaded).unwrap();

        assert_eq!(downloaded, b"jre bytes");
        let requests = server.requests();
        assert_eq!(requests.len(), 2, "expected the redirect to be followed");
        for request in requests {
            assert_eq!(request.header("authorization"), Some("Bearer s3cr3t"), "on {}", request.path);
        }
    }

    /// OQ-6: authentication survives a redirect, but not one that leaves the origin. Two loopback
    /// ports are two origins even though they are one host — which is exactly the case the agent's
    /// own `SameHost` rule would have let through.
    #[test]
    fn drops_the_token_across_a_cross_origin_redirect() {
        let cdn = TestServer::always(TestResponse::bytes(b"jre bytes"));
        let sonar = TestServer::always(TestResponse::redirect(&cdn.url("/artifacts/jre.tar.gz")));
        let client = client_for(&sonar);

        let mut downloaded = Vec::new();
        client.download(&sonar.url("/api/v2/analysis/jres/1"), &mut downloaded).unwrap();

        assert_eq!(downloaded, b"jre bytes");
        assert_eq!(sonar.last_request().header("authorization"), Some("Bearer s3cr3t"));
        assert_eq!(cdn.last_request().header("authorization"), None);
    }

    #[test]
    fn follows_a_relative_redirect() {
        let server = TestServer::start(|request| match request.path.as_str() {
            "/api/v2/analysis/jres/1" => TestResponse::redirect("../../../../artifacts/jre.tar.gz"),
            "/artifacts/jre.tar.gz" => TestResponse::bytes(b"jre bytes"),
            other => TestResponse::status(404).with_body(other.as_bytes()),
        });
        let client = client_for(&server);

        let mut downloaded = Vec::new();
        client.download(&server.url("/api/v2/analysis/jres/1"), &mut downloaded).unwrap();

        assert_eq!(downloaded, b"jre bytes");
    }

    #[test]
    fn gives_up_on_a_redirect_loop() {
        let server = TestServer::start(|request| TestResponse::redirect(&format!("{}x", request.path)));
        let client = client_for(&server);

        let error = client.get_string(&server.url("/loop")).unwrap_err();

        assert!(error.to_string().starts_with("Gave up after 10 redirects"), "{error}");
        assert_eq!(server.requests().len(), MAX_REDIRECTS);
    }

    /// A redirect without a usable `Location` is reported as the status it is; it must not be
    /// mistaken for a successful response with an empty body.
    #[test]
    fn reports_a_redirect_that_cannot_be_followed() {
        let server = TestServer::always(TestResponse::status(302));
        let client = client_for(&server);

        let error = client.get_string(&server.url("/api/v2/analysis/engine")).unwrap_err();

        assert!(error.to_string().ends_with("returned HTTP 302"), "{error}");
    }

    /// Resolution is the `url` crate's; what this pins down is the set of `Location` shapes the
    /// artifact endpoints actually send, and that none of them is mistaken for another.
    #[test]
    fn resolves_a_location_header() {
        let base = "https://sq.example.com/api/v2/analysis/jres/1?os=linux";
        assert_eq!(
            resolve_location(base, "https://cdn.example.com/jre").as_deref(),
            Some("https://cdn.example.com/jre")
        );
        assert_eq!(resolve_location(base, "//cdn.example.com/jre").as_deref(), Some("https://cdn.example.com/jre"));
        assert_eq!(resolve_location(base, "/artifacts/jre").as_deref(), Some("https://sq.example.com/artifacts/jre"));
        assert_eq!(
            resolve_location(base, "2").as_deref(),
            Some("https://sq.example.com/api/v2/analysis/jres/2"),
            "a relative location resolves against the directory, and drops the query"
        );
        assert_eq!(resolve_location("https://sq.example.com", "jre").as_deref(), Some("https://sq.example.com/jre"));
        assert_eq!(resolve_location(base, ""), None);
        assert_eq!(
            resolve_location(base, "../../../../artifacts/jre").as_deref(),
            Some("https://sq.example.com/artifacts/jre"),
            "dot segments are resolved, and cannot climb above the root"
        );
        assert_eq!(resolve_location(base, "./2").as_deref(), Some("https://sq.example.com/api/v2/analysis/jres/2"));
        assert_eq!(
            resolve_location(base, "http://sq.example.com:8443/jre").as_deref(),
            Some("http://sq.example.com:8443/jre"),
            "another port is a different URL, and below a different origin"
        );
        assert_eq!(resolve_location("not a url", "/artifacts/jre"), None, "nothing resolves against nothing");
    }

    #[test]
    fn deserializes_a_json_body() {
        #[derive(Deserialize)]
        struct Metadata {
            filename: String,
            sha256: String,
        }
        let server = TestServer::always(TestResponse::json(r#"{"filename":"engine.jar","sha256":"abc"}"#));
        let client = client_for(&server);

        let metadata: Metadata = client.get_json(&server.url("/api/v2/analysis/engine")).unwrap();

        assert_eq!(metadata.filename, "engine.jar");
        assert_eq!(metadata.sha256, "abc");
        assert_eq!(server.last_request().header("accept"), Some("application/json"));
    }

    #[test]
    fn reports_a_body_that_is_not_json() {
        let server = TestServer::always(TestResponse::text("<html>a proxy error page</html>"));
        let client = client_for(&server);

        let error = client.get_json::<serde_json::Value>(&server.url("/api/v2/analysis/engine")).unwrap_err();

        assert!(error.to_string().contains("as JSON"), "{error}");
    }

    /// A peer that answers the head and then goes quiet must not hang the analysis. One second is the
    /// smallest socket timeout the properties can express, and the server stops stalling as soon as
    /// the client closes the connection, so the wait is that second and no more.
    #[test]
    fn gives_up_on_a_body_that_stops_arriving() {
        let server = TestServer::always(TestResponse::json(r#"{"filename":"#).stalling(64));
        let client = client_with(&server, &[(SOCKET_TIMEOUT, "1")]);

        let error = client.get_json::<serde_json::Value>(&server.url("/api/v2/analysis/engine")).unwrap_err();

        assert!(error.to_string().contains("Failed to read the response"), "{error}");
    }

    /// The socket timeout bounds each read of a download, not the transfer as a whole. A JRE of tens
    /// of megabytes over a home connection takes longer than any per-call budget, and cutting it off
    /// at the budget of a metadata call is what `Transfer::Artifact` exists to prevent. Three bytes
    /// 500ms apart against a one-second timeout: no read waits a whole second, the transfer takes 1.5.
    #[test]
    fn allows_a_download_slower_than_the_socket_timeout() {
        let server = TestServer::always(TestResponse::bytes(b"jre").paced(Duration::from_millis(500)));
        let client = client_with(&server, &[(SOCKET_TIMEOUT, "1")]);

        let mut downloaded = Vec::new();
        client.download(&server.url("/artifacts/jre.tar.gz"), &mut downloaded).unwrap();

        assert_eq!(downloaded, b"jre");
    }

    /// Lifting the head deadline for a download does not leave it unbounded: the per-read timeout
    /// still catches a peer that promises bytes and then goes quiet.
    #[test]
    fn gives_up_on_a_download_that_stops_arriving() {
        let server = TestServer::always(TestResponse::bytes(b"jre").stalling(64));
        let client = client_with(&server, &[(SOCKET_TIMEOUT, "1")]);

        let mut downloaded = Vec::new();
        let error = client.download(&server.url("/artifacts/jre.tar.gz"), &mut downloaded).unwrap_err();

        assert!(error.to_string().contains("Failed to download"), "{error}");
    }

    #[test]
    fn streams_a_download_larger_than_one_buffer() {
        let artifact: Vec<u8> = (0..300_000u32).map(|index| index as u8).collect();
        let server = TestServer::always(TestResponse::bytes(&artifact));
        let client = client_for(&server);

        let mut downloaded = Vec::new();
        let written = client.download(&server.url("/artifacts/jre.tar.gz"), &mut downloaded).unwrap();

        assert_eq!(written, artifact.len() as u64);
        assert_eq!(downloaded, artifact);
        assert_eq!(server.last_request().header("accept"), Some("application/octet-stream"));
    }

    #[test]
    fn explains_a_rejected_token_on_a_server() {
        let server = TestServer::always(TestResponse::status(401));
        let client = client_for(&server);

        let error = client.get_string(&server.url("/api/v2/analysis/version")).unwrap_err();

        assert_eq!(
            error.to_string(),
            format!(
                "Unable to authenticate on SonarQube Server. Please check your token or generate a new one at {}/account/security",
                server.base_url()
            )
        );
        assert!(matches!(error, HttpError::Unauthorized(_)), "{error:?}");
    }

    #[test]
    fn explains_a_rejected_token_on_cloud() {
        let global = cloud_client("https://sonarcloud.io", "");
        let us = cloud_client("https://sonarqube.us", "us");

        assert_eq!(
            unauthorized(&global),
            "Unable to authenticate on SonarQube Cloud. Please check your token or generate a new one at https://sonarcloud.io/account/security"
        );
        assert_eq!(
            unauthorized(&us),
            "Unable to authenticate on SonarQube Cloud [us]. Please check your token or generate a new one at https://sonarqube.us/account/security"
        );
    }

    #[test]
    fn explains_missing_permissions_per_product() {
        let server = TestServer::always(TestResponse::status(403));
        let client = client_for(&server);

        let error = client.get_string(&server.url("/api/v2/analysis/version")).unwrap_err();

        assert_eq!(
            error.to_string(),
            "You don't have permission to execute an analysis on this SonarQube Server instance."
        );
        assert!(matches!(error, HttpError::Forbidden(_)), "{error:?}");
        assert_eq!(
            forbidden(&cloud_client("https://sonarcloud.io", "")),
            "You don't have permission to execute an analysis in any organization on SonarQube Cloud."
        );
        assert_eq!(
            forbidden(&cloud_client("https://sonarqube.us", "us")),
            "You don't have permission to execute an analysis in any organization on SonarQube Cloud [us]."
        );
    }

    #[test]
    fn reports_any_other_status_with_the_url() {
        let server = TestServer::always(TestResponse::status(503));
        let client = client_for(&server);

        let error = client.get_string(&server.url("/api/v2/analysis/version")).unwrap_err();

        assert!(error.to_string().contains("returned HTTP 503"), "{error}");
    }

    #[test]
    fn reports_an_unreachable_host() {
        // Port 1 on loopback: reserved, and nothing listens there.
        let client = client_with(&TestServer::always(TestResponse::text("unused")), &[]);

        let error = client.get_string("http://127.0.0.1:1/api/v2/analysis/version").unwrap_err();

        assert!(error.to_string().starts_with("Failed to call http://127.0.0.1:1/"), "{error}");
    }

    /// A trailing slash on `sonar.host.url` must not double up in the message. The client leans on the
    /// endpoint having stripped it, so the property goes through the resolver here instead of being
    /// written into an `Endpoint` by hand.
    #[test]
    fn does_not_double_the_slash_in_the_token_url() {
        let properties: Properties =
            [(crate::config::HOST_URL.to_string(), "https://sq.example.com/".to_string())].into_iter().collect();
        let endpoint = crate::endpoint::resolve(&properties).unwrap();
        let client = HttpClient::new(&properties, &endpoint).unwrap();

        assert!(
            unauthorized(&client).ends_with("https://sq.example.com/account/security"),
            "{}",
            unauthorized(&client)
        );
    }

    #[test]
    fn trusts_both_the_host_and_the_api_origins() {
        let endpoint = Endpoint {
            host_url: "https://sonarcloud.io".to_string(),
            api_base_url: "https://api.sonarcloud.io".to_string(),
            is_cloud: true,
            region: String::new(),
        };
        let properties: Properties = [(TOKEN.to_string(), "s3cr3t".to_string())].into_iter().collect();
        let client = HttpClient::new(&properties, &endpoint).unwrap();

        assert_eq!(client.credential_for("https://sonarcloud.io/account"), Some("s3cr3t"));
        assert_eq!(client.credential_for("https://api.sonarcloud.io/analysis/version"), Some("s3cr3t"));
        // Same suffix, different host: a classic near-miss that must not be trusted.
        assert_eq!(client.credential_for("https://evil-sonarcloud.io/x"), None);
        assert_eq!(client.credential_for("https://cdn.example.com/jre.tar.gz"), None);
        // Downgrading the scheme is a different origin.
        assert_eq!(client.credential_for("http://sonarcloud.io/account"), None);
    }

    #[test]
    fn normalises_origins() {
        assert_eq!(origin("https://SQ.Example.com:443/api/v2?x=1").as_deref(), Some("https://sq.example.com"));
        assert_eq!(origin("http://sq.example.com:80").as_deref(), Some("http://sq.example.com"));
        assert_eq!(origin("https://sq.example.com:8443/").as_deref(), Some("https://sq.example.com:8443"));
        assert_eq!(origin("https://user:pass@sq.example.com/x").as_deref(), Some("https://sq.example.com"));
        assert_eq!(origin("not a url"), None);
        assert_eq!(origin("https://"), None, "no host, so nothing to compare");
        // The standard skips the extra slash for a scheme that has an authority, so this is a host.
        assert_eq!(origin("https:///sq.example.com").as_deref(), Some("https://sq.example.com"));
        // A scheme that carries no host has an opaque origin, which must not match a trusted one.
        assert_eq!(origin("mailto:someone@sq.example.com"), None);
    }

    #[test]
    fn reads_the_timeouts_from_the_properties() {
        let properties: Properties = [
            (CONNECT_TIMEOUT.to_string(), "10".to_string()),
            (SOCKET_TIMEOUT.to_string(), "not a number".to_string()),
            (RESPONSE_TIMEOUT.to_string(), "0".to_string()),
        ]
        .into_iter()
        .collect();

        assert_eq!(seconds(&properties, CONNECT_TIMEOUT, DEFAULT_CONNECT_TIMEOUT), Duration::from_secs(10));
        // Unparseable falls back rather than failing the analysis.
        assert_eq!(seconds(&properties, SOCKET_TIMEOUT, DEFAULT_SOCKET_TIMEOUT), Duration::from_secs(60));
        assert_eq!(optional_seconds(&properties, RESPONSE_TIMEOUT, DEFAULT_RESPONSE_TIMEOUT), None);
        assert_eq!(
            optional_seconds(&Properties::new(), RESPONSE_TIMEOUT, DEFAULT_RESPONSE_TIMEOUT),
            None,
            "the default response timeout is unlimited"
        );
    }

    #[test]
    fn configures_a_proxy_from_the_properties() {
        let properties: Properties = [
            (PROXY_HOST.to_string(), "proxy.example.com".to_string()),
            (PROXY_PORT.to_string(), "3128".to_string()),
            (PROXY_USER.to_string(), "scanner".to_string()),
            (PROXY_PASSWORD.to_string(), "s3cr3t".to_string()),
        ]
        .into_iter()
        .collect();

        let proxy = configured_proxy(&properties).unwrap().unwrap();

        assert_eq!(proxy.host(), "proxy.example.com");
        assert_eq!(proxy.port(), 3128);
        assert_eq!(proxy.username(), Some("scanner"));
        assert_eq!(proxy.password(), Some("s3cr3t"));
    }

    #[test]
    fn falls_back_to_the_default_proxy_port() {
        let properties: Properties = [(PROXY_HOST.to_string(), "proxy.example.com".to_string())].into_iter().collect();

        assert_eq!(configured_proxy(&properties).unwrap().unwrap().port(), DEFAULT_PROXY_PORT);
        assert!(configured_proxy(&Properties::new()).unwrap().is_none(), "no proxy host means no explicit proxy");
    }

    fn unauthorized(client: &HttpClient) -> String {
        message(client, 401)
    }

    fn forbidden(client: &HttpClient) -> String {
        message(client, 403)
    }

    /// The message for `status`, obtained from a local server that returns it. The client's own
    /// endpoint is unreachable in these cases, so the URL is deliberately untrusted — which also
    /// shows the wording does not depend on the request having been authenticated.
    fn message(client: &HttpClient, status: u16) -> String {
        let server = TestServer::always(TestResponse::status(status));
        client.get_string(&server.url("/api/v2/analysis/version")).unwrap_err().to_string()
    }
}
