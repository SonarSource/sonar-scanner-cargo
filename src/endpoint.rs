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
//! Deciding which product we are talking to, and at which URLs.
//!
//! Mirrors `ScannerEndpointResolver` from the Java scanner library: inconsistent inputs are always
//! an error, never a guess.

use thiserror::Error;

use crate::config::{API_BASE_URL, HOST_URL, Properties, REGION};

const SONARCLOUD_URL: &str = "sonar.scanner.sonarcloudUrl";

/// The SonarQube Cloud regions, global first.
const CLOUD_REGIONS: &[CloudRegion] = &[
    CloudRegion { region: "", host_url: "https://sonarcloud.io", api_base_url: "https://api.sonarcloud.io" },
    CloudRegion { region: "us", host_url: "https://sonarqube.us", api_base_url: "https://api.sonarqube.us" },
];

struct CloudRegion {
    region: &'static str,
    host_url: &'static str,
    api_base_url: &'static str,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Endpoint {
    /// Value of `sonar.host.url` as handed to the engine — always set, including for Cloud.
    pub host_url: String,
    pub api_base_url: String,
    pub is_cloud: bool,
    /// Empty for SonarQube Server and for the global Cloud region.
    pub region: String,
}

impl Endpoint {
    /// How to name this endpoint in a user-facing message.
    pub fn product(&self) -> String {
        match (self.is_cloud, self.region.is_empty()) {
            (false, _) => "SonarQube Server".to_string(),
            (true, true) => "SonarQube Cloud".to_string(),
            (true, false) => format!("SonarQube Cloud [{}]", self.region),
        }
    }
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum EndpointError {
    #[error(
        "Invalid region '{0}'. Valid regions are: {valid}. \
         Please check the '{region}' property or the 'SONAR_REGION' environment variable.",
        valid = "'us'",
        region = REGION
    )]
    UnknownRegion(String),

    #[error(
        "Setting both '{REGION}' and '{HOST_URL}' is only supported when the host URL is the one of \
         the corresponding SonarQube Cloud region. Got region '{region}' and host URL '{host_url}'."
    )]
    RegionMismatch { region: String, host_url: String },

    #[error(
        "Defining a custom SonarQube Cloud URL without providing its API base URL is not supported. \
         Set '{API_BASE_URL}' alongside '{SONARCLOUD_URL}'."
    )]
    MissingCloudApiBaseUrl,

    #[error("Both '{SONARCLOUD_URL}' and '{HOST_URL}' are set but differ: '{cloud_url}' vs '{host_url}'.")]
    CloudUrlMismatch { cloud_url: String, host_url: String },
}

/// Resolve the endpoint from the effective property set.
pub fn resolve(properties: &Properties) -> Result<Endpoint, EndpointError> {
    let host_url = properties.get_non_blank(HOST_URL).map(strip_trailing_slashes);
    let cloud_url = properties.get_non_blank(SONARCLOUD_URL).map(strip_trailing_slashes);
    let api_base_url = properties.get_non_blank(API_BASE_URL).map(strip_trailing_slashes);
    let region = properties.get_non_blank(REGION).unwrap_or("").to_lowercase();

    // A custom Cloud URL is a testing/staging override; it short-circuits everything else. The
    // region is still validated, so that an override cannot smuggle in a region that does not exist.
    if let Some(cloud_url) = cloud_url {
        cloud_region(&region)?;
        if let Some(host_url) = host_url
            && !same_url(host_url, cloud_url)
        {
            return Err(EndpointError::CloudUrlMismatch {
                cloud_url: cloud_url.to_string(),
                host_url: host_url.to_string(),
            });
        }
        let api_base_url = api_base_url.ok_or(EndpointError::MissingCloudApiBaseUrl)?;
        return Ok(Endpoint {
            host_url: cloud_url.to_string(),
            api_base_url: api_base_url.to_string(),
            is_cloud: true,
            region,
        });
    }

    match host_url {
        // No host URL: SonarQube Cloud, in the requested region.
        None => {
            let cloud = cloud_region(&region)?;
            Ok(Endpoint {
                host_url: cloud.host_url.to_string(),
                api_base_url: api_base_url.unwrap_or(cloud.api_base_url).to_string(),
                is_cloud: true,
                region: cloud.region.to_string(),
            })
        }
        Some(host_url) => {
            // Reject an unknown region even when the host URL makes it irrelevant.
            cloud_region(&region)?;
            match CLOUD_REGIONS.iter().find(|cloud| same_url(host_url, cloud.host_url)) {
                // The host URL is a known Cloud URL: it must agree with any explicit region.
                Some(cloud) => {
                    if !region.is_empty() && region != cloud.region {
                        return Err(EndpointError::RegionMismatch { region, host_url: host_url.to_string() });
                    }
                    Ok(Endpoint {
                        host_url: cloud.host_url.to_string(),
                        api_base_url: api_base_url.unwrap_or(cloud.api_base_url).to_string(),
                        is_cloud: true,
                        region: cloud.region.to_string(),
                    })
                }
                // Anything else is a SonarQube Server, for which a region is meaningless.
                None => {
                    if !region.is_empty() {
                        return Err(EndpointError::RegionMismatch { region, host_url: host_url.to_string() });
                    }
                    Ok(Endpoint {
                        host_url: host_url.to_string(),
                        api_base_url: api_base_url.map(str::to_string).unwrap_or_else(|| format!("{host_url}/api/v2")),
                        is_cloud: false,
                        region: String::new(),
                    })
                }
            }
        }
    }
}

