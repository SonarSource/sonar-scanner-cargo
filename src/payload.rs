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
//! The JSON document handed to the scanner engine on its standard input.

use serde::Serialize;

use crate::config::Properties;

#[derive(Debug, Serialize)]
pub struct ScannerPayload {
    #[serde(rename = "scannerProperties")]
    pub scanner_properties: Vec<ScannerProperty>,
}

#[derive(Debug, Serialize)]
pub struct ScannerProperty {
    pub key: String,
    pub value: String,
}

impl ScannerPayload {
    pub fn from_properties(properties: &Properties) -> Self {
        ScannerPayload {
            scanner_properties: properties
                .iter()
                .map(|(key, value)| ScannerProperty { key: key.clone(), value: value.clone() })
                .collect(),
        }
    }

    /// Compact form: what the engine reads on stdin.
    #[cfg_attr(not(test), expect(dead_code, reason = "used by the engine handoff, milestone M3"))]
    pub fn to_json(&self) -> String {
        // Serialising a map of strings cannot fail.
        serde_json::to_string(self).expect("failed to serialise the scanner payload")
    }

    /// Indented form: what `sonar.scanner.internal.dumpToFile` writes.
    pub fn to_pretty_json(&self) -> String {
        serde_json::to_string_pretty(self).expect("failed to serialise the scanner payload")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serialises_the_documented_shape() {
        let properties: Properties = [("sonar.scanner.app".to_string(), "cargo".to_string())].into_iter().collect();
        let json = ScannerPayload::from_properties(&properties).to_json();
        assert_eq!(json, r#"{"scannerProperties":[{"key":"sonar.scanner.app","value":"cargo"}]}"#);
    }
}
