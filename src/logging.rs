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
//! The scanner's [`log`] implementation, and the single point where secrets are redacted.
//!
//! Using the `log` facade rather than bespoke macros means records emitted by dependencies land in
//! the same stream, formatted the same way, and redacted by the same code. The five levels of the
//! facade are exactly the five the scanner engine emits on its NDJSON stdout, so re-emitting them
//! needs no mapping.
//!
//! Nothing in this crate writes to stdout or stderr directly: every line goes through the logger or
//! through [`print`], so a value registered with [`register_secrets`] cannot reach an output stream.

use std::fmt::Display;
use std::io::Write;
use std::sync::OnceLock;

use log::{Level, LevelFilter, Log, Metadata, Record};

/// Replacement for any registered secret.
pub const REDACTED: &str = "[HIDDEN]";

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
        let line = format!("{}: {}", record.level(), redact(&record.args().to_string()));
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

/// Values registered once, after the configuration is resolved, and only read afterwards — so a
/// write-once cell is enough and the read path needs no lock.
static SECRETS: OnceLock<Vec<String>> = OnceLock::new();

/// Shorter values are ignored: redacting them would mangle unrelated text without protecting
/// anything meaningful.
const MIN_SECRET_LEN: usize = 4;

/// Register the values that must never appear in an output stream.
///
/// Called once, from configuration resolution. Later calls are ignored, which is what keeps the
/// read path lock-free.
pub fn register_secrets(values: impl IntoIterator<Item = String>) {
    let secrets = values
        .into_iter()
        .map(|value| value.trim().to_string())
        .filter(|value| value.len() >= MIN_SECRET_LEN)
        .collect();
    let _ = SECRETS.set(secrets);
}

/// Replace every registered secret with [`REDACTED`].
pub fn redact(message: &str) -> String {
    match SECRETS.get() {
        Some(secrets) => redact_with(secrets, message),
        None => message.to_string(),
    }
}

fn redact_with(secrets: &[String], message: &str) -> String {
    let mut redacted = message.to_string();
    for secret in secrets {
        if redacted.contains(secret.as_str()) {
            redacted = redacted.replace(secret.as_str(), REDACTED);
        }
    }
    redacted
}

/// Write a line that is program output rather than a log record (the dry-run dump).
pub fn print(message: impl Display) {
    let mut out = std::io::stdout().lock();
    let _ = writeln!(out, "{}", redact(&message.to_string()));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redacts_every_occurrence_of_a_registered_secret() {
        let secrets = vec!["squ_deadbeefcafe".to_string()];
        let redacted = redact_with(&secrets, "token=squ_deadbeefcafe and again squ_deadbeefcafe");
        assert_eq!(redacted, format!("token={REDACTED} and again {REDACTED}"));
    }

    #[test]
    fn ignores_values_too_short_to_be_credentials() {
        assert!("us".len() < MIN_SECRET_LEN);
        assert_eq!(redact_with(&[], "sonar.region=us"), "sonar.region=us");
    }
}
