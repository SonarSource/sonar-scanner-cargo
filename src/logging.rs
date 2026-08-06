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
//! Log emission in the `sonar-scanner-cli` format.
//!
//! Nothing in this crate writes to stdout or stderr directly: every line goes through [`emit`], so
//! that there is a single place to add secret redaction once there are secrets to redact.

use std::fmt::Display;
use std::io::Write;
use std::sync::atomic::{AtomicBool, Ordering};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    Debug,
    Info,
    Error,
}

impl Level {
    fn label(self) -> &'static str {
        match self {
            Level::Debug => "DEBUG",
            Level::Info => "INFO",
            Level::Error => "ERROR",
        }
    }
}

static VERBOSE: AtomicBool = AtomicBool::new(false);

pub fn set_verbose(verbose: bool) {
    VERBOSE.store(verbose, Ordering::Relaxed);
}

pub fn is_verbose() -> bool {
    VERBOSE.load(Ordering::Relaxed)
}

/// Emit one log line. `ERROR` goes to stderr, everything else to stdout, mirroring the CLI scanner.
pub fn emit(level: Level, message: impl Display) {
    if level == Level::Debug && !is_verbose() {
        return;
    }
    let line = format!("{}: {message}", level.label());
    if level == Level::Error {
        let mut out = std::io::stderr().lock();
        let _ = writeln!(out, "{line}");
    } else {
        let mut out = std::io::stdout().lock();
        let _ = writeln!(out, "{line}");
    }
}

macro_rules! log_debug {
    ($($arg:tt)*) => { $crate::logging::emit($crate::logging::Level::Debug, format_args!($($arg)*)) };
}
macro_rules! log_info {
    ($($arg:tt)*) => { $crate::logging::emit($crate::logging::Level::Info, format_args!($($arg)*)) };
}
macro_rules! log_error {
    ($($arg:tt)*) => { $crate::logging::emit($crate::logging::Level::Error, format_args!($($arg)*)) };
}

pub(crate) use {log_debug, log_error, log_info};
