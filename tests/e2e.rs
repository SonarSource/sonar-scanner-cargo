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
//! Analysis against a live SonarQube Server. This is the only test that exercises the whole chain
//! against real components — a provisioned JRE, the real scanner engine, and the real Rust analyzer.
//!
//! Skipped unless `SONAR_HOST_URL` and `SONAR_TOKEN` are set, so a contributor without a server is
//! never blocked. `docs/end-to-end-testing.md` explains how to stand one up.
//!
//! Unlike `tests/cli.rs`, the environment is *inherited* rather than cleared: the engine is a real
//! subprocess that needs a working `PATH` and `HOME`, and the endpoint comes from `SONAR_HOST_URL`
//! and `SONAR_TOKEN` rather than the command line, so the token never reaches an argv.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

const BIN: &str = env!("CARGO_BIN_EXE_cargo-sonar-scanner");

/// Ceiling on each Web API call this test makes against the server. It does not govern the scanner
/// subprocess, which is spawned without a timeout — a first run downloads a JRE and the engine.
const API_TIMEOUT: Duration = Duration::from_secs(60);

/// How long to wait for the server to process an uploaded report.
const PROCESSING_TIMEOUT: Duration = Duration::from_secs(120);

struct Server {
    url: String,
    token: String,
}

/// The server to analyse against, or `None` when the environment does not name one.
fn configured_server() -> Option<Server> {
    let non_blank =
        |name: &str| std::env::var(name).ok().map(|value| value.trim().to_string()).filter(|value| !value.is_empty());
    Some(Server {
        url: non_blank("SONAR_HOST_URL")?.trim_end_matches('/').to_string(),
        token: non_blank("SONAR_TOKEN")?,
    })
}

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures").join(name)
}

fn get(server: &Server, path: &str) -> serde_json::Value {
    let url = format!("{}/{path}", server.url);
    let mut response = ureq::get(&url)
        .header("Authorization", &format!("Bearer {}", server.token))
        .config()
        .timeout_global(Some(API_TIMEOUT))
        .build()
        .call()
        .unwrap_or_else(|error| panic!("GET {url} failed: {error}"));
    let body = response.body_mut().read_to_string().expect("failed to read the response body");
    serde_json::from_str(&body).unwrap_or_else(|error| panic!("GET {url} returned no JSON: {error}\n{body}"))
}

/// Block until the server has processed the uploaded report.
///
/// `cargo sonar-scanner` returns as soon as the report is uploaded — the server analyses it
/// afterwards, on its own queue. Querying measures straight after a successful run therefore finds
/// nothing, which looks exactly like an analysis that indexed no files.
fn wait_for_processing(server: &Server, project_key: &str) {
    let deadline = Instant::now() + PROCESSING_TIMEOUT;
    loop {
        let activity = get(server, &format!("api/ce/component?component={project_key}"));
        let queued = activity["queue"].as_array().is_none_or(|queue| !queue.is_empty());
        let current = activity["current"]["status"].as_str().unwrap_or("");

        // `current` is the last *finished* task, which while anything is queued still describes the
        // previous run: on a re-run against a project whose last analysis failed, reading it too
        // early condemns a report that is merely pending. The submit call enqueues the task before
        // the scanner returns, so an empty queue here means our own report is the finished one.
        if !queued {
            assert_ne!(current, "FAILED", "the server failed to process the analysis report");
            if current == "SUCCESS" {
                return;
            }
        }
        assert!(
            Instant::now() < deadline,
            "the report was still unprocessed after {PROCESSING_TIMEOUT:?} (queued: {queued}, current: {current})"
        );
        std::thread::sleep(Duration::from_millis(500));
    }
}

/// A measure as an integer. Measures are absent rather than zero when nothing was analysed, which is
/// exactly the failure this test is looking for, so the caller gets `None` and asserts on it.
fn measure(server: &Server, project_key: &str, metric: &str) -> Option<i64> {
    let response = get(server, &format!("api/measures/component?component={project_key}&metricKeys={metric}"));
    response["component"]["measures"].as_array()?.iter().find(|entry| entry["metric"] == metric)?["value"]
        .as_str()?
        .parse()
        .ok()
}

#[test]
fn analysing_a_single_crate_lands_on_the_server() {
    let Some(server) = configured_server() else {
        eprintln!(
            "skipping: set SONAR_HOST_URL and SONAR_TOKEN to run this test \
             (see docs/end-to-end-testing.md)"
        );
        return;
    };
    // Overridable so that two runs against a shared server do not fight over one project.
    let project_key =
        std::env::var("SCANCARGO_E2E_PROJECT_KEY").unwrap_or_else(|_| "scancargo-e2e-single-crate".to_string());
    let directory = fixture("single-crate");

    let output = Command::new(BIN)
        .current_dir(&directory)
        .arg("sonar-scanner")
        .arg(format!("-Dsonar.projectKey={project_key}"))
        .arg("-Dsonar.projectName=SonarScanner for Cargo end-to-end fixture")
        .output()
        .expect("failed to run the scanner binary");
    let logs = format!("{}{}", String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr));

    assert_eq!(output.status.code(), Some(0), "the analysis failed\n{logs}");
    assert!(!logs.contains(&server.token), "the token was written to a log stream");

    wait_for_processing(&server, &project_key);

    // The project exists and Rust files were measured. `ncloc` comes from the Rust analyzer, so a
    // value here means the analyzer ran, not merely that the scanner uploaded something.
    let ncloc = measure(&server, &project_key, "ncloc");
    assert!(
        ncloc.is_some_and(|lines| lines > 0),
        "expected {project_key} to have a non-zero ncloc, got {ncloc:?} — \
         the analysis reported success but nothing was indexed\n{logs}"
    );

    // A rule fired. The fixture raises `rust:S1488` on purpose, which is active in the built-in
    // Sonar way profile; if this is the only failing assertion, check the profile the project was
    // analysed with before assuming the chain is broken.
    let issues = get(&server, &format!("api/issues/search?componentKeys={project_key}"));
    let total = issues["total"].as_i64().unwrap_or(0);
    assert!(total > 0, "expected at least one issue on {project_key}, got {total}\n{logs}");
}
