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
//! Environment variables: the individually named ones, plus the systematic `SONAR_SCANNER_*` mapping.

use std::collections::BTreeMap;

use super::Properties;

/// Properties whose environment variable cannot be derived by [`systematic_key`].
///
/// Every entry follows the same schema as the systematic mapping — dots and camel-case boundaries
/// become `_`, and the name is upper-cased — and a test asserts it. The table exists because the
/// reverse direction is ambiguous: `SONAR_HOST_URL` could be `sonar.host.url` or `sonar.hostUrl`,
/// and only a list of known properties can tell them apart.
const NAMED: &[(&str, &str)] = &[
    ("SONAR_TOKEN", super::TOKEN),
    ("SONAR_HOST_URL", super::HOST_URL),
    ("SONAR_REGION", super::REGION),
    ("SONAR_USER_HOME", super::USER_HOME),
    ("SONAR_ORGANIZATION", "sonar.organization"),
    ("SONAR_LOGIN", "sonar.login"),
    ("SONAR_PASSWORD", "sonar.password"),
];

/// Consumed by [`super::json_params`], not by the systematic mapping.
const JSON_PARAM_VARS: &[&str] = &["SONAR_SCANNER_JSON_PARAMS", "SONARQUBE_SCANNER_PARAMS"];

const SCANNER_PREFIX: &str = "SONAR_SCANNER_";

/// Project the environment onto the Sonar property namespace.
pub fn from_env(env: &BTreeMap<String, String>) -> Properties {
    let mut properties = Properties::new();
    // Systematic mapping first: a named variable wins if both somehow map to the same key.
    for (name, value) in env {
        if JSON_PARAM_VARS.contains(&name.as_str()) {
            continue;
        }
        if let Some(key) = systematic_key(name) {
            properties.set(key, value);
        }
    }
    for (name, key) in NAMED {
        if let Some(value) = env.get(*name) {
            properties.set(*key, value);
        }
    }
    properties
}

/// `SONAR_SCANNER_XXX_YYY` → `sonar.scanner.xxxYyy`.
fn systematic_key(name: &str) -> Option<String> {
    let tail = name.strip_prefix(SCANNER_PREFIX)?;
    if tail.is_empty() {
        return None;
    }
    let mut key = String::from("sonar.scanner.");
    for (index, segment) in tail.split('_').filter(|s| !s.is_empty()).enumerate() {
        let lower = segment.to_ascii_lowercase();
        if index == 0 {
            key.push_str(&lower);
        } else {
            let mut chars = lower.chars();
            match chars.next() {
                Some(first) => {
                    key.extend(first.to_uppercase());
                    key.push_str(chars.as_str());
                }
                None => return None,
            }
        }
    }
    Some(key)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env_of(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
    }

    /// The documented property-to-variable schema: dots and camel-case boundaries become `_`, and
    /// the whole name is upper-cased. `sonar.scanner.proxyPort` becomes `SONAR_SCANNER_PROXY_PORT`.
    fn env_variable_name(property: &str) -> String {
        let mut name = String::with_capacity(property.len() + 4);
        for c in property.chars() {
            if c == '.' {
                name.push('_');
            } else if c.is_ascii_uppercase() {
                name.push('_');
                name.push(c);
            } else {
                name.push(c.to_ascii_uppercase());
            }
        }
        name
    }

    #[test]
    fn the_named_variables_follow_the_same_schema() {
        for (variable, property) in NAMED {
            assert_eq!(env_variable_name(property), *variable, "for {property}");
        }
    }

    #[test]
    fn the_systematic_mapping_inverts_the_same_schema() {
        for property in [
            "sonar.scanner.os",
            "sonar.scanner.proxyPort",
            "sonar.scanner.javaOpts",
            "sonar.scanner.skipJreProvisioning",
        ] {
            assert_eq!(systematic_key(&env_variable_name(property)).as_deref(), Some(property));
        }
    }

    #[test]
    fn maps_the_named_variables() {
        let properties = from_env(&env_of(&[
            ("SONAR_TOKEN", "t"),
            ("SONAR_HOST_URL", "https://sq.example.com"),
            ("SONAR_REGION", "us"),
            ("SONAR_USER_HOME", "/tmp/sonar"),
        ]));
        assert_eq!(properties.get("sonar.token"), Some("t"));
        assert_eq!(properties.get("sonar.host.url"), Some("https://sq.example.com"));
        assert_eq!(properties.get("sonar.region"), Some("us"));
        assert_eq!(properties.get("sonar.userHome"), Some("/tmp/sonar"));
    }

    #[test]
    fn camel_cases_the_systematic_mapping() {
        assert_eq!(systematic_key("SONAR_SCANNER_PROXY_PORT").as_deref(), Some("sonar.scanner.proxyPort"));
        assert_eq!(systematic_key("SONAR_SCANNER_OS").as_deref(), Some("sonar.scanner.os"));
        assert_eq!(systematic_key("SONAR_SCANNER_JAVA_OPTS").as_deref(), Some("sonar.scanner.javaOpts"));
        assert_eq!(
            systematic_key("SONAR_SCANNER_SKIP_JRE_PROVISIONING").as_deref(),
            Some("sonar.scanner.skipJreProvisioning")
        );
    }

    #[test]
    fn ignores_variables_outside_the_scanner_namespace() {
        assert_eq!(systematic_key("PATH"), None);
        assert_eq!(systematic_key("SONAR_TOKEN"), None);
        assert_eq!(systematic_key("SONAR_SCANNER_"), None);
        assert!(from_env(&env_of(&[("PATH", "/usr/bin")])).is_empty());
    }

    #[test]
    fn leaves_json_params_to_their_own_source() {
        let properties = from_env(&env_of(&[("SONAR_SCANNER_JSON_PARAMS", "{}")]));
        assert!(properties.is_empty());
    }
}
