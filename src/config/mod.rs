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
//! Resolution of the Sonar property set.
//!
//! [`resolve`] is a pure function of (command line, environment, current directory) plus reads of
//! the configuration files, which makes the whole precedence contract testable without mocking.

pub mod env;
pub mod files;
pub mod json_params;

use std::collections::BTreeMap;
use std::collections::btree_map::Iter;
use std::path::{Path, PathBuf};

use crate::cli::Cli;
use crate::error::{Result, ScannerError};
use log::{debug, warn};

/// Properties set by the bootstrapper itself; a user value for these is ignored.
pub const APP: &str = "sonar.scanner.app";
pub const APP_VERSION: &str = "sonar.scanner.appVersion";
pub const BOOTSTRAP_START_TIME: &str = "sonar.scanner.bootstrapStartTime";

pub const HOST_URL: &str = "sonar.host.url";
pub const REGION: &str = "sonar.region";
pub const TOKEN: &str = "sonar.token";
pub const USER_HOME: &str = "sonar.userHome";
pub const PROJECT_BASE_DIR: &str = "sonar.projectBaseDir";
pub const VERBOSE: &str = "sonar.verbose";
pub const AUTOCONFIG_DISABLED: &str = "sonar.buildsystem.autoconfig.disabled";

/// This bootstrapper's identity, per the scanner naming convention (maven, gradle, cli, npm, …).
pub const SCANNER_APP: &str = "cargo";

/// Name of the project-level configuration file, looked up in `sonar.projectBaseDir`.
pub const PROJECT_PROPERTIES_FILE: &str = "sonar-project.properties";
/// Name of the user-level configuration file, looked up in `sonar.userHome`.
pub const USER_PROPERTIES_FILE: &str = "sonar-scanner.properties";

/// Property keys whose value must never be logged or dumped.
pub(crate) const SENSITIVE_KEYS: &[&str] = &[
    TOKEN,
    "sonar.login",
    "sonar.password",
    "sonar.scanner.proxyPassword",
    "sonar.scanner.truststorePassword",
    "sonar.scanner.keystorePassword",
];

pub fn is_sensitive(key: &str) -> bool {
    SENSITIVE_KEYS.contains(&key)
}

/// An ordered, case-sensitive property map.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Properties {
    map: BTreeMap<String, String>,
}

impl Properties {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn set(&mut self, key: impl Into<String>, value: impl Into<String>) {
        self.map.insert(key.into(), value.into());
    }

    pub fn get(&self, key: &str) -> Option<&str> {
        self.map.get(key).map(String::as_str)
    }

    /// The value of `key`, trimmed, unless it is absent or blank.
    pub fn get_non_blank(&self, key: &str) -> Option<&str> {
        self.get(key).map(str::trim).filter(|value| !value.is_empty())
    }

    pub fn get_bool(&self, key: &str) -> bool {
        self.get_non_blank(key).is_some_and(|value| value.eq_ignore_ascii_case("true"))
    }

    pub fn contains(&self, key: &str) -> bool {
        self.map.contains_key(key)
    }

    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }

    pub fn len(&self) -> usize {
        self.map.len()
    }

    pub fn iter(&self) -> Iter<'_, String, String> {
        self.map.iter()
    }

    /// Copy every entry of `other` over this one; `other` wins on conflict.
    pub fn merge(&mut self, other: &Properties) {
        for (key, value) in other.iter() {
            self.map.insert(key.clone(), value.clone());
        }
    }
}

impl<'a> IntoIterator for &'a Properties {
    type Item = (&'a String, &'a String);
    type IntoIter = Iter<'a, String, String>;

    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

impl FromIterator<(String, String)> for Properties {
    fn from_iter<I: IntoIterator<Item = (String, String)>>(iter: I) -> Self {
        Properties { map: iter.into_iter().collect() }
    }
}

/// Where every property came from, kept so `--dry-run` can explain the resolution.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    CommandLine,
    Environment,
    JsonParams,
    ProjectFile,
    UserFile,
    Bootstrapper,
}

impl Source {
    pub fn label(self) -> &'static str {
        match self {
            Source::CommandLine => "command line",
            Source::Environment => "environment",
            Source::JsonParams => "JSON params",
            Source::ProjectFile => "project properties file",
            Source::UserFile => "user properties file",
            Source::Bootstrapper => "bootstrapper",
        }
    }
}

/// The resolved configuration: the effective property set plus the provenance of each entry.
#[derive(Debug, Clone)]
pub struct Configuration {
    pub properties: Properties,
    pub origins: BTreeMap<String, Source>,
    pub user_home: PathBuf,
    pub project_base_dir: PathBuf,
    /// Configuration files that were actually read.
    pub loaded_files: Vec<PathBuf>,
}

