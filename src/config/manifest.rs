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
//! In-manifest configuration: `[package.metadata.sonar]` and `[workspace.metadata.sonar]`.
//!
//! Cargo reserves the `metadata` tables for third-party tools and ignores their contents entirely,
//! which makes them the idiomatic project-level configuration file for a Cargo bootstrapper — the
//! same role `pom.xml` plays for Maven and `[tool.sonar]` in `pyproject.toml` plays for Python.
//!
//! Only this one reserved table is read. Nothing else in the manifest is interpreted: workspaces,
//! targets and inherited fields are the scanner engine's business.

use std::path::{Path, PathBuf};

use toml::{Table, Value};

use super::Properties;
use crate::error::{Result, ScannerError};
use log::{debug, warn};

pub const MANIFEST_FILE: &str = "Cargo.toml";

/// Keys whose real property name is not the camel-cased form of the bare key.
const ALIASES: &[(&str, &str)] =
    &[("host-url", "sonar.host.url"), ("project-base-dir", "sonar.projectBaseDir"), ("user-home", "sonar.userHome")];

/// Read `<base_dir>/Cargo.toml` and return the properties configured in it.
///
/// Returns `Ok(None)` when there is no manifest, or no `metadata.sonar` table in it.
pub fn load_if_present(base_dir: &Path) -> Result<Option<(PathBuf, Properties)>> {
    let path = base_dir.join(MANIFEST_FILE);
    let contents = match std::fs::read_to_string(&path) {
        Ok(contents) => contents,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            debug!("No {MANIFEST_FILE} at {}", path.display());
            return Ok(None);
        }
        Err(source) => return Err(ScannerError::FileRead { path, source }),
    };

    // A manifest we cannot parse is Cargo's problem to report, not a reason to fail the analysis.
    let manifest: Table = match contents.parse() {
        Ok(manifest) => manifest,
        Err(error) => {
            warn!("Ignoring {}: it is not valid TOML ({error})", path.display());
            return Ok(None);
        }
    };

    let mut properties = Properties::new();
    // A workspace root may also be a package; the more specific table wins.
    for section in ["workspace", "package"] {
        if let Some(table) = sonar_table(&manifest, section) {
            debug!("Reading [{section}.metadata.sonar] from {}", path.display());
            flatten(table, "sonar.", &mut properties)
                .map_err(|message| ScannerError::InvalidManifestConfig { path: path.clone(), message })?;
        }
    }
    if properties.is_empty() {
        return Ok(None);
    }

    // The manifest is committed, and for a library crate it is published inside the .crate file.
    for key in super::SENSITIVE_KEYS {
        if properties.contains(key) {
            warn!(
                "'{key}' is set in {}. Credentials do not belong in a manifest that is committed \
                 and published; use the SONAR_TOKEN environment variable instead.",
                path.display()
            );
        }
    }
    Ok(Some((path, properties)))
}

fn sonar_table<'a>(manifest: &'a Table, section: &str) -> Option<&'a Table> {
    manifest.get(section)?.as_table()?.get("metadata")?.as_table()?.get("sonar")?.as_table()
}

/// Flatten a TOML table into dotted property keys.
///
/// Nested tables become dotted segments and bare kebab-case keys become camelCase, so
/// `[package.metadata.sonar.scanner] java-opts = "-Xmx1g"` yields `sonar.scanner.javaOpts`. A key
/// that already starts with `sonar.` is taken verbatim, which is the escape hatch for any property
/// the naming convention cannot express.
fn flatten(table: &Table, prefix: &str, out: &mut Properties) -> std::result::Result<(), String> {
    for (key, value) in table {
        let full_key = resolve_key(key, prefix);
        match value {
            Value::Table(nested) => flatten(nested, &format!("{full_key}."), out)?,
            other => out.set(full_key.clone(), scalar(other, &full_key)?),
        }
    }
    Ok(())
}

fn resolve_key(key: &str, prefix: &str) -> String {
    if let Some(aliased) = ALIASES.iter().find(|(alias, _)| *alias == key).map(|(_, name)| *name)
        && prefix == "sonar."
    {
        return aliased.to_string();
    }
    if key.starts_with("sonar.") {
        return key.to_string();
    }
    // A quoted key already containing dots is a property path the user spelled out.
    if key.contains('.') { format!("{prefix}{key}") } else { format!("{prefix}{}", camel_case(key)) }
}

fn camel_case(key: &str) -> String {
    let mut camel = String::with_capacity(key.len());
    for (index, segment) in key.split('-').filter(|s| !s.is_empty()).enumerate() {
        if index == 0 {
            camel.push_str(segment);
            continue;
        }
        let mut chars = segment.chars();
        if let Some(first) = chars.next() {
            camel.extend(first.to_uppercase());
            camel.push_str(chars.as_str());
        }
    }
    camel
}

