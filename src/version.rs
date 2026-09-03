/*
 * SonarScanner for Cargo
 * Copyright (C) SonarSource Sàrl
 * mailto:info AT sonarsource DOT com
 *
 * This program is free software; you can redistribute it and/or
 * modify it under the terms of the GNU Lesser General Public
 * License version 3 as published by the Free Software Foundation.
 *
 * This program is distributed in the hope that it will be useful,
 * but WITHOUT ANY WARRANTY; without even the implied warranty of
 * MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the GNU
 * Lesser General Public License for more details.
 *
 * You should have received a copy of the GNU Lesser General Public License
 * along with this program; if not, write to the Free Software Foundation,
 * Inc., 51 Franklin Street, Fifth Floor, Boston, MA  02110-1301, USA.
 */
//! The server version check: the first HTTP call of an analysis, and the gate that decides whether
//! this bootstrapper can talk to the target at all.
//!
//! SonarQube Cloud is not versioned and is never asked. For SonarQube Server the version decides
//! whether the engine can be provisioned over the API: 10.6 introduced the endpoints this
//! bootstrapper uses, and there is no legacy path to fall back on, so an older server is refused
//! with a pointer at the scanner that still supports it.

use log::{debug, info};
use thiserror::Error;

use crate::config::Properties;
use crate::endpoint::Endpoint;
use crate::http::HttpClient;

/// Testing hook: the server version to assume, which skips the HTTP call altogether.
pub const SQ_VERSION: &str = "sonar.scanner.internal.sqVersion";

/// The first SonarQube Server version serving `/api/v2/analysis/*`.
pub const MINIMUM_SERVER_VERSION: &str = "10.6";

const VERSION_ENDPOINT: &str = "/analysis/version";
const LEGACY_VERSION_ENDPOINT: &str = "/api/server/version";

/// Longest string still worth treating as a version. A server reports at most `2025.1.0.112345`.
const MAX_VERSION_LENGTH: usize = 32;

/// How much of an unexpected response body to quote back: enough to recognise a login page or a
/// proxy error, not enough to fill the log with one.
const QUOTED_BODY_LENGTH: usize = 60;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum VersionError {
    #[error(
        "SonarQube Server {version} is not supported. This scanner requires {MINIMUM_SERVER_VERSION} or later. \
         To analyse with an older server, use the SonarScanner CLI: \
         https://docs.sonarsource.com/sonarqube-server/latest/analyzing-source-code/scanners/sonarscanner/"
    )]
    UnsupportedServer { version: String },

    #[error(
        "{url} answered with something that is not a server version: {body:?}. Check that this URL is a \
         SonarQube Server, and that nothing on the way to it — a proxy, a captive portal — is answering \
         in its place."
    )]
    NotAVersion { url: String, body: String },
}

/// The version of the target server, or `None` for SonarQube Cloud, which has none to report.
///
/// Fails when the server is too old, or when the version cannot be obtained at all — an
/// unauthenticated or unreachable server is reported here rather than at the next call, because this
/// is the first call and its error message is the one the user sees.
pub fn resolve(
    client: &HttpClient,
    properties: &Properties,
    endpoint: &Endpoint,
) -> crate::error::Result<Option<String>> {
    if endpoint.is_cloud {
        debug!("Skipping the server version check: {} is not versioned", endpoint.product());
        return Ok(None);
    }
    let version = match properties.get_non_blank(SQ_VERSION) {
        Some(simulated) => {
            debug!("Assuming server version {simulated} from '{SQ_VERSION}', no version call is made");
            simulated.to_string()
        }
        None => query(client, endpoint)?,
    };

    if !is_at_least(&version, MINIMUM_SERVER_VERSION) {
        return Err(VersionError::UnsupportedServer { version }.into());
    }
    info!("Communicating with {} {version}", endpoint.product());
    Ok(Some(version))
}

fn query(client: &HttpClient, endpoint: &Endpoint) -> crate::error::Result<String> {
    let url = format!("{}{VERSION_ENDPOINT}", endpoint.api_base_url);
    let failure = match client.get_string(&url) {
        // An answer that is not a version means whatever replied is not this endpoint, so the
        // fallback below has nothing to add: it would only ask the same wrong thing again.
        Ok(body) => {
            return version_in(&body).ok_or_else(|| VersionError::NotAVersion { url, body: quoted(&body) }.into());
        }
        Err(failure) => failure,
    };

    // Every server this bootstrapper supports serves the endpoint above, so a failure there is
    // meaningful. The legacy endpoint is called only to tell "too old to support" apart from
    // "misconfigured or unauthenticated", and its answer is not trusted for anything else.
    debug!("{failure}. Falling back to {LEGACY_VERSION_ENDPOINT} to find out whether the server is simply too old");
    let legacy_url = format!("{}{LEGACY_VERSION_ENDPOINT}", endpoint.host_url);
    match client.get_string(&legacy_url).map(|body| version_in(&body)) {
        Ok(Some(version)) if !is_at_least(&version, MINIMUM_SERVER_VERSION) => Ok(version),
        // A server new enough to serve the modern endpoint failed to, or the answer here is not a
        // version either: report why the call above failed, because that is the real problem, and it
        // would otherwise resurface later as a confusing error.
        _ => Err(failure.into()),
    }
}

