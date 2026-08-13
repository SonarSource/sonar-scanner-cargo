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
//! Finding the Java runtime that will run the scanner engine.
//!
//! In order:
//!
//! 1. `sonar.scanner.javaExePath`, if the user named one.
//! 2. The JRE the server offers for this platform, downloaded into the cache and extracted — unless
//!    `sonar.scanner.skipJreProvisioning` says not to, or the server has none for this platform.
//! 3. `JAVA_HOME/bin/java`.
//! 4. `java` on the `PATH`.
//!
//! Provisioning is what makes an analysis work on a machine with no Java at all, which is the point
//! of a Cargo-native scanner: a Rust developer has no reason to have a JVM installed.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use log::{debug, info, warn};
use serde::Deserialize;
use thiserror::Error;

use crate::cache::Cache;
use crate::config::Properties;
use crate::endpoint::Endpoint;
use crate::http::HttpClient;
use crate::platform::Platform;

/// A Java executable to use as it is, rather than provisioning one.
pub const JAVA_EXE_PATH: &str = "sonar.scanner.javaExePath";

/// Whether to leave the JRE alone and use whatever Java the machine already has.
pub const SKIP_JRE_PROVISIONING: &str = "sonar.scanner.skipJreProvisioning";

/// Reported to the engine, which forwards it as analysis telemetry.
pub const WAS_JRE_CACHE_HIT: &str = "sonar.scanner.wasJreCacheHit";

const JRES_ENDPOINT: &str = "/analysis/jres";

#[cfg(windows)]
const JAVA_BINARY: &str = "java.exe";
#[cfg(not(windows))]
const JAVA_BINARY: &str = "java";

#[derive(Debug, Error)]
pub enum JreError {
    #[error(
        "The JRE provisioned from {filename} has no Java executable at {path}. \
         Delete the cache directory and run the analysis again."
    )]
    MissingJavaBinary { filename: String, path: PathBuf },

    #[error("The server reported {java_path:?} as the Java executable of {filename}, which is not a path inside it.")]
    UnusableJavaPath { filename: String, java_path: String },

    #[error(
        "No Java runtime was found. Install Java 17 or later and set JAVA_HOME, or put java on the PATH, \
         or point -Dsonar.scanner.javaExePath=<path> at one, or let the scanner provision a JRE by \
         unsetting -Dsonar.scanner.skipJreProvisioning."
    )]
    NoJavaRuntime,
}

/// The Java runtime to run the engine with, and what to tell the engine about the JRE cache.
#[derive(Debug, PartialEq, Eq)]
pub struct Jre {
    pub java_exe: PathBuf,
    pub cache_hit: CacheHit,
}

/// The three values `sonar.scanner.wasJreCacheHit` takes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CacheHit {
    Hit,
    Miss,
    /// Nothing was provisioned, so the cache was not consulted at all.
    Disabled,
}

impl CacheHit {
    pub fn as_str(self) -> &'static str {
        match self {
            CacheHit::Hit => "HIT",
            CacheHit::Miss => "MISS",
            CacheHit::Disabled => "DISABLED",
        }
    }
}

/// What the server says about the JRE for one platform. `os` and `arch` are echoed back and ignored.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Metadata {
    id: String,
    filename: String,
    sha256: String,
    /// The Java executable's path inside the archive, e.g. `jdk-17.0.13+11-jre/bin/java`.
    java_path: String,
    /// A direct download, usually on a CDN. Absent when only the API serves the archive.
    download_url: Option<String>,
}

pub fn resolve(
    client: &HttpClient,
    properties: &Properties,
    endpoint: &Endpoint,
    platform: &Platform,
    cache: &Cache,
    env: &BTreeMap<String, String>,
) -> crate::error::Result<Jre> {
    if let Some(path) = properties.get_non_blank(JAVA_EXE_PATH) {
        info!("Using the Java executable {path}, from '{JAVA_EXE_PATH}'");
        return Ok(Jre { java_exe: PathBuf::from(path), cache_hit: CacheHit::Disabled });
    }
    if properties.get_bool(SKIP_JRE_PROVISIONING) {
        info!("JRE provisioning is disabled by '{SKIP_JRE_PROVISIONING}'");
        return Ok(Jre { java_exe: local(env)?, cache_hit: CacheHit::Disabled });
    }
    match provision(client, endpoint, platform, cache)? {
        Some(jre) => Ok(jre),
        None => {
            // A server that serves no JRE for this platform is not misconfigured: it simply does not
            // ship one, and the machine's own Java is then the only option.
            info!(
                "The server offers no JRE for {}/{}, falling back to the Java runtime of this machine",
                platform.os, platform.arch
            );
            Ok(Jre { java_exe: local(env)?, cache_hit: CacheHit::Disabled })
        }
    }
}