/// Property values are strings on the wire. Scalars are stringified and arrays are joined with
/// commas, which is how every Sonar property expresses a list.
fn scalar(value: &Value, key: &str) -> std::result::Result<String, String> {
    match value {
        Value::String(value) => Ok(value.clone()),
        Value::Integer(value) => Ok(value.to_string()),
        Value::Float(value) => Ok(value.to_string()),
        Value::Boolean(value) => Ok(value.to_string()),
        Value::Datetime(value) => Ok(value.to_string()),
        Value::Array(items) => {
            let joined: std::result::Result<Vec<String>, String> = items
                .iter()
                .map(|item| match item {
                    Value::Array(_) | Value::Table(_) => {
                        Err(format!("'{key}' contains a nested array or table, expected a list of scalars"))
                    }
                    scalar_item => scalar(scalar_item, key),
                })
                .collect();
            Ok(joined?.join(","))
        }
        Value::Table(_) => Err(format!("'{key}' is a table where a value was expected")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::tests::{tempdir, write};

    fn load(manifest: &str) -> Option<Properties> {
        let dir = tempdir();
        write(dir.path().join(MANIFEST_FILE), manifest);
        load_if_present(dir.path()).unwrap().map(|(_, properties)| properties)
    }

    #[test]
    fn reads_the_package_metadata_table() {
        let properties = load(
            r#"
            [package]
            name = "my-crate"

            [package.metadata.sonar]
            project-key = "my-org_my-crate"
            "#,
        )
        .unwrap();
        assert_eq!(properties.get("sonar.projectKey"), Some("my-org_my-crate"));
    }

    #[test]
    fn reads_the_workspace_metadata_table_of_a_virtual_manifest() {
        let properties = load(
            r#"
            [workspace]
            members = ["crates/*"]

            [workspace.metadata.sonar]
            project-key = "my-workspace"
            "#,
        )
        .unwrap();
        assert_eq!(properties.get("sonar.projectKey"), Some("my-workspace"));
    }

    #[test]
    fn the_package_table_overrides_the_workspace_table() {
        let properties = load(
            r#"
            [workspace.metadata.sonar]
            project-key = "from-workspace"
            organization = "shared"

            [package.metadata.sonar]
            project-key = "from-package"
            "#,
        )
        .unwrap();
        assert_eq!(properties.get("sonar.projectKey"), Some("from-package"));
        assert_eq!(properties.get("sonar.organization"), Some("shared"));
    }

    #[test]
    fn camel_cases_bare_keys_and_nests_tables() {
        let properties = load(
            r#"
            [package.metadata.sonar]
            project-version = "1.2.3"

            [package.metadata.sonar.scanner]
            java-opts = "-Xmx1g"
            skip-jre-provisioning = true
            "#,
        )
        .unwrap();
        assert_eq!(properties.get("sonar.projectVersion"), Some("1.2.3"));
        assert_eq!(properties.get("sonar.scanner.javaOpts"), Some("-Xmx1g"));
        assert_eq!(properties.get("sonar.scanner.skipJreProvisioning"), Some("true"));
    }

    #[test]
    fn maps_the_dotted_property_aliases() {
        let properties = load(
            r#"
            [package.metadata.sonar]
            host-url = "https://sq.example.com"
            user-home = "/tmp/sonar"
            "#,
        )
        .unwrap();
        assert_eq!(properties.get("sonar.host.url"), Some("https://sq.example.com"));
        assert_eq!(properties.get("sonar.userHome"), Some("/tmp/sonar"));
    }

    #[test]
    fn accepts_a_fully_qualified_property_name() {
        let properties = load(
            r#"
            [package.metadata.sonar]
            "sonar.cpd.exclusions" = "src/generated/**"
            "#,
        )
        .unwrap();
        assert_eq!(properties.get("sonar.cpd.exclusions"), Some("src/generated/**"));
    }

    #[test]
    fn joins_arrays_with_commas() {
        let properties = load(
            r#"
            [package.metadata.sonar]
            exclusions = ["target/**", "vendor/**"]
            "#,
        )
        .unwrap();
        assert_eq!(properties.get("sonar.exclusions"), Some("target/**,vendor/**"));
    }

    #[test]
    fn stringifies_scalars() {
        let properties = load(
            r#"
            [package.metadata.sonar]
            verbose = true

            [package.metadata.sonar.scanner]
            connect-timeout = 30
            "#,
        )
        .unwrap();
        assert_eq!(properties.get("sonar.verbose"), Some("true"));
        assert_eq!(properties.get("sonar.scanner.connectTimeout"), Some("30"));
    }

    #[test]
    fn is_absent_without_a_manifest_or_a_sonar_table() {
        let dir = tempdir();
        assert!(load_if_present(dir.path()).unwrap().is_none());
        assert!(load("[package]\nname = \"my-crate\"\n").is_none());
    }

    #[test]
    fn a_manifest_that_is_not_valid_toml_is_ignored() {
        assert!(load("this is not toml <<<").is_none());
    }

    #[test]
    fn rejects_a_value_that_cannot_become_a_property() {
        let dir = tempdir();
        write(dir.path().join(MANIFEST_FILE), "[package.metadata.sonar]\nexclusions = [[\"nested\"]]\n");
        let error = load_if_present(dir.path()).unwrap_err();
        assert!(error.to_string().contains("nested array or table"), "{error}");
    }

    #[test]
    fn camel_cases_kebab_keys() {
        assert_eq!(camel_case("project-key"), "projectKey");
        assert_eq!(camel_case("skip-jre-provisioning"), "skipJreProvisioning");
        assert_eq!(camel_case("organization"), "organization");
    }
}