fn cloud_region(region: &str) -> Result<&'static CloudRegion, EndpointError> {
    CLOUD_REGIONS
        .iter()
        .find(|cloud| cloud.region == region)
        .ok_or_else(|| EndpointError::UnknownRegion(region.to_string()))
}

fn strip_trailing_slashes(url: &str) -> &str {
    url.trim().trim_end_matches('/')
}

/// Compare two URLs ignoring case, trailing slashes and a leading `www.` in the host.
fn same_url(left: &str, right: &str) -> bool {
    normalize(left) == normalize(right)
}

fn normalize(url: &str) -> String {
    let url = strip_trailing_slashes(url).to_lowercase();
    match url.split_once("://") {
        Some((scheme, rest)) => format!("{scheme}://{}", rest.strip_prefix("www.").unwrap_or(rest)),
        None => url,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn properties(pairs: &[(&str, &str)]) -> Properties {
        pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
    }

    fn resolved(pairs: &[(&str, &str)]) -> Endpoint {
        resolve(&properties(pairs)).expect("expected a resolvable endpoint")
    }

    #[test]
    fn defaults_to_global_sonarqube_cloud() {
        let endpoint = resolved(&[]);
        assert_eq!(endpoint.host_url, "https://sonarcloud.io");
        assert_eq!(endpoint.api_base_url, "https://api.sonarcloud.io");
        assert!(endpoint.is_cloud);
        assert_eq!(endpoint.region, "");
        assert_eq!(endpoint.product(), "SonarQube Cloud");
    }

    #[test]
    fn resolves_the_us_region() {
        for value in ["us", "US", " Us "] {
            let endpoint = resolved(&[(REGION, value)]);
            assert_eq!(endpoint.host_url, "https://sonarqube.us");
            assert_eq!(endpoint.api_base_url, "https://api.sonarqube.us");
            assert!(endpoint.is_cloud);
            assert_eq!(endpoint.product(), "SonarQube Cloud [us]");
        }
    }

    #[test]
    fn recognises_cloud_urls_written_by_hand() {
        for url in [
            "https://sonarcloud.io",
            "https://sonarcloud.io/",
            "https://www.sonarcloud.io",
            "https://WWW.SonarCloud.IO/",
        ] {
            let endpoint = resolved(&[(HOST_URL, url)]);
            assert!(endpoint.is_cloud, "{url} should resolve to Cloud");
            assert_eq!(endpoint.host_url, "https://sonarcloud.io");
            assert_eq!(endpoint.api_base_url, "https://api.sonarcloud.io");
        }
    }

    #[test]
    fn recognises_the_us_cloud_url() {
        let endpoint = resolved(&[(HOST_URL, "https://sonarqube.us/")]);
        assert!(endpoint.is_cloud);
        assert_eq!(endpoint.region, "us");
        assert_eq!(endpoint.api_base_url, "https://api.sonarqube.us");
    }

    #[test]
    fn a_consistent_region_and_cloud_url_are_accepted() {
        let endpoint = resolved(&[(HOST_URL, "https://sonarqube.us"), (REGION, "us")]);
        assert_eq!(endpoint.region, "us");
    }

    #[test]
    fn any_other_host_is_a_sonarqube_server() {
        let endpoint = resolved(&[(HOST_URL, "https://sq.example.com/")]);
        assert!(!endpoint.is_cloud);
        assert_eq!(endpoint.host_url, "https://sq.example.com");
        assert_eq!(endpoint.api_base_url, "https://sq.example.com/api/v2");
        assert_eq!(endpoint.product(), "SonarQube Server");
    }

    #[test]
    fn the_api_base_url_can_be_overridden() {
        let endpoint = resolved(&[(HOST_URL, "https://sq.example.com"), (API_BASE_URL, "https://api.example.com/")]);
        assert_eq!(endpoint.api_base_url, "https://api.example.com");

        let endpoint = resolved(&[(API_BASE_URL, "https://api.example.com")]);
        assert!(endpoint.is_cloud);
        assert_eq!(endpoint.api_base_url, "https://api.example.com");
    }

    #[test]
    fn rejects_a_region_combined_with_a_server_url() {
        let error = resolve(&properties(&[(HOST_URL, "https://sq.example.com"), (REGION, "us")])).unwrap_err();
        assert!(matches!(error, EndpointError::RegionMismatch { .. }));
    }

    #[test]
    fn rejects_a_region_that_contradicts_the_cloud_url() {
        let error = resolve(&properties(&[(HOST_URL, "https://sonarcloud.io"), (REGION, "us")])).unwrap_err();
        assert_eq!(
            error,
            EndpointError::RegionMismatch { region: "us".to_string(), host_url: "https://sonarcloud.io".to_string() }
        );
    }

    #[test]
    fn rejects_an_unknown_region() {
        let error = resolve(&properties(&[(REGION, "eu")])).unwrap_err();
        assert_eq!(error, EndpointError::UnknownRegion("eu".to_string()));
        assert!(error.to_string().contains("Valid regions are: 'us'"));

        assert!(resolve(&properties(&[(HOST_URL, "https://sq.example.com"), (REGION, "eu")])).is_err());
    }

    #[test]
    fn supports_a_custom_cloud_url_with_its_api_base_url() {
        let endpoint = resolved(&[
            (SONARCLOUD_URL, "https://staging.sonarcloud.io/"),
            (API_BASE_URL, "https://api.staging.sonarcloud.io"),
        ]);
        assert!(endpoint.is_cloud);
        assert_eq!(endpoint.host_url, "https://staging.sonarcloud.io");
        assert_eq!(endpoint.api_base_url, "https://api.staging.sonarcloud.io");
    }

    #[test]
    fn rejects_an_unknown_region_even_with_a_custom_cloud_url() {
        let error = resolve(&properties(&[
            (SONARCLOUD_URL, "https://staging.sonarcloud.io"),
            (API_BASE_URL, "https://api.staging.sonarcloud.io"),
            (REGION, "eu"),
        ]))
        .unwrap_err();
        assert_eq!(error, EndpointError::UnknownRegion("eu".to_string()));
    }

    #[test]
    fn keeps_a_known_region_with_a_custom_cloud_url() {
        let endpoint = resolved(&[
            (SONARCLOUD_URL, "https://staging.sonarqube.us"),
            (API_BASE_URL, "https://api.staging.sonarqube.us"),
            (REGION, "us"),
        ]);
        assert_eq!(endpoint.region, "us");
        assert_eq!(endpoint.product(), "SonarQube Cloud [us]");
    }

    #[test]
    fn rejects_a_custom_cloud_url_without_an_api_base_url() {
        let error = resolve(&properties(&[(SONARCLOUD_URL, "https://staging.sonarcloud.io")])).unwrap_err();
        assert_eq!(error, EndpointError::MissingCloudApiBaseUrl);
    }

    #[test]
    fn rejects_a_custom_cloud_url_contradicting_the_host_url() {
        let error = resolve(&properties(&[
            (SONARCLOUD_URL, "https://staging.sonarcloud.io"),
            (API_BASE_URL, "https://api.staging.sonarcloud.io"),
            (HOST_URL, "https://sq.example.com"),
        ]))
        .unwrap_err();
        assert!(matches!(error, EndpointError::CloudUrlMismatch { .. }));
    }

    #[test]
    fn a_blank_host_url_is_treated_as_unset() {
        assert_eq!(resolved(&[(HOST_URL, "   ")]).host_url, "https://sonarcloud.io");
    }
}