/// The provisioned JRE, or `None` when the server offers none for this platform.
fn provision(
    client: &HttpClient,
    endpoint: &Endpoint,
    platform: &Platform,
    cache: &Cache,
) -> crate::error::Result<Option<Jre>> {
    let api_base_url = endpoint.api_base_url.as_str();
    let url = format!("{api_base_url}{JRES_ENDPOINT}?os={}&arch={}", encoded(&platform.os), encoded(&platform.arch));

    // Two attempts at most. A checksum mismatch is re-read from the metadata rather than retried
    // against the same URL: if the JRE was republished between the two calls, the checksum we are
    // comparing against is the stale one and no number of downloads will ever match it.
    for attempt in 1..=2 {
        // The server returns the JREs it considers usable, best first.
        let jres: Vec<Metadata> = client.get_json(&url)?;
        let Some(metadata) = jres.into_iter().next() else {
            return Ok(None);
        };
        debug!("The server offers the JRE {} ({})", metadata.filename, metadata.id);

        match install(client, api_base_url, cache, &metadata) {
            Err(failure) if attempt == 1 && is_checksum_mismatch(&failure) => {
                warn!("{failure}");
                warn!("Asking the server about the JRE again, in case it was republished mid-download");
            }
            result => return result.map(Some),
        }
    }
    unreachable!("the loop returns on its second attempt")
}

fn install(client: &HttpClient, api_base_url: &str, cache: &Cache, metadata: &Metadata) -> crate::error::Result<Jre> {
    let java_path = Path::new(&metadata.java_path);
    if !crate::archive::is_inside(java_path) {
        return Err(JreError::UnusableJavaPath {
            filename: metadata.filename.clone(),
            java_path: metadata.java_path.clone(),
        }
        .into());
    }
    let entry = cache.entry(&metadata.filename, &metadata.sha256)?;

    // A `downloadUrl` usually points at a CDN, which is a foreign origin and therefore gets no token:
    // that rule lives in the HTTP client, so both branches are the same call here.
    // The id comes from the server's JSON and lands in a path segment, so it is encoded rather than
    // pasted: a `/` or a `?` in it would otherwise decide which URL is called.
    let url = metadata
        .download_url
        .clone()
        .unwrap_or_else(|| format!("{api_base_url}{JRES_ENDPOINT}/{}", encoded(&metadata.id)));

    let archive = entry.file(|sink| {
        info!("Downloading the JRE {} from {url}", metadata.filename);
        client.download(&url, sink)?;
        Ok(())
    })?;
    let extracted = entry.extracted(|archive, into| {
        crate::archive::extract(archive, into)?;
        Ok(())
    })?;

    let java_exe = extracted.path.join(java_path);
    if !java_exe.is_file() {
        return Err(JreError::MissingJavaBinary { filename: metadata.filename.clone(), path: java_exe }.into());
    }
    info!("Using the provisioned JRE at {}", java_exe.display());
    // Only a JRE that needed no work at all is a hit: an archive that was cached but not yet
    // extracted still cost this analysis the extraction.
    let cache_hit = if archive.hit && extracted.hit { CacheHit::Hit } else { CacheHit::Miss };
    Ok(Jre { java_exe, cache_hit })
}

fn is_checksum_mismatch(failure: &crate::error::ScannerError) -> bool {
    matches!(failure, crate::error::ScannerError::Cache(error) if error.is_checksum_mismatch())
}

