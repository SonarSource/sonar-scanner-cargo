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
mod error;
mod logging;

use std::collections::BTreeMap;
use std::process::ExitCode;
use std::time::{SystemTime, UNIX_EPOCH};

use log::{debug, error, info};

use crate::cli::Cli;
use crate::config::Configuration;
use crate::error::{Result, ScannerError};

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
    let config = config::resolve(cli, &env, &cwd, start_time_ms)?;

    // `sonar.verbose` may have come from a file or the environment rather than `-v`.
    if config.properties.get_bool(config::VERBOSE) {
        logging::set_verbose(true);
    }
    log_resolved(&config);

    // Endpoint resolution, provisioning and the engine handoff are not implemented yet.
    Err(ScannerError::NotImplemented(
        "Running an analysis is not implemented yet: this build resolves the configuration only.".to_string(),
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
