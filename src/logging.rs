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
//! The scanner's [`log`] implementation.
//!
//! Using the `log` facade rather than bespoke macros means records emitted by dependencies land in
//! the same stream, formatted the same way, and — once secret redaction arrives — redacted by the
//! same code. The five levels of the facade are exactly the five the scanner engine emits on its
//! NDJSON stdout, so re-emitting them needs no mapping.

use std::io::Write;

use log::{Level, LevelFilter, Log, Metadata, Record};

static LOGGER: ScannerLogger = ScannerLogger;

struct ScannerLogger;

impl Log for ScannerLogger {
    fn enabled(&self, metadata: &Metadata) -> bool {
        metadata.level() <= log::max_level()
    }

    fn log(&self, record: &Record) {
        if !self.enabled(record.metadata()) {
            return;
        }
        let line = format!("{}: {}", record.level(), record.args());
        // ERROR goes to stderr, everything else to stdout.
        if record.level() == Level::Error {
            let mut out = std::io::stderr().lock();
            let _ = writeln!(out, "{line}");
        } else {
            let mut out = std::io::stdout().lock();
            let _ = writeln!(out, "{line}");
        }
    }

    fn flush(&self) {
        let _ = std::io::stdout().flush();
        let _ = std::io::stderr().flush();
    }
}

/// Install the scanner logger. Subsequent calls are ignored, as the facade allows only one logger.
pub fn init(verbose: bool) {
    let _ = log::set_logger(&LOGGER);
    set_verbose(verbose);
}

/// `sonar.verbose` selects DEBUG, per the bootstrapping guidelines. It can be set again once the
/// property has been resolved from a file or the environment rather than the command line.
pub fn set_verbose(verbose: bool) {
    log::set_max_level(if verbose { LevelFilter::Debug } else { LevelFilter::Info });
}
