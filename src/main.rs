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
mod error;
mod logging;

use std::process::ExitCode;
use std::time::{SystemTime, UNIX_EPOCH};

use log::{debug, error, info};

use crate::cli::Cli;
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
    debug!("Bootstrap start time: {start_time_ms}");

    for (key, _) in &cli.define {
        debug!("Property defined on the command line: {key}");
    }

    // Milestones M1 to M3 are not implemented yet.
    Err(ScannerError::NotImplemented(
        "Running an analysis is not implemented yet: this build only provides the command line \
         interface. Configuration resolution, provisioning and the scanner engine handoff are \
         still to come."
            .to_string(),
    ))
}
