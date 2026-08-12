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

/// Stand-in for a token. Deliberately not shaped like a real SonarQube token: redaction keys off
/// the configured value, not its format, so a realistic-looking literal would buy nothing and only
/// trip secret scanners.
const FAKE_TOKEN: &str = "not-a-real-token-0123456789abcdef";

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
fn dry_run_reports_the_default_cloud_endpoint_without_network_access() {
    let dir = tempdir();
    let run = run(dir.path(), &[], &["sonar-scanner", "--dry-run"]);
    run.assert_success();
    assert!(run.stdout.contains("Product:        SonarQube Cloud"), "{}", run.stdout);
    assert!(run.stdout.contains("https://api.sonarcloud.io"), "{}", run.stdout);
    assert!(run.stdout.contains("sonar.scanner.app=cargo"), "{}", run.stdout);
    assert!(run.stdout.contains(&format!("sonar.projectBaseDir={}", dir.path().display())), "{}", run.stdout);
}

#[test]
fn dry_run_shows_the_full_precedence_chain() {
    let dir = tempdir();
    write(&dir.path().join("sonar-project.properties"), "sonar.projectKey=from-project-file\nsonar.sources=src\n");
    let run = run(
        dir.path(),
        &[
            ("SONAR_HOST_URL", "https://sq.example.com"),
            ("SONAR_SCANNER_JSON_PARAMS", r#"{"sonar.projectVersion":"1.2.3"}"#),
            ("SONAR_SCANNER_PROXY_PORT", "3128"),
        ],
        &["sonar-scanner", "--dry-run", "-Dsonar.projectKey=from-cli"],
    );
    run.assert_success();
    assert!(run.stdout.contains("Product:        SonarQube Server"), "{}", run.stdout);
    assert!(run.stdout.contains("https://sq.example.com/api/v2"), "{}", run.stdout);
    assert!(run.stdout.contains("sonar.projectKey=from-cli   [command line]"), "{}", run.stdout);
    assert!(run.stdout.contains("sonar.sources=src   [project properties file]"), "{}", run.stdout);
    assert!(run.stdout.contains("sonar.projectVersion=1.2.3   [JSON params]"), "{}", run.stdout);
    assert!(run.stdout.contains("sonar.scanner.proxyPort=3128   [environment]"), "{}", run.stdout);
}

#[test]
fn dry_run_reads_the_sonar_table_of_the_manifest() {
    let dir = tempdir();
    write(
        &dir.path().join("Cargo.toml"),
        r#"
[package]
name = "my-crate"
version = "0.1.0"

[package.metadata.sonar]
project-key = "my-org_my-crate"
host-url = "https://sq.example.com"
exclusions = ["target/**", "vendor/**"]

[package.metadata.sonar.scanner]
java-opts = "-Xmx1g"
"#,
    );
    let run = run(dir.path(), &[], &["sonar-scanner", "--dry-run"]);
    run.assert_success();
    assert!(run.stdout.contains("Product:        SonarQube Server"), "{}", run.stdout);
    assert!(run.stdout.contains("sonar.projectKey=my-org_my-crate   [Cargo.toml]"), "{}", run.stdout);
    assert!(run.stdout.contains("sonar.host.url=https://sq.example.com   [Cargo.toml]"), "{}", run.stdout);
    assert!(run.stdout.contains("sonar.exclusions=target/**,vendor/**   [Cargo.toml]"), "{}", run.stdout);
    assert!(run.stdout.contains("sonar.scanner.javaOpts=-Xmx1g   [Cargo.toml]"), "{}", run.stdout);
}

#[test]
fn a_credential_in_the_manifest_is_warned_about_but_not_printed() {
    let dir = tempdir();
    let secret = FAKE_TOKEN;
    write(&dir.path().join("Cargo.toml"), &format!("[package.metadata.sonar]\ntoken = \"{secret}\"\n"));
    let run = run(dir.path(), &[], &["sonar-scanner", "--dry-run"]);
    run.assert_success();
    assert!(run.stdout.contains("WARN:"), "{}", run.stdout);
    assert!(run.stdout.contains("Credentials do not belong in a manifest"), "{}", run.stdout);
    assert!(!run.all_output().contains(secret), "the token leaked:\n{}", run.all_output());
}

#[test]
fn the_token_never_reaches_a_log_stream() {
    let dir = tempdir();
    let secret = FAKE_TOKEN;
    let run = run(dir.path(), &[("SONAR_TOKEN", secret)], &["sonar-scanner", "--dry-run", "--verbose"]);
    run.assert_success();
    assert!(!run.all_output().contains(secret), "the token leaked:\n{}", run.all_output());
    assert!(run.stdout.contains("sonar.token=******"), "{}", run.stdout);
    assert!(run.stdout.contains("Token:          set"), "{}", run.stdout);
}

#[test]
fn verbose_enables_debug_logging() {
    let dir = tempdir();
    let quiet = run(dir.path(), &[], &["sonar-scanner", "--dry-run"]);
    assert!(!quiet.all_output().contains("DEBUG:"), "{}", quiet.all_output());

    let verbose = run(dir.path(), &[], &["sonar-scanner", "--dry-run", "-v"]);
    verbose.assert_success();
    assert!(verbose.stdout.contains("DEBUG:"), "{}", verbose.stdout);

    // `sonar.verbose` from a properties file has the same effect as `-v`.
    write(&dir.path().join("sonar-project.properties"), "sonar.verbose=true\n");
    let from_file = run(dir.path(), &[], &["sonar-scanner", "--dry-run"]);
    from_file.assert_success();
    assert!(from_file.stdout.contains("DEBUG:"), "{}", from_file.stdout);
}

#[test]
fn dump_to_file_writes_the_engine_payload() {
    let dir = tempdir();
    let dump = dir.path().join("payload.json");
    let run = run(
        dir.path(),
        &[("SONAR_TOKEN", FAKE_TOKEN)],
        &[
            "sonar-scanner",
            &format!("-Dsonar.scanner.internal.dumpToFile={}", dump.display()),
            "-Dsonar.projectKey=my-crate",
        ],
    );
    run.assert_success();

    let payload = std::fs::read_to_string(&dump).expect("the payload file was not written");
    assert!(payload.contains(r#""key": "sonar.scanner.app""#), "{payload}");
    assert!(payload.contains(r#""value": "cargo""#), "{payload}");
    assert!(payload.contains(r#""value": "my-crate""#), "{payload}");
    // The engine needs the real token, so the payload — unlike any log — carries it.
    assert!(payload.contains(FAKE_TOKEN), "{payload}");

    // Which is why the file must not be readable by anyone else on a shared host.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&dump).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "dump file mode is {mode:o}");
    }
}

#[test]
fn an_inconsistent_endpoint_fails_with_an_actionable_message() {
    let dir = tempdir();
    let run = run(
        dir.path(),
        &[("SONAR_HOST_URL", "https://sonarcloud.io"), ("SONAR_REGION", "us")],
        &["sonar-scanner", "--dry-run"],
    );
    run.assert_failure();
    assert!(run.stderr.contains("sonar.region"), "{}", run.stderr);
    assert!(run.stderr.contains("EXECUTION FAILURE"), "{}", run.stderr);
}

#[test]
fn an_unknown_region_is_rejected() {
    let dir = tempdir();
    let run = run(dir.path(), &[("SONAR_REGION", "eu")], &["sonar-scanner", "--dry-run"]);
    run.assert_failure();
    assert!(run.stderr.contains("Invalid region 'eu'"), "{}", run.stderr);
}

/// Malformed arguments are clap's to report, so this is a usage error (exit 2) rather than a
/// bootstrap failure (exit 1).
#[test]
fn a_malformed_define_is_rejected() {
    let dir = tempdir();
    let run = run(dir.path(), &[], &["sonar-scanner", "--dry-run", "-Dnot-a-pair"]);
    assert_eq!(run.status, 2, "{}", run.all_output());
    assert!(run.stderr.contains("invalid value 'not-a-pair'"), "{}", run.stderr);
    assert!(run.stderr.contains("expected key=value"), "{}", run.stderr);
}

/// An analysis starts by talking to the server, so an unreachable one is what a run without a
/// server reports. Port 1 is where nothing listens: no analysis leaves this test.
#[test]
fn running_an_analysis_contacts_the_server() {
    let dir = tempdir();
    let token = format!("-Dsonar.token={FAKE_TOKEN}");
    let run = run(dir.path(), &[], &["sonar-scanner", "-Dsonar.host.url=http://127.0.0.1:1", &token]);
    run.assert_failure();
    assert!(run.stderr.contains("http://127.0.0.1:1"), "{}", run.stderr);
    assert!(run.stderr.contains("EXECUTION FAILURE"), "{}", run.stderr);
}

#[test]
fn works_when_invoked_directly_rather_than_through_cargo() {
    let dir = tempdir();
    let direct = run(dir.path(), &[], &["--dry-run"]);
    let through_cargo = run(dir.path(), &[], &["sonar-scanner", "--dry-run"]);
    direct.assert_success();
    through_cargo.assert_success();

    // The bootstrap timestamp differs between runs; everything else must be identical.
    let strip_timestamp = |output: &str| {
        output.lines().filter(|line| !line.contains("sonar.scanner.bootstrapStartTime")).collect::<Vec<_>>().join("\n")
    };
    assert_eq!(strip_timestamp(&direct.stdout), strip_timestamp(&through_cargo.stdout));
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

fn write(path: &Path, contents: &str) {
    std::fs::write(path, contents).unwrap();
}