/// The version a version endpoint reported, or `None` when the body is not one.
///
/// HTTP 200 is not proof that the server answered: a proxy, a captive portal or a login page will
/// happily return one, and taking that body for a version reports it back as an unsupported server —
/// pointing the user at their server when the problem is on the way to it.
fn version_in(body: &str) -> Option<String> {
    let version = body.trim();
    let plausible = (1..=MAX_VERSION_LENGTH).contains(&version.len())
        && version.starts_with(|first: char| first.is_ascii_digit())
        && version.chars().all(|character| character.is_ascii_alphanumeric() || ".-_+".contains(character));
    plausible.then(|| version.to_string())
}

/// A response body on one line and of bounded length, fit for an error message.
fn quoted(body: &str) -> String {
    let collapsed: Vec<&str> = body.split_whitespace().collect();
    let collapsed = collapsed.join(" ");
    match collapsed.char_indices().nth(QUOTED_BODY_LENGTH) {
        Some((end, _)) => format!("{}…", &collapsed[..end]),
        None => collapsed,
    }
}

/// Whether `version` is at least `target`, ignoring any `-qualifier` suffix.
///
/// Both are dotted numbers, of any length: a server reports `10.6`, `2025.1.0.112345` or
/// `25.5.0.107428`. A non-numeric component compares as 0, which is enough for versions of that
/// shape and keeps a surprising build suffix from failing an analysis.
fn is_at_least(version: &str, target: &str) -> bool {
    let version = version.trim();
    if !version.starts_with(|first: char| first.is_ascii_digit()) {
        return false;
    }
    let version = version.split_once('-').map_or(version, |(release, _qualifier)| release);
    components(version) >= components(target)
}

