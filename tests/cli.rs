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
//! End-to-end tests of the binary, with a fully controlled environment and working directory.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU32, Ordering};

const BIN: &str = env!("CARGO_BIN_EXE_cargo-sonar-scanner");

struct Run {
    stdout: String,
    stderr: String,
    status: i32,
}

impl Run {
    fn all_output(&self) -> String {
        format!("{}{}", self.stdout, self.stderr)
    }

    fn assert_success(&self) -> &Self {
        assert_eq!(self.status, 0, "expected success, got {}\n{}", self.status, self.all_output());
        self
    }

    fn assert_failure(&self) -> &Self {
        assert_eq!(self.status, 1, "expected failure, got {}\n{}", self.status, self.all_output());
        self
    }
}

/// Run the binary in `cwd` with exactly the given environment — nothing inherited.
fn run(cwd: &Path, env: &[(&str, &str)], args: &[&str]) -> Run {
    let mut command = Command::new(BIN);
    command.current_dir(cwd).env_clear().env("HOME", cwd).args(args);
    for (name, value) in env {
        command.env(name, value);
    }
    let Output { status, stdout, stderr } = command.output().expect("failed to run the scanner binary");
    Run {
        stdout: String::from_utf8_lossy(&stdout).into_owned(),
        stderr: String::from_utf8_lossy(&stderr).into_owned(),
        status: status.code().expect("the scanner was killed by a signal"),
    }
}

#[test]
fn help_renders_under_the_cargo_subcommand_name() {
    let dir = tempdir();
    let run = run(dir.path(), &[], &["sonar-scanner", "--help"]);
    run.assert_success();
    assert!(run.stdout.contains("cargo sonar-scanner"), "{}", run.stdout);
    assert!(run.stdout.contains("--dry-run"), "{}", run.stdout);
}

#[test]
fn version_is_the_crate_version() {
    let dir = tempdir();
    let run = run(dir.path(), &[], &["sonar-scanner", "--version"]);
    run.assert_success();
    assert!(run.stdout.contains(env!("CARGO_PKG_VERSION")), "{}", run.stdout);
}

#[test]
fn works_when_invoked_directly_rather_than_through_cargo() {
    let dir = tempdir();
    let direct = run(dir.path(), &[], &["--help"]);
    let through_cargo = run(dir.path(), &[], &["sonar-scanner", "--help"]);
    direct.assert_success();
    through_cargo.assert_success();
    assert_eq!(direct.stdout, through_cargo.stdout);
}

#[test]
fn verbose_enables_debug_logging() {
    let dir = tempdir();
    let quiet = run(dir.path(), &[], &["sonar-scanner"]);
    assert!(!quiet.all_output().contains("DEBUG:"), "{}", quiet.all_output());

    let verbose = run(dir.path(), &[], &["sonar-scanner", "-v"]);
    assert!(verbose.stdout.contains("DEBUG:"), "{}", verbose.stdout);
}

#[test]
fn a_malformed_define_is_rejected() {
    let dir = tempdir();
    let run = run(dir.path(), &[], &["sonar-scanner", "-Dnot-a-pair"]);
    run.assert_failure();
    assert!(run.stderr.contains("Invalid property definition 'not-a-pair'"), "{}", run.stderr);
    assert!(run.stderr.contains("EXECUTION FAILURE"), "{}", run.stderr);
}

#[test]
fn running_an_analysis_reports_that_it_is_not_implemented_yet() {
    let dir = tempdir();
    let run = run(dir.path(), &[], &["sonar-scanner"]);
    run.assert_failure();
    assert!(run.stderr.contains("not implemented yet"), "{}", run.stderr);
}

struct TempDir(PathBuf);

impl TempDir {
    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn tempdir() -> TempDir {
    static COUNTER: AtomicU32 = AtomicU32::new(0);
    let unique = format!("cargo-sonar-scanner-it-{}-{}", std::process::id(), COUNTER.fetch_add(1, Ordering::Relaxed));
    let path = std::env::temp_dir().join(unique);
    std::fs::create_dir_all(&path).unwrap();
    // The system temp directory is a symlink on macOS; resolve it so paths match what the scanner
    // reports for its working directory.
    TempDir(path.canonicalize().unwrap())
}