impl Configuration {
    pub fn origin_of(&self, key: &str) -> Source {
        self.origins.get(key).copied().unwrap_or(Source::Bootstrapper)
    }
}

/// Resolve the effective property set.
///
/// `env` is the process environment, passed in rather than read so tests need no global state.
/// `cwd` is the directory the user ran the command from.
pub fn resolve(cli: &Cli, env: &BTreeMap<String, String>, cwd: &Path, start_time_ms: u128) -> Result<Configuration> {
    let command_line = cli.to_properties();
    let environment = env::from_env(env);
    let json_params = json_params::from_env(env)?;

    // The location of both configuration files is itself configurable, so resolve the higher
    // precedence layers first and use them to find the files.
    let mut layers: Vec<(Source, Properties)> = vec![
        (Source::JsonParams, json_params),
        (Source::Environment, environment),
        (Source::CommandLine, command_line),
    ];
    let preliminary = flatten(&layers);

    let project_base_dir = resolve_base_dir(&preliminary, cwd);
    let user_home = resolve_user_home(&preliminary, env, cwd)?;

    let mut loaded_files = Vec::new();
    let file_layers = load_file_layers(&project_base_dir, &user_home, &mut loaded_files)?;
    layers.splice(0..0, file_layers);

    let mut properties = flatten(&layers);
    let mut origins: BTreeMap<String, Source> = BTreeMap::new();
    for (source, layer) in &layers {
        for key in layer.iter().map(|(key, _)| key) {
            origins.insert(key.clone(), *source);
        }
    }

    apply_scanner_properties(&mut properties, &mut origins, start_time_ms);
    apply_defaults(&mut properties, &mut origins, &project_base_dir, &user_home);
    guard_credentials(&properties);

    Ok(Configuration { properties, origins, user_home, project_base_dir, loaded_files })
}

/// The project- and user-level configuration files, lowest precedence first.
fn load_file_layers(
    project_base_dir: &Path,
    user_home: &Path,
    loaded_files: &mut Vec<PathBuf>,
) -> Result<Vec<(Source, Properties)>> {
    let mut file_layers = Vec::new();
    let user_file = user_home.join(USER_PROPERTIES_FILE);
    let project_file = project_base_dir.join(PROJECT_PROPERTIES_FILE);

    for (source, path) in [(Source::UserFile, &user_file), (Source::ProjectFile, &project_file)] {
        if let Some(properties) = files::load_if_present(path)? {
            debug!("Loaded {} properties from {}", properties.len(), path.display());
            loaded_files.push(path.clone());
            file_layers.push((source, properties));
        } else {
            debug!("No configuration file at {}", path.display());
        }
    }
    Ok(file_layers)
}

/// Properties owned by the bootstrapper, which the user cannot override.
fn apply_scanner_properties(properties: &mut Properties, origins: &mut BTreeMap<String, Source>, start_time_ms: u128) {
    let owned = [
        (APP, SCANNER_APP.to_string()),
        (APP_VERSION, env!("CARGO_PKG_VERSION").to_string()),
        (BOOTSTRAP_START_TIME, start_time_ms.to_string()),
    ];
    for (key, value) in owned {
        if properties.get(key).is_some_and(|existing| existing != value) {
            warn!("Ignoring user-supplied value for {key}, it is set by the scanner.");
        }
        properties.set(key, value);
        origins.insert(key.to_string(), Source::Bootstrapper);
    }
}

/// Properties the bootstrapper contributes on the project's behalf, all user-overridable.
fn apply_defaults(
    properties: &mut Properties,
    origins: &mut BTreeMap<String, Source>,
    project_base_dir: &Path,
    user_home: &Path,
) {
    // Both directories are recorded in absolute form, whatever the user wrote. The origin stays
    // theirs when they supplied the key: the value is still theirs, only normalised.
    for (key, directory) in [(PROJECT_BASE_DIR, project_base_dir), (USER_HOME, user_home)] {
        if !properties.contains(key) {
            origins.insert(key.to_string(), Source::Bootstrapper);
        }
        properties.set(key, directory.display().to_string());
    }
    // Engine-side auto-configuration became opt-in in SCANENGINE-542, so its own default is `true`.
    // The bootstrapper turns it on for the user, because a Cargo project would otherwise derive
    // nothing and the whole point of this scanner would be lost. Still overridable.
    if !properties.contains(AUTOCONFIG_DISABLED) {
        properties.set(AUTOCONFIG_DISABLED, "false");
        origins.insert(AUTOCONFIG_DISABLED.to_string(), Source::Bootstrapper);
    }
}

