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
//! Finding the scanner engine — the JAR that does the actual analysis.
//!
//! Either the user names one with `sonar.scanner.engineJarPath`, or the server is asked for the one
//! it wants this analysis to run, and it is downloaded into the cache. Unlike the JRE, the engine is
//! a single JAR: there is nothing to extract, and there is no fallback to something already on the
//! machine — the engine belongs to the server, and a mismatched one is not worth guessing at.

use std::path::PathBuf;

use log::{debug, info};
use serde::Deserialize;
use thiserror::Error;

use crate::cache::Cache;
use crate::config::Properties;
use crate::endpoint::Endpoint;
use crate::http::HttpClient;

/// A scanner engine JAR to use as it is, rather than downloading the server's.
pub const ENGINE_JAR_PATH: &str = "sonar.scanner.engineJarPath";

/// Reported to the engine, which forwards it as analysis telemetry.
pub const WAS_ENGINE_CACHE_HIT: &str = "sonar.scanner.wasEngineCacheHit";

const ENGINE_ENDPOINT: &str = "/analysis/engine";

#[derive(Debug, Error)]
pub enum EngineError {
    #[error("The scanner engine {path} named by '{ENGINE_JAR_PATH}' does not exist.")]
    MissingJar { path: PathBuf },
}

/// The engine JAR to run, and what to tell it about the engine cache.
#[derive(Debug, PartialEq, Eq)]
pub struct Engine {
    pub jar: PathBuf,
    pub cache_hit: bool,
}

/// What the server says about the engine it wants this analysis to run.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Metadata {
    filename: String,
    sha256: String,
    /// A direct download, usually on a CDN. Absent when only the API serves the JAR.
    download_url: Option<String>,
}

pub fn resolve(
    client: &HttpClient,
    properties: &Properties,
    endpoint: &Endpoint,
    cache: &Cache,
) -> crate::error::Result<Engine> {
    if let Some(path) = properties.get_non_blank(ENGINE_JAR_PATH) {
        let jar = PathBuf::from(path);
        // Checked here rather than left to the JVM: "no such file" beats a class-loading failure
        // several screens further on.
        if !jar.is_file() {
            return Err(EngineError::MissingJar { path: jar }.into());
        }
        info!("Using the scanner engine {path}, from '{ENGINE_JAR_PATH}'");
        // Nothing was cached, and the engine reports the property as a plain boolean, so a
        // user-supplied JAR is not a hit.
        return Ok(Engine { jar, cache_hit: false });
    }
    provision(client, endpoint, cache)
}