/// The Java runtime this machine already has, from `JAVA_HOME` or from the `PATH`.
fn local(env: &BTreeMap<String, String>) -> crate::error::Result<PathBuf> {
    if let Some(home) = variable(env, "JAVA_HOME").map(str::trim).filter(|home| !home.is_empty()) {
        let candidate = Path::new(home).join("bin").join(JAVA_BINARY);
        if candidate.is_file() {
            info!("Using the Java runtime of JAVA_HOME, at {}", candidate.display());
            return Ok(candidate);
        }
        warn!("JAVA_HOME is {home}, which contains no bin/{JAVA_BINARY}; looking on the PATH instead");
    }
    match on_path(variable(env, "PATH").unwrap_or_default()) {
        Some(java_exe) => {
            info!("Using the Java runtime of the PATH, at {}", java_exe.display());
            Ok(java_exe)
        }
        None => Err(JreError::NoJavaRuntime.into()),
    }
}

fn on_path(path: &str) -> Option<PathBuf> {
    std::env::split_paths(path)
        // A relative entry, `.` above all, must not get to decide which java runs — that is the
        // untrusted search path Windows is famous for.
        .filter(|directory| directory.is_absolute())
        .map(|directory| directory.join(JAVA_BINARY))
        .find(|candidate| candidate.is_file())
}

/// Environment variable names are case-insensitive on Windows, where `PATH` is spelled `Path`.
fn variable<'a>(env: &'a BTreeMap<String, String>, name: &str) -> Option<&'a str> {
    if cfg!(windows) {
        env.iter().find(|(key, _)| key.eq_ignore_ascii_case(name)).map(|(_, value)| value.as_str())
    } else {
        env.get(name).map(String::as_str)
    }
}

