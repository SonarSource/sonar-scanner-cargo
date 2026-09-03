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
//! `SONAR_SCANNER_JSON_PARAMS`, with `SONARQUBE_SCANNER_PARAMS` as the deprecated fallback.

use std::collections::BTreeMap;

use serde_json::Value;

use super::Properties;
use crate::error::{Result, ScannerError};
use log::warn;

const PRIMARY: &str = "SONAR_SCANNER_JSON_PARAMS";
const FALLBACK: &str = "SONARQUBE_SCANNER_PARAMS";

pub fn from_env(env: &BTreeMap<String, String>) -> Result<Properties> {
    let primary = env.get(PRIMARY).filter(|raw| !raw.trim().is_empty());
    let fallback = env.get(FALLBACK).filter(|raw| !raw.trim().is_empty());
    if primary.is_some() && fallback.is_some() {
        warn!("Both {PRIMARY} and {FALLBACK} are set. Only {PRIMARY} is used.");
    }
    match primary.map(|raw| (PRIMARY, raw)).or_else(|| fallback.map(|raw| (FALLBACK, raw))) {
        Some((var, raw)) => parse(var, raw),
        None => Ok(Properties::new()),
    }
}

/// Parse a JSON object of scalar values. Numbers and booleans are accepted and stringified, since
/// property values are always strings on the wire; nested objects and arrays are not.
fn parse(var: &str, raw: &str) -> Result<Properties> {
    let invalid = |message: String| ScannerError::InvalidJsonParams { var: var.to_string(), message };

    let parsed: Value = serde_json::from_str(raw).map_err(|e| invalid(e.to_string()))?;
    let object = parsed.as_object().ok_or_else(|| invalid("expected a JSON object".to_string()))?;

    let mut properties = Properties::new();
    for (key, value) in object {
        let value = match value {
            Value::String(value) => value.clone(),
            Value::Number(value) => value.to_string(),
            Value::Bool(value) => value.to_string(),
            other => {
                return Err(invalid(format!("property '{key}' has a {} value, expected a string", kind_of(other))));
            }
        };
        properties.set(key, value);
    }
    Ok(properties)
}

fn kind_of(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env_of(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
    }

    #[test]
    fn parses_a_flat_object() {
        let properties = from_env(&env_of(&[(
            PRIMARY,
            r#"{"sonar.projectKey":"k","sonar.verbose":true,"sonar.scanner.connectTimeout":5}"#,
        )]))
        .unwrap();
        assert_eq!(properties.get("sonar.projectKey"), Some("k"));
        assert_eq!(properties.get("sonar.verbose"), Some("true"));
        assert_eq!(properties.get("sonar.scanner.connectTimeout"), Some("5"));
    }

    #[test]
    fn falls_back_to_the_deprecated_variable() {
        let properties = from_env(&env_of(&[(FALLBACK, r#"{"sonar.projectKey":"k"}"#)])).unwrap();
        assert_eq!(properties.get("sonar.projectKey"), Some("k"));
    }

    #[test]
    fn prefers_the_primary_variable() {
        let properties = from_env(&env_of(&[
            (PRIMARY, r#"{"sonar.projectKey":"primary"}"#),
            (FALLBACK, r#"{"sonar.projectKey":"fallback"}"#),
        ]))
        .unwrap();
        assert_eq!(properties.get("sonar.projectKey"), Some("primary"));
    }

    #[test]
    fn is_empty_when_unset_or_blank() {
        assert!(from_env(&env_of(&[])).unwrap().is_empty());
        assert!(from_env(&env_of(&[(PRIMARY, "   ")])).unwrap().is_empty());
    }

    #[test]
    fn rejects_malformed_input() {
        assert!(from_env(&env_of(&[(PRIMARY, "not json")])).is_err());
        assert!(from_env(&env_of(&[(PRIMARY, "[1,2]")])).is_err());
        assert!(from_env(&env_of(&[(PRIMARY, r#"{"a":{"b":"c"}}"#)])).is_err());
    }
}