fn provision(client: &HttpClient, endpoint: &Endpoint, cache: &Cache) -> crate::error::Result<Engine> {
    let url = format!("{}{ENGINE_ENDPOINT}", endpoint.api_base_url);

    crate::cache::retrying_a_checksum_mismatch(|| {
        let metadata: Metadata = client.get_json(&url)?;
        debug!("The server wants the scanner engine {}", metadata.filename);
        let entry = cache.entry(&metadata.filename, &metadata.sha256)?;

        // A `downloadUrl` usually points at a CDN, which is a foreign origin and therefore gets no
        // token: that rule lives in the HTTP client, so both branches are the same call here. Without
        // one, the same URL serves the bytes; only the `Accept` header tells the two calls apart.
        let download_url = metadata.download_url.clone().unwrap_or_else(|| url.clone());
        let cached = entry.file(|sink| {
            info!("Downloading the scanner engine {} from {download_url}", metadata.filename);
            client.download(&download_url, sink)?;
            Ok(())
        })?;

        info!("Using the scanner engine at {}", cached.path.display());
        Ok(Engine { jar: cached.path, cache_hit: cached.hit })
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use sha2::{Digest as _, Sha256};
    use tempfile::TempDir;

    use crate::config::TOKEN;
    use crate::test_server::{Response, TestServer};

    /// Stands in for the engine JAR: nothing here ever looks inside it.
    const JAR_BYTES: &[u8] = b"PK\x03\x04 not really a jar";

    fn tempdir() -> TempDir {
        TempDir::with_prefix("cargo-sonar-scanner-engine-").unwrap()
    }

    fn checksum_of(bytes: &[u8]) -> String {
        Sha256::digest(bytes).iter().map(|byte| format!("{byte:02x}")).collect()
    }

    fn metadata_json(checksum: &str, download_url: Option<&str>) -> String {
        let download = download_url.map(|url| format!(r#","downloadUrl":"{url}""#)).unwrap_or_default();
        format!(r#"{{"filename":"sonar-scanner-engine-shaded.jar","sha256":"{checksum}"{download}}}"#)
    }

    fn endpoint_of(server: &TestServer) -> Endpoint {
        Endpoint {
            host_url: server.base_url(),
            api_base_url: format!("{}/api/v2", server.base_url()),
            is_cloud: false,
            region: String::new(),
        }
    }

    fn resolve_against(server: &TestServer, home: &TempDir, pairs: &[(&str, &str)]) -> crate::error::Result<Engine> {
        let endpoint = endpoint_of(server);
        let properties: Properties = [&[(TOKEN, "s3cr3t")], pairs]
            .concat()
            .iter()
            .map(|(key, value)| ((*key).to_string(), (*value).to_string()))
            .collect();
        let client = HttpClient::new(&properties, &endpoint).unwrap();
        resolve(&client, &properties, &endpoint, &Cache::new(home.path()))
    }

    /// A server serving the metadata and the JAR from the same URL, as a server without a CDN does.
    fn engine_server() -> TestServer {
        TestServer::start(|request| {
            if request.path != "/api/v2/analysis/engine" {
                Response::status(404)
            } else if request.header("accept") == Some("application/json") {
                Response::json(&metadata_json(&checksum_of(JAR_BYTES), None))
            } else {
                Response::bytes(JAR_BYTES)
            }
        })
    }

    #[test]
    fn uses_the_engine_jar_the_user_named() {
        let dir = tempdir();
        let home = tempdir();
        let jar = dir.path().join("my-engine.jar");
        std::fs::write(&jar, JAR_BYTES).unwrap();
        let server = TestServer::always(Response::status(500));

        let engine = resolve_against(&server, &home, &[(ENGINE_JAR_PATH, jar.to_str().unwrap())]).unwrap();

        assert_eq!(engine, Engine { jar, cache_hit: false });
        assert!(server.requests().is_empty(), "a named engine is used as it is, without asking the server");
    }

    #[test]
    fn reports_an_engine_jar_that_is_not_there() {
        let dir = tempdir();
        let home = tempdir();
        let missing = dir.path().join("absent.jar");
        let server = TestServer::always(Response::status(500));

        let failure = resolve_against(&server, &home, &[(ENGINE_JAR_PATH, missing.to_str().unwrap())]).unwrap_err();

        assert_eq!(
            failure.to_string(),
            format!("The scanner engine {} named by 'sonar.scanner.engineJarPath' does not exist.", missing.display())
        );
    }

    #[test]
    fn downloads_the_engine_the_server_wants() {
        let home = tempdir();
        let server = engine_server();

        let engine = resolve_against(&server, &home, &[]).unwrap();

        let checksum = checksum_of(JAR_BYTES);
        assert_eq!(engine.jar, home.path().join("cache").join(&checksum).join("sonar-scanner-engine-shaded.jar"));
        assert_eq!(std::fs::read(&engine.jar).unwrap(), JAR_BYTES);
        assert!(!engine.cache_hit);

        let requests = server.requests();
        assert_eq!(requests.len(), 2);
        // Without a `downloadUrl`, one URL serves both the metadata and the bytes; `Accept` is what
        // tells the server which of the two is being asked for.
        for request in &requests {
            assert_eq!(request.path, "/api/v2/analysis/engine");
            assert_eq!(request.header("authorization"), Some("Bearer s3cr3t"));
        }
        assert_eq!(requests[0].header("accept"), Some("application/json"));
        assert_eq!(requests[1].header("accept"), Some("application/octet-stream"));
    }

    #[test]
    fn reuses_a_downloaded_engine() {
        let home = tempdir();
        let server = engine_server();
        let first = resolve_against(&server, &home, &[]).unwrap();

        let second = resolve_against(&server, &home, &[]).unwrap();

        assert_eq!(second, Engine { jar: first.jar, cache_hit: true });
        let requests = server.requests();
        let downloads = requests.iter().filter(|request| request.header("accept") == Some("application/octet-stream"));
        assert_eq!(downloads.count(), 1, "the JAR is downloaded once; only the metadata is asked for again");
    }

    /// The property that matters about a CDN download: the token stays with the server it belongs to.
    #[test]
    fn downloads_from_a_cdn_without_the_token() {
        let home = tempdir();
        let cdn = TestServer::always(Response::bytes(JAR_BYTES));
        let cdn_url = cdn.url("/downloads/engine.jar");
        let server = TestServer::always(Response::json(&metadata_json(&checksum_of(JAR_BYTES), Some(&cdn_url))));

        let engine = resolve_against(&server, &home, &[]).unwrap();

        assert_eq!(std::fs::read(&engine.jar).unwrap(), JAR_BYTES);
        let download = cdn.last_request();
        assert_eq!(download.path, "/downloads/engine.jar");
        assert_eq!(download.header("authorization"), None, "the CDN is a foreign origin");
    }

    /// The engine may be republished between the metadata call and the download, which invalidates the
    /// checksum we were comparing against, so the retry starts from the metadata again.
    #[test]
    fn asks_again_when_the_download_does_not_match_the_checksum() {
        let home = tempdir();
        let calls = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&calls);
        let server = TestServer::start(move |request| {
            if request.header("accept") == Some("application/json") {
                let stale = counter.fetch_add(1, Ordering::Relaxed) == 0;
                let reported = if stale { "0".repeat(64) } else { checksum_of(JAR_BYTES) };
                Response::json(&metadata_json(&reported, None))
            } else {
                Response::bytes(JAR_BYTES)
            }
        });

        let engine = resolve_against(&server, &home, &[]).unwrap();

        assert_eq!(std::fs::read(&engine.jar).unwrap(), JAR_BYTES);
        assert_eq!(calls.load(Ordering::Relaxed), 2, "the metadata is read again, not just the download retried");
    }

    #[test]
    fn gives_up_when_the_download_never_matches_the_checksum() {
        let home = tempdir();
        let server = TestServer::start(|request| {
            if request.header("accept") == Some("application/json") {
                Response::json(&metadata_json(&"0".repeat(64), None))
            } else {
                Response::bytes(JAR_BYTES)
            }
        });

        let failure = resolve_against(&server, &home, &[]).unwrap_err();

        assert!(failure.to_string().contains("has checksum"), "{failure}");
        let requests = server.requests();
        let metadata_calls = requests.iter().filter(|request| request.header("accept") == Some("application/json"));
        assert_eq!(metadata_calls.count(), 2, "exactly one retry");
    }

    /// This name is the engine's vocabulary, not ours: it forwards the value as telemetry.
    #[test]
    fn names_the_cache_outcome_the_way_the_engine_expects() {
        assert_eq!(WAS_ENGINE_CACHE_HIT, "sonar.scanner.wasEngineCacheHit");
    }
}