/// Percent-encode a single query parameter value or path segment: everything outside the unreserved
/// set goes, which is safe in both positions. The os, the arch and the JRE id are ordinary tokens in
/// practice, but two come from configuration and one from the server, and all three end up in a URL.
fn encoded(value: &str) -> String {
    value
        .bytes()
        .map(|byte| match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => (byte as char).to_string(),
            _ => format!("%{byte:02X}"),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use sha2::{Digest as _, Sha256};
    use tempfile::TempDir;

    use crate::archive::tests::tar_gz;
    use crate::config::TOKEN;
    use crate::test_server::{Response, TestServer};

    const JAVA_SCRIPT: &str = "#!/bin/sh\nexec true\n";

    fn tempdir() -> TempDir {
        TempDir::with_prefix("cargo-sonar-scanner-jre-").unwrap()
    }

    fn checksum_of(path: &Path) -> String {
        Sha256::digest(std::fs::read(path).unwrap()).iter().map(|byte| format!("{byte:02x}")).collect()
    }

    /// A JRE archive containing `jre/bin/java`, with its bytes and its checksum.
    fn jre_archive(dir: &TempDir) -> (Vec<u8>, String) {
        let path = tar_gz(dir.path(), "jre.tar.gz", &[("jre/bin/java", JAVA_SCRIPT, 0o755)]);
        (std::fs::read(&path).unwrap(), checksum_of(&path))
    }

    fn metadata_json(checksum: &str, download_url: Option<&str>) -> String {
        let download = download_url.map(|url| format!(r#","downloadUrl":"{url}""#)).unwrap_or_default();
        format!(
            r#"[{{"id":"jre-1","filename":"jre.tar.gz","sha256":"{checksum}","javaPath":"jre/bin/java",
                 "os":"linux","arch":"x86_64"{download}}}]"#
        )
    }

    fn endpoint_of(server: &TestServer) -> Endpoint {
        Endpoint {
            host_url: server.base_url(),
            api_base_url: format!("{}/api/v2", server.base_url()),
            is_cloud: false,
            region: String::new(),
        }
    }

    fn properties(pairs: &[(&str, &str)]) -> Properties {
        [&[(TOKEN, "s3cr3t")], pairs].concat().iter().map(|(key, value)| (key.to_string(), value.to_string())).collect()
    }

    fn linux() -> Platform {
        Platform { os: "linux".to_string(), arch: "x86_64".to_string() }
    }

    fn resolve_against(
        server: &TestServer,
        home: &TempDir,
        pairs: &[(&str, &str)],
        env: &[(&str, &str)],
    ) -> crate::error::Result<Jre> {
        let endpoint = endpoint_of(server);
        let properties = properties(pairs);
        let client = HttpClient::new(&properties, &endpoint).unwrap();
        let cache = Cache::new(home.path());
        let env = env.iter().map(|(key, value)| ((*key).to_string(), (*value).to_string())).collect();
        resolve(&client, &properties, &endpoint, &linux(), &cache, &env)
    }

    /// A server serving the JRE metadata and the archive from the API, as a server without a CDN does.
    fn jre_server(archive: Vec<u8>, checksum: String) -> TestServer {
        TestServer::start(move |request| {
            if request.path.starts_with("/api/v2/analysis/jres?") {
                Response::json(&metadata_json(&checksum, None))
            } else if request.path == "/api/v2/analysis/jres/jre-1" {
                Response::bytes(&archive)
            } else {
                Response::status(404)
            }
        })
    }

    #[test]
    fn uses_the_java_executable_the_user_named() {
        let home = tempdir();
        let server = TestServer::always(Response::status(500));

        let jre = resolve_against(&server, &home, &[(JAVA_EXE_PATH, "/opt/jdk/bin/java")], &[]).unwrap();

        assert_eq!(jre, Jre { java_exe: PathBuf::from("/opt/jdk/bin/java"), cache_hit: CacheHit::Disabled });
        assert!(server.requests().is_empty(), "a named Java executable is used as it is, without asking the server");
    }

    #[test]
    fn provisions_the_jre_the_server_offers() {
        let dir = tempdir();
        let home = tempdir();
        let (archive, checksum) = jre_archive(&dir);
        let server = jre_server(archive, checksum.clone());

        let jre = resolve_against(&server, &home, &[], &[]).unwrap();

        assert_eq!(
            jre.java_exe,
            home.path().join("cache").join(&checksum).join("jre.tar.gz_extracted").join("jre/bin/java")
        );
        assert_eq!(std::fs::read_to_string(&jre.java_exe).unwrap(), JAVA_SCRIPT);
        assert_eq!(jre.cache_hit, CacheHit::Miss);

        let requests = server.requests();
        assert_eq!(requests[0].path, "/api/v2/analysis/jres?os=linux&arch=x86_64");
        assert_eq!(requests[0].header("authorization"), Some("Bearer s3cr3t"));
        assert_eq!(requests[1].path, "/api/v2/analysis/jres/jre-1", "the API serves the archive when there is no CDN");
        assert_eq!(requests[1].header("accept"), Some("application/octet-stream"));
    }

    /// The id is a path segment whose value the server owns. One carrying a `/` or a `?` must not be
    /// able to send the download somewhere else.
    #[test]
    fn encodes_the_jre_id_in_the_download_url() {
        let dir = tempdir();
        let home = tempdir();
        let (archive, checksum) = jre_archive(&dir);
        let metadata = format!(
            r#"[{{"id":"jre/1?os=win","filename":"jre.tar.gz","sha256":"{checksum}","javaPath":"jre/bin/java",
                 "os":"linux","arch":"x86_64"}}]"#
        );
        let expected = "/api/v2/analysis/jres/jre%2F1%3Fos%3Dwin";
        let server = TestServer::start(move |request| {
            if request.path.starts_with("/api/v2/analysis/jres?") {
                Response::json(&metadata)
            } else if request.path == expected {
                Response::bytes(&archive)
            } else {
                Response::status(404).with_body(request.path.as_bytes())
            }
        });

        resolve_against(&server, &home, &[], &[]).unwrap();

        assert_eq!(server.requests()[1].path, expected);
    }

    #[test]
    fn reuses_a_provisioned_jre() {
        let dir = tempdir();
        let home = tempdir();
        let (archive, checksum) = jre_archive(&dir);
        let server = jre_server(archive, checksum);
        let first = resolve_against(&server, &home, &[], &[]).unwrap();

        let second = resolve_against(&server, &home, &[], &[]).unwrap();

        assert_eq!(second, Jre { java_exe: first.java_exe, cache_hit: CacheHit::Hit });
        let downloads = server.requests().iter().filter(|request| request.path.contains("jres/jre-1")).count();
        assert_eq!(downloads, 1, "the archive is downloaded once; only the metadata is asked for again");
    }

    /// The property that matters about a CDN download: the token stays with the server it belongs to.
    #[test]
    fn downloads_from_a_cdn_without_the_token() {
        let dir = tempdir();
        let home = tempdir();
        let (archive, checksum) = jre_archive(&dir);
        let cdn = TestServer::start(move |_| Response::bytes(&archive));
        let cdn_url = cdn.url("/download/jre.tar.gz");
        let server = TestServer::start(move |_| Response::json(&metadata_json(&checksum, Some(&cdn_url))));

        let jre = resolve_against(&server, &home, &[], &[]).unwrap();

        assert!(jre.java_exe.is_file());
        let download = cdn.last_request();
        assert_eq!(download.path, "/download/jre.tar.gz");
        assert_eq!(download.header("authorization"), None, "the CDN is a foreign origin");
    }

    /// The JRE may be republished between the metadata call and the download, which invalidates the
    /// checksum we were comparing against, so the retry starts from the metadata again.
    #[test]
    fn asks_again_when_the_download_does_not_match_the_checksum() {
        let dir = tempdir();
        let home = tempdir();
        let (archive, checksum) = jre_archive(&dir);
        let calls = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&calls);
        let server = TestServer::start(move |request| {
            if request.path.starts_with("/api/v2/analysis/jres?") {
                let stale = counter.fetch_add(1, Ordering::Relaxed) == 0;
                let reported = if stale { "0".repeat(64) } else { checksum.clone() };
                Response::json(&metadata_json(&reported, None))
            } else {
                Response::bytes(&archive)
            }
        });

        let jre = resolve_against(&server, &home, &[], &[]).unwrap();

        assert!(jre.java_exe.is_file());
        assert_eq!(calls.load(Ordering::Relaxed), 2, "the metadata is read again, not just the download retried");
    }

    #[test]
    fn gives_up_when_the_download_never_matches_the_checksum() {
        let dir = tempdir();
        let home = tempdir();
        let (archive, _) = jre_archive(&dir);
        let server = TestServer::start(move |request| {
            if request.path.starts_with("/api/v2/analysis/jres?") {
                Response::json(&metadata_json(&"0".repeat(64), None))
            } else {
                Response::bytes(&archive)
            }
        });

        let failure = resolve_against(&server, &home, &[], &[]).unwrap_err();

        assert!(failure.to_string().contains("has checksum"), "{failure}");
        let metadata_calls = server.requests().iter().filter(|request| request.path.contains("jres?")).count();
        assert_eq!(metadata_calls, 2, "exactly one retry");
    }

    #[test]
    fn refuses_a_java_path_that_leaves_the_archive() {
        let dir = tempdir();
        let home = tempdir();
        let (_, checksum) = jre_archive(&dir);
        let server = TestServer::start(move |_| {
            Response::json(&format!(
                r#"[{{"id":"jre-1","filename":"jre.tar.gz","sha256":"{checksum}","javaPath":"../../../bin/sh"}}]"#
            ))
        });

        let failure = resolve_against(&server, &home, &[], &[]).unwrap_err();

        assert_eq!(
            failure.to_string(),
            "The server reported \"../../../bin/sh\" as the Java executable of jre.tar.gz, which is not a path inside it."
        );
    }

    #[test]
    fn reports_a_jre_archive_without_the_java_executable() {
        let dir = tempdir();
        let home = tempdir();
        let path = tar_gz(dir.path(), "jre.tar.gz", &[("jre/release", "JAVA=17\n", 0o644)]);
        let archive = std::fs::read(&path).unwrap();
        let server = jre_server(archive, checksum_of(&path));

        let failure = resolve_against(&server, &home, &[], &[]).unwrap_err();

        assert!(failure.to_string().starts_with("The JRE provisioned from jre.tar.gz has no Java executable at "));
    }

    #[test]
    fn falls_back_to_java_home_when_provisioning_is_disabled() {
        let dir = tempdir();
        let home = tempdir();
        let java_home = fake_java_home(&dir);
        let server = TestServer::always(Response::status(500));

        let jre = resolve_against(
            &server,
            &home,
            &[(SKIP_JRE_PROVISIONING, "true")],
            &[("JAVA_HOME", java_home.to_str().unwrap())],
        )
        .unwrap();

        assert_eq!(jre, Jre { java_exe: java_home.join("bin").join(JAVA_BINARY), cache_hit: CacheHit::Disabled });
        assert!(server.requests().is_empty(), "nothing is provisioned, so the server is not asked");
    }

    #[test]
    fn falls_back_when_the_server_offers_no_jre_for_the_platform() {
        let dir = tempdir();
        let home = tempdir();
        let java_home = fake_java_home(&dir);
        let server = TestServer::always(Response::json("[]"));

        let jre = resolve_against(&server, &home, &[], &[("JAVA_HOME", java_home.to_str().unwrap())]).unwrap();

        assert_eq!(jre, Jre { java_exe: java_home.join("bin").join(JAVA_BINARY), cache_hit: CacheHit::Disabled });
    }

    #[test]
    fn reports_that_there_is_no_java_at_all() {
        let home = tempdir();
        let server = TestServer::always(Response::json("[]"));

        let failure = resolve_against(&server, &home, &[], &[("PATH", "")]).unwrap_err();

        assert_eq!(
            failure.to_string(),
            "No Java runtime was found. Install Java 17 or later and set JAVA_HOME, or put java on the PATH, \
             or point -Dsonar.scanner.javaExePath=<path> at one, or let the scanner provision a JRE by \
             unsetting -Dsonar.scanner.skipJreProvisioning."
        );
    }

    #[test]
    fn falls_back_to_the_path_when_java_home_is_wrong() {
        let dir = tempdir();
        let home = tempdir();
        let java_home = fake_java_home(&dir);
        let server = TestServer::always(Response::json("[]"));
        let path = format!("{}{}{}", "/nowhere", PATH_SEPARATOR, java_home.join("bin").display());

        let jre = resolve_against(
            &server,
            &home,
            &[],
            &[("JAVA_HOME", dir.path().join("not-a-jdk").to_str().unwrap()), ("PATH", &path)],
        )
        .unwrap();

        assert_eq!(jre.java_exe, java_home.join("bin").join(JAVA_BINARY));
    }

    #[test]
    fn never_takes_java_from_a_relative_path_entry() {
        let dir = tempdir();
        let java_home = fake_java_home(&dir);
        let relative = ["", ".", "bin"].join(PATH_SEPARATOR);

        assert_eq!(on_path(&relative), None, "a relative PATH entry cannot decide which java runs");
        let with_absolute = format!("{relative}{PATH_SEPARATOR}{}", java_home.join("bin").display());
        assert_eq!(on_path(&with_absolute), Some(java_home.join("bin").join(JAVA_BINARY)));
    }

    /// These three strings are the engine's vocabulary, not ours: it forwards them as telemetry.
    #[test]
    fn names_the_cache_outcome_the_way_the_engine_expects() {
        assert_eq!(WAS_JRE_CACHE_HIT, "sonar.scanner.wasJreCacheHit");
        assert_eq!(CacheHit::Hit.as_str(), "HIT");
        assert_eq!(CacheHit::Miss.as_str(), "MISS");
        assert_eq!(CacheHit::Disabled.as_str(), "DISABLED");
    }

    #[test]
    fn encodes_a_platform_that_is_not_a_plain_token() {
        assert_eq!(encoded("linux"), "linux");
        assert_eq!(encoded("x86_64"), "x86_64");
        assert_eq!(encoded("mac os"), "mac%20os");
        assert_eq!(encoded("a&b=c"), "a%26b%3Dc");
    }

    #[cfg(windows)]
    const PATH_SEPARATOR: &str = ";";
    #[cfg(not(windows))]
    const PATH_SEPARATOR: &str = ":";

    /// A directory that looks enough like a JDK: `bin/java` exists.
    fn fake_java_home(dir: &TempDir) -> PathBuf {
        let java_home = dir.path().join("jdk");
        std::fs::create_dir_all(java_home.join("bin")).unwrap();
        std::fs::write(java_home.join("bin").join(JAVA_BINARY), JAVA_SCRIPT).unwrap();
        java_home
    }
}