/// Teach the logger every credential in the property set, and warn about deprecated ones.
fn guard_credentials(properties: &Properties) {
    crate::logging::register_secrets(
        SENSITIVE_KEYS.iter().filter_map(|key| properties.get_non_blank(key)).map(str::to_string),
    );
    if properties.get_non_blank(TOKEN).is_some()
        && (properties.get_non_blank("sonar.login").is_some() || properties.get_non_blank("sonar.password").is_some())
    {
        warn!(
            "Both '{TOKEN}' and the deprecated 'sonar.login'/'sonar.password' are set. \
             '{TOKEN}' takes precedence; remove the deprecated properties."
        );
    }
}

/// Merge layers lowest precedence first.
fn flatten(layers: &[(Source, Properties)]) -> Properties {
    let mut merged = Properties::new();
    for (_, layer) in layers {
        merged.merge(layer);
    }
    merged
}

/// The analysis base directory: an explicit `sonar.projectBaseDir`, else the current directory.
///
/// We deliberately do not walk up looking for a workspace root — that would mean reading
/// `Cargo.toml`, which is the engine's job. Running from inside a member crate analyses that member.
fn resolve_base_dir(properties: &Properties, cwd: &Path) -> PathBuf {
    match properties.get_non_blank(PROJECT_BASE_DIR) {
        Some(configured) => absolutize(Path::new(configured), cwd),
        None => cwd.to_path_buf(),
    }
}

/// The scanner home directory: `sonar.userHome` if set, otherwise `~/.sonar` as the guidelines
/// require. Always absolute, because it is written back into the property set and handed to the
/// engine, which must not have to guess what a relative path was relative to.
fn resolve_user_home(properties: &Properties, env: &BTreeMap<String, String>, cwd: &Path) -> Result<PathBuf> {
    if let Some(configured) = properties.get_non_blank(USER_HOME) {
        return Ok(absolutize(Path::new(configured), cwd));
    }
    // `HOME` from the passed-in environment first, so tests can control it.
    let home = env
        .get("HOME")
        .filter(|value| !value.trim().is_empty())
        .map(PathBuf::from)
        .or_else(dirs::home_dir)
        .ok_or(ScannerError::NoHomeDirectory)?;
    Ok(home.join(".sonar"))
}

