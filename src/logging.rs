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
//! Nothing in this crate writes to stdout or stderr directly: every line goes through the logger,
//! so a value registered with [`register_secret`] cannot reach an output stream.

use std::io::Write;
use std::sync::{Mutex, OnceLock};

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

fn secrets() -> &'static Mutex<Vec<String>> {
    static SECRETS: OnceLock<Mutex<Vec<String>>> = OnceLock::new();
    SECRETS.get_or_init(|| Mutex::new(Vec::new()))
}

/// Register a value that must never appear in any output stream.
///
/// Very short values are ignored: redacting them would mangle unrelated text without protecting
/// anything meaningful.
pub fn register_secret(secret: &str) {
    let secret = secret.trim();
    if secret.len() < 4 {
        return;
    }
    let mut guard = secrets().lock().unwrap_or_else(|e| e.into_inner());
    if !guard.iter().any(|s| s == secret) {
        guard.push(secret.to_string());
    }
}

#[cfg(test)]
pub fn clear_secrets() {
    secrets().lock().unwrap_or_else(|e| e.into_inner()).clear();
}

/// Replace every registered secret with [`REDACTED`].
pub fn redact(message: &str) -> String {
    let guard = secrets().lock().unwrap_or_else(|e| e.into_inner());
    let mut redacted = message.to_string();
    for secret in guard.iter() {
        if redacted.contains(secret.as_str()) {
            redacted = redacted.replace(secret.as_str(), REDACTED);
        }
    }
    redacted
}

#[cfg(test)]
mod tests {
    use super::*;

    // The secret registry is process-global, so these tests must not run concurrently.
    static LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn redacts_every_occurrence_of_a_registered_secret() {
        let _guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());
        clear_secrets();
        register_secret("squ_deadbeefcafe");
        let redacted = redact("token=squ_deadbeefcafe and again squ_deadbeefcafe");
        assert_eq!(redacted, format!("token={REDACTED} and again {REDACTED}"));
        clear_secrets();
    }

    #[test]
    fn ignores_values_too_short_to_be_credentials() {
        let _guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());
        clear_secrets();
        register_secret("us");
        assert_eq!(redact("sonar.region=us"), "sonar.region=us");
        clear_secrets();
    }
}