fn components(version: &str) -> Vec<u64> {
    version.split('.').map(|component| component.parse().unwrap_or_default()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::TOKEN;
    use crate::test_server::{Response, TestServer};

    fn endpoint_of(server: &TestServer) -> Endpoint {
        Endpoint {
            host_url: server.base_url(),
            api_base_url: format!("{}/api/v2", server.base_url()),
            is_cloud: false,
            region: String::new(),
        }
    }

    fn properties(pairs: &[(&str, &str)]) -> Properties {
        pairs.iter().map(|(key, value)| ((*key).to_string(), (*value).to_string())).collect()
    }

    /// Resolve against a server, with the token set so the authenticated call is exercised.
    fn resolve_against(server: &TestServer, pairs: &[(&str, &str)]) -> crate::error::Result<Option<String>> {
        let endpoint = endpoint_of(server);
        let properties = properties(&[&[(TOKEN, "s3cr3t")], pairs].concat());
        let client = HttpClient::new(&properties, &endpoint).unwrap();
        resolve(&client, &properties, &endpoint)
    }

    #[test]
    fn reads_the_version_from_the_analysis_endpoint() {
        let server = TestServer::always(Response::text("2025.1.0.112345\n"));

        let version = resolve_against(&server, &[]).unwrap();

        assert_eq!(version.as_deref(), Some("2025.1.0.112345"), "the body is trimmed");
        let request = server.last_request();
        assert_eq!(request.path, "/api/v2/analysis/version");
        assert_eq!(request.header("authorization"), Some("Bearer s3cr3t"));
    }

    #[test]
    fn does_not_ask_sonarqube_cloud_for_a_version() {
        let server = TestServer::always(Response::text("should not be called"));
        let endpoint = Endpoint {
            host_url: "https://sonarcloud.io".to_string(),
            api_base_url: server.base_url(),
            is_cloud: true,
            region: String::new(),
        };
        let client = HttpClient::new(&Properties::new(), &endpoint).unwrap();

        assert_eq!(resolve(&client, &Properties::new(), &endpoint).unwrap(), None);
        assert!(server.requests().is_empty(), "SonarQube Cloud has no version to check");
    }

    #[test]
    fn the_simulated_version_replaces_the_call() {
        let server = TestServer::always(Response::text("should not be called"));

        let version = resolve_against(&server, &[(SQ_VERSION, "2025.4")]).unwrap();

        assert_eq!(version.as_deref(), Some("2025.4"));
        assert!(server.requests().is_empty(), "the version hook exists to avoid the call");
    }

    #[test]
    fn a_simulated_version_is_gated_like_a_real_one() {
        let server = TestServer::always(Response::text("should not be called"));

        let error = resolve_against(&server, &[(SQ_VERSION, "9.9")]).unwrap_err();

        assert!(error.to_string().starts_with("SonarQube Server 9.9 is not supported."), "{error}");
    }

    #[test]
    fn falls_back_to_the_legacy_endpoint_to_recognise_an_old_server() {
        let server = TestServer::start(|request| match request.path.as_str() {
            LEGACY_VERSION_ENDPOINT => Response::text("9.9.4.87374"),
            // Before 10.6 the modern endpoint does not exist.
            _ => Response::status(404),
        });

        let error = resolve_against(&server, &[]).unwrap_err();

        assert_eq!(
            error.to_string(),
            format!(
                "SonarQube Server 9.9.4.87374 is not supported. This scanner requires 10.6 or later. To analyse \
                 with an older server, use the SonarScanner CLI: \
                 https://docs.sonarsource.com/sonarqube-server/latest/analyzing-source-code/scanners/sonarscanner/"
            )
        );
    }

    /// A modern server that fails the modern endpoint has a different problem, and saying "too old"
    /// would send the user looking in the wrong place.
    #[test]
    fn reports_the_original_failure_when_the_legacy_endpoint_reports_a_supported_version() {
        let server = TestServer::start(|request| match request.path.as_str() {
            LEGACY_VERSION_ENDPOINT => Response::text("2025.1.0.112345"),
            _ => Response::status(401),
        });

        let error = resolve_against(&server, &[]).unwrap_err();

        assert!(error.to_string().starts_with("Unable to authenticate on SonarQube Server."), "{error}");
    }

    #[test]
    fn reports_the_original_failure_when_neither_endpoint_answers() {
        let server = TestServer::always(Response::status(500));

        let error = resolve_against(&server, &[]).unwrap_err();

        assert!(error.to_string().contains("returned HTTP 500"), "{error}");
        assert!(error.to_string().contains(VERSION_ENDPOINT), "the modern endpoint is the one reported: {error}");
    }

    /// The captive-portal case: something answers 200 with a web page. Reporting that page as the
    /// server version would send the user to upgrade a server that never saw the request.
    #[test]
    fn reports_a_body_that_is_not_a_version() {
        let page = format!("<!DOCTYPE html>\n<html><body>{}</body></html>", "Sign in to the proxy. ".repeat(20));
        let server = TestServer::always(Response::text(&page));

        let error = resolve_against(&server, &[]).unwrap_err();

        let message = error.to_string();
        assert!(message.contains("answered with something that is not a server version"), "{message}");
        assert!(message.contains("<!DOCTYPE html> <html><body>Sign in"), "the body is quoted on one line: {message}");
        assert!(message.contains('…'), "a long body is cut short: {message}");
        assert!(!message.contains("is not supported"), "this is not an old server: {message}");
        assert!(message.len() < page.len(), "the whole page does not end up in the message: {message}");
    }

    /// Whatever answered the modern endpoint answers the legacy one too, so its body says no more
    /// about the server than the first one did.
    #[test]
    fn does_not_take_a_legacy_body_that_is_not_a_version_for_an_old_server() {
        let server = TestServer::start(|request| match request.path.as_str() {
            LEGACY_VERSION_ENDPOINT => Response::text("<html>Sign in</html>"),
            _ => Response::status(403),
        });

        let error = resolve_against(&server, &[]).unwrap_err();

        assert!(error.to_string().starts_with("You don't have permission"), "the modern call is reported: {error}");
    }

    #[test]
    fn recognises_a_body_shaped_like_a_version() {
        assert_eq!(version_in("10.6").as_deref(), Some("10.6"));
        assert_eq!(version_in(" 2025.1.0.112345\n").as_deref(), Some("2025.1.0.112345"));
        assert_eq!(version_in("10.7-SNAPSHOT").as_deref(), Some("10.7-SNAPSHOT"));
        // Too old to support is still a version: the caller decides what to do about it.
        assert_eq!(version_in("9.9.4.87374").as_deref(), Some("9.9.4.87374"));
        assert_eq!(version_in(""), None);
        assert_eq!(version_in("   "), None);
        assert_eq!(version_in("<html>error</html>"), None);
        assert_eq!(version_in("10.6 and some prose"), None, "a version is one word");
        assert_eq!(version_in(&"1".repeat(MAX_VERSION_LENGTH + 1)), None, "no version is that long");
    }

    #[test]
    fn accepts_the_minimum_version() {
        let server = TestServer::always(Response::text(MINIMUM_SERVER_VERSION));

        assert_eq!(resolve_against(&server, &[]).unwrap().as_deref(), Some("10.6"));
    }

    #[test]
    fn compares_versions_ignoring_the_qualifier() {
        assert!(is_at_least("10.6", "10.6"));
        assert!(is_at_least("10.6.0.92116", "10.6"));
        assert!(is_at_least("2025.1.0.112345", "10.6"));
        assert!(is_at_least("25.5.0.107428", "10.6"));
        assert!(is_at_least("10.7-SNAPSHOT", "10.6"));
        assert!(!is_at_least("10.5.1.90531", "10.6"));
        assert!(!is_at_least("9.9.4.87374", "10.6"));
        assert!(!is_at_least("10.6-SNAPSHOT", "10.6.1"));
        // Anything not starting with a digit is not a version we can reason about.
        assert!(!is_at_least("", "10.6"));
        assert!(!is_at_least("<html>error</html>", "10.6"));
    }
}