fn absolutize(path: &Path, cwd: &Path) -> PathBuf {
    if path.is_absolute() { path.to_path_buf() } else { cwd.join(path) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::Cli;

    fn env_of(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
    }

    fn resolve_with(args: &[&str], env: &[(&str, &str)], cwd: &Path) -> Configuration {
        let mut argv = vec!["cargo-sonar-scanner"];
        argv.extend_from_slice(args);
        let cli = Cli::parse_argv(argv);
        let mut env = env_of(env);
        env.entry("HOME".to_string()).or_insert_with(|| cwd.display().to_string());
        resolve(&cli, &env, cwd, 1_700_000_000_000).expect("resolution failed")
    }

    #[test]
    fn command_line_beats_environment() {
        let dir = tempdir();
        let config = resolve_with(&["-Dsonar.token=from-cli"], &[("SONAR_TOKEN", "from-env")], dir.path());
        assert_eq!(config.properties.get(TOKEN), Some("from-cli"));
        assert_eq!(config.origin_of(TOKEN), Source::CommandLine);
    }

    #[test]
    fn environment_beats_json_params() {
        let dir = tempdir();
        let config = resolve_with(
            &[],
            &[("SONAR_TOKEN", "from-env"), ("SONAR_SCANNER_JSON_PARAMS", r#"{"sonar.token":"from-json"}"#)],
            dir.path(),
        );
        assert_eq!(config.properties.get(TOKEN), Some("from-env"));
        assert_eq!(config.origin_of(TOKEN), Source::Environment);
    }

    #[test]
    fn json_params_beat_the_project_file() {
        let dir = tempdir();
        write(dir.path().join(PROJECT_PROPERTIES_FILE), "sonar.projectKey=from-file\n");
        let config =
            resolve_with(&[], &[("SONAR_SCANNER_JSON_PARAMS", r#"{"sonar.projectKey":"from-json"}"#)], dir.path());
        assert_eq!(config.properties.get("sonar.projectKey"), Some("from-json"));
        assert_eq!(config.origin_of("sonar.projectKey"), Source::JsonParams);
    }

    #[test]
    fn the_project_file_beats_the_user_file() {
        let dir = tempdir();
        let user_home = dir.path().join("userhome");
        std::fs::create_dir_all(&user_home).unwrap();
        write(user_home.join(USER_PROPERTIES_FILE), "sonar.projectKey=from-user-file\n");
        write(dir.path().join(PROJECT_PROPERTIES_FILE), "sonar.projectKey=from-project-file\n");

        let config = resolve_with(&[], &[("SONAR_USER_HOME", &user_home.display().to_string())], dir.path());
        assert_eq!(config.properties.get("sonar.projectKey"), Some("from-project-file"));
        assert_eq!(config.origin_of("sonar.projectKey"), Source::ProjectFile);
    }

    #[test]
    fn the_user_file_is_read_when_nothing_else_defines_the_key() {
        let dir = tempdir();
        let user_home = dir.path().join("userhome");
        std::fs::create_dir_all(&user_home).unwrap();
        write(user_home.join(USER_PROPERTIES_FILE), "sonar.host.url=https://sq.example.com\n");

        let config = resolve_with(&[], &[("SONAR_USER_HOME", &user_home.display().to_string())], dir.path());
        assert_eq!(config.properties.get(HOST_URL), Some("https://sq.example.com"));
        assert_eq!(config.origin_of(HOST_URL), Source::UserFile);
    }

    #[test]
    fn the_project_file_is_read_from_an_overridden_base_dir() {
        let dir = tempdir();
        let base = dir.path().join("crates/inner");
        std::fs::create_dir_all(&base).unwrap();
        write(base.join(PROJECT_PROPERTIES_FILE), "sonar.projectKey=inner\n");

        let config = resolve_with(&["-Dsonar.projectBaseDir=crates/inner"], &[], dir.path());
        assert_eq!(config.properties.get("sonar.projectKey"), Some("inner"));
        assert_eq!(config.project_base_dir, base);
    }

    #[test]
    fn the_base_dir_defaults_to_the_current_directory() {
        let dir = tempdir();
        let config = resolve_with(&[], &[], dir.path());
        assert_eq!(config.project_base_dir, dir.path());
        assert_eq!(config.properties.get(PROJECT_BASE_DIR), Some(dir.path().display().to_string().as_str()));
    }

    #[test]
    fn a_relative_user_home_is_resolved_against_the_working_directory() {
        let dir = tempdir();
        let config = resolve_with(&["-Dsonar.userHome=./scanner-home"], &[], dir.path());
        assert_eq!(config.user_home, dir.path().join("./scanner-home"));
        assert!(Path::new(config.properties.get(USER_HOME).unwrap()).is_absolute());
    }

    #[test]
    fn the_user_home_defaults_to_dot_sonar_in_the_home_directory() {
        let dir = tempdir();
        let config = resolve_with(&[], &[("HOME", &dir.path().display().to_string())], dir.path());
        assert_eq!(config.user_home, dir.path().join(".sonar"));
    }

    #[test]
    fn the_scanner_identity_cannot_be_overridden() {
        let dir = tempdir();
        let config = resolve_with(&["-Dsonar.scanner.app=not-cargo"], &[], dir.path());
        assert_eq!(config.properties.get(APP), Some(SCANNER_APP));
        assert_eq!(config.properties.get(APP_VERSION), Some(env!("CARGO_PKG_VERSION")));
        assert_eq!(config.properties.get(BOOTSTRAP_START_TIME), Some("1700000000000"));
    }

    #[test]
    fn auto_configuration_is_enabled_by_default_and_overridable() {
        let dir = tempdir();
        let config = resolve_with(&[], &[], dir.path());
        assert_eq!(config.properties.get(AUTOCONFIG_DISABLED), Some("false"));

        let config = resolve_with(&["-Dsonar.buildsystem.autoconfig.disabled=true"], &[], dir.path());
        assert_eq!(config.properties.get(AUTOCONFIG_DISABLED), Some("true"));
    }

    // Minimal temp-dir helper: the crate has no dev-dependency on `tempfile` yet.
    pub(crate) struct TempDir(PathBuf);

    impl TempDir {
        pub(crate) fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    pub(crate) fn tempdir() -> TempDir {
        use std::sync::atomic::{AtomicU32, Ordering};
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let unique =
            format!("cargo-sonar-scanner-test-{}-{}", std::process::id(), COUNTER.fetch_add(1, Ordering::Relaxed));
        let path = std::env::temp_dir().join(unique);
        std::fs::create_dir_all(&path).unwrap();
        TempDir(path)
    }

    pub(crate) fn write(path: PathBuf, contents: &str) {
        std::fs::write(path, contents).unwrap();
    }
}
