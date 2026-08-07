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
//! `cargo sonar-scanner` — the SonarScanner bootstrapper for Cargo projects.

mod cli;
mod config;
mod dryrun;
mod endpoint;
mod error;
mod logging;
mod payload;
mod platform;

use std::collections::BTreeMap;
use std::path::Path;
use std::process::ExitCode;
use std::time::{SystemTime, UNIX_EPOCH};

use log::{debug, error, info};

use crate::cli::Cli;
use crate::config::Configuration;
use crate::error::{Result, ScannerError};
use crate::payload::ScannerPayload;

/// Exit code for a failed bootstrap, matching the other scanners.
const FAILURE: u8 = 1;

fn main() -> ExitCode {
    let start_time_ms = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis()).unwrap_or_default();
    let cli = Cli::parse_argv(std::env::args_os());
    logging::init(cli.verbose);

    match run(&cli, start_time_ms) {
        Ok(code) => code,
        Err(failure) => {
            error!("{failure}");
            let mut source = std::error::Error::source(&failure);
            while let Some(cause) = source {
                error!("Caused by: {cause}");
                source = cause.source();
            }
            error!("EXECUTION FAILURE");
            ExitCode::from(FAILURE)
        }
    }
}

fn run(cli: &Cli, start_time_ms: u128) -> Result<ExitCode> {
    info!("SonarScanner for Cargo {}", env!("CARGO_PKG_VERSION"));

    let env: BTreeMap<String, String> = std::env::vars().collect();
    let cwd = std::env::current_dir().map_err(ScannerError::CurrentDir)?;
    let mut config = config::resolve(cli, &env, &cwd, start_time_ms)?;

    // `sonar.verbose` may have come from a file or the environment rather than `-v`.
    if config.properties.get_bool(config::VERBOSE) {
        logging::set_verbose(true);
    }
    let endpoint = endpoint::resolve(&config.properties)?;
    let platform = platform::detect(&config.properties);
    // The engine expects a resolved `sonar.host.url`, including when the target is SonarQube Cloud.
    config.set_resolved(config::HOST_URL, &endpoint.host_url);
    config.set_resolved(config::API_BASE_URL, &endpoint.api_base_url);
    info!("Analysis target: {} at {}", endpoint.product(), endpoint.host_url);
    debug!("Detected platform: {}/{}", platform.os, platform.arch);
    info!("Base directory: {}", config.project_base_dir.display());

    log_resolved(&config);

    if cli.dry_run {
        dryrun::report(&config, &endpoint, &platform);
        return Ok(ExitCode::SUCCESS);
    }

    if let Some(path) = config.properties.get_non_blank(config::DUMP_TO_FILE) {
        return dump_to_file(&config, path).map(|()| ExitCode::SUCCESS);
    }

    // Provisioning and the engine handoff are not implemented yet.
    Err(ScannerError::NotImplemented(
        "Running an analysis is not implemented yet: this build resolves the configuration only. \
         Use --dry-run to inspect the resolved properties, or -Dsonar.scanner.internal.dumpToFile=<path> \
         to write the payload that would be sent to the scanner engine."
            .to_string(),
    ))
}

/// Report the whole resolution at DEBUG, so that `--verbose` answers "where did that value come
/// from?" without a server. Sensitive values are masked.
fn log_resolved(config: &Configuration) {
    debug!("Base directory: {}", config.project_base_dir.display());
    debug!("User home: {}", config.user_home.display());
    for path in &config.loaded_files {
        debug!("Loaded configuration from {}", path.display());
    }
    debug!("Resolved {} properties", config.properties.len());
    for (key, value) in config.properties.iter() {
        let shown = if config::is_sensitive(key) && !value.is_empty() { "******" } else { value };
        debug!("  {key}={shown} [{}]", config.origin_of(key).label());
    }
}

/// Testing hook: write the engine payload instead of executing an analysis.
///
/// The payload deliberately carries the real token, because that is what the engine receives, so
/// the file is created readable only by its owner rather than at the process umask.
fn dump_to_file(config: &Configuration, path: &str) -> Result<()> {
    let payload = ScannerPayload::from_properties(&config.properties);
    write_private(Path::new(path), payload.to_pretty_json().as_bytes())
        .map_err(|source| ScannerError::FileWrite { path: path.into(), source })?;
    info!("Scanner properties written to {path}");
    Ok(())
}

#[cfg(unix)]
fn write_private(path: &Path, contents: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

    let mut file = std::fs::OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).open(path)?;
    // `mode` only applies when the file is created, so tighten an existing one too.
    file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
    file.write_all(contents)
}

#[cfg(not(unix))]
fn write_private(path: &Path, contents: &[u8]) -> std::io::Result<()> {
    std::fs::write(path, contents)
}
