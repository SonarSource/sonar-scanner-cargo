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
//! Handing the analysis over to the scanner engine.
//!
//! ```text
//! <java> <sonar.scanner.javaOpts…> -jar <engine.jar>
//! ```
//!
//! The property document goes in on the child's standard input, its log records come back on its
//! standard output as newline-delimited JSON, and its exit code becomes ours. The environment is
//! inherited whole: the engine reads proxy settings and CI variables from it.
//!
//! The three streams are pumped **concurrently**, by three threads, and that is not an
//! implementation detail. A pipe holds only 64 KiB on Linux, so writing a larger property document
//! while nothing drains the child's output blocks this process in `write` and the engine in `write`,
//! each waiting for the other. Any project with a long inclusion list gets there, so the payload size
//! is not hypothetical.

use std::io::{BufRead, BufReader, Read, Write};
use std::path::Path;
use std::process::{Child, Command, ExitCode, ExitStatus, Stdio};

use log::{Level, debug, error, log};
use serde::Deserialize;
use thiserror::Error;

use crate::config::Properties;
use crate::payload::ScannerPayload;

/// Options for the JVM that runs the engine, e.g. `-Xmx1g`.
pub const JAVA_OPTS: &str = "sonar.scanner.javaOpts";

/// Exit code reported when the engine's own code cannot be passed through.
const FAILURE: u8 = 1;

/// A stdout line that is not a log event is reported at this level, per the bootstrapping guidelines.
const UNPARSEABLE_LEVEL: Level = Level::Info;

/// Log target of a record that came from the engine rather than from the bootstrapper.
///
/// The scanner's logger does not print targets, so this changes no output; it keeps the analysis's own
/// log apart from our diagnostics about running it, for anything that filters records — the tests
/// below above all.
const ENGINE: &str = "scanner-engine";

#[derive(Debug, Error)]
pub enum ProcessError {
    #[error("Failed to start the Java runtime {java_exe}: {source}")]
    Spawn {
        java_exe: String,
        #[source]
        source: std::io::Error,
    },

    #[error("Failed to wait for the scanner engine: {0}")]
    Wait(#[source] std::io::Error),

    #[error("The scanner engine was terminated abnormally ({status}).")]
    Terminated { status: String },
}

/// Run the engine to completion, returning the exit code the scanner should exit with.
pub fn run(java_exe: &Path, jar: &Path, properties: &Properties) -> crate::error::Result<ExitCode> {
    let payload = ScannerPayload::from_properties(properties).to_json();
    let options = java_opts(properties);

    let mut command = Command::new(java_exe);
    command.args(&options).arg("-jar").arg(jar);
    // No `env_clear`: the engine inherits this process's environment in full.
    command.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());

    debug!("Starting the scanner engine: {} {} -jar {}", java_exe.display(), options.join(" "), jar.display());
    let child =
        command.spawn().map_err(|source| ProcessError::Spawn { java_exe: java_exe.display().to_string(), source })?;

    let status = pump(child, payload)?;
    debug!("The scanner engine exited with {status}");
    code_of(status)
}

/// Feed the payload in and report everything that comes out, then wait for the child.
fn pump(mut child: Child, payload: String) -> crate::error::Result<ExitStatus> {
    let mut stdin = child.stdin.take().expect("stdin was piped");
    let stdout = child.stdout.take().expect("stdout was piped");
    let stderr = child.stderr.take().expect("stderr was piped");

    std::thread::scope(|scope| {
        scope.spawn(move || {
            // A write failure here is almost always an engine that exited before reading its input,
            // and its exit code says why far better than a broken pipe would. Reported at DEBUG so
            // that `--verbose` still shows it.
            if let Err(failure) = stdin.write_all(payload.as_bytes()).and_then(|()| stdin.flush()) {
                debug!("Failed to send the properties to the scanner engine: {failure}");
            }
            // Dropping stdin closes the pipe, which is what tells the engine the document is complete.
        });
        // Everything on stderr is an error, per the guidelines: the engine logs through its stdout
        // protocol, so a line on stderr is a JVM failure or a crash.
        scope.spawn(move || report(stderr, |line| error!(target: ENGINE, "{line}")));
        report(stdout, emit);
    });

    child.wait().map_err(|source| ProcessError::Wait(source).into())
}

/// Report every line of `stream` as it arrives, so a long analysis logs as it goes.
fn report(stream: impl Read, mut line_reported: impl FnMut(&str)) {
    // Not `lines()`: one line of invalid UTF-8 would end the loop and silence the rest of the
    // analysis. A stack trace from a JVM in a non-UTF-8 locale is exactly that.
    for line in BufReader::new(stream).split(b'\n') {
        match line {
            Ok(line) => {
                let line = String::from_utf8_lossy(&line);
                // Trailing whitespace covers the `\r` of a Windows line ending. A line with nothing
                // else on it carries nothing to report.
                let line = line.trim_end();
                if !line.is_empty() {
                    line_reported(line);
                }
            }
            Err(failure) => {
                debug!("Failed to read the output of the scanner engine: {failure}");
                return;
            }
        }
    }
}

/// One record of the engine's stdout protocol.
#[derive(Debug, Deserialize)]
struct LogEvent {
    message: Option<String>,
    level: Option<String>,
    stacktrace: Option<String>,
}

/// Re-emit one line of the engine's stdout as a log record of our own.
fn emit(line: &str) {
    match serde_json::from_str::<LogEvent>(line) {
        Ok(LogEvent { message: Some(message), level, stacktrace }) => {
            // An unknown or absent level is not worth losing the message over.
            let level = level.and_then(|level| level.trim().parse().ok()).unwrap_or(UNPARSEABLE_LEVEL);
            log!(target: ENGINE, level, "{message}");
            if let Some(stacktrace) = stacktrace.as_deref().map(str::trim).filter(|trace| !trace.is_empty()) {
                log!(target: ENGINE, level, "{stacktrace}");
            }
        }
        // Anything the engine did not write as a log event is still output. Logging it rather than
        // dropping it is the specified behaviour: it is where a JVM warning or a stray `println` ends
        // up, and those are exactly what one needs when an analysis misbehaves.
        _ => log!(target: ENGINE, UNPARSEABLE_LEVEL, "{line}"),
    }
}

/// The exit code to leave with. Zero is the engine's success, and every other code is passed through
/// so a CI job can tell a quality-gate failure from a crash.
fn code_of(status: ExitStatus) -> crate::error::Result<ExitCode> {
    match status.code() {
        Some(0) => Ok(ExitCode::SUCCESS),
        // A code outside a byte cannot be passed through; it is still a failure.
        Some(code) => Ok(ExitCode::from(u8::try_from(code).unwrap_or(FAILURE))),
        // On unix a signal leaves no exit code at all. Reported, rather than silently turned into a
        // success by a `unwrap_or(0)`.
        None => Err(ProcessError::Terminated { status: status.to_string() }.into()),
    }
}

/// Split `sonar.scanner.javaOpts` the way the other scanners do: on whitespace, with no shell quoting.
fn java_opts(properties: &Properties) -> Vec<String> {
    properties.get_non_blank(JAVA_OPTS).unwrap_or_default().split_whitespace().map(str::to_string).collect()
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::sync::{Mutex, OnceLock};

    use crate::config::TOKEN;

    /// A stand-in for the JVM, compiled by `rustc` at test time so that these tests drive a real
    /// child process on every platform rather than a shell script on some of them.
    ///
    /// It answers to the arguments between `argv[0]` and `-jar`, where the JVM reads its own options:
    ///
    /// * `stdout:<text>` — write `<text>` to stdout, with `\s` and `\n` decoded, see [`encoded`]
    /// * `stderr:<text>` — the same on stderr
    /// * `chatter:<count>` — write `<count>` log events to stdout, enough of them to fill a pipe
    /// * `echo-stdin` — read stdin to the end and report how many bytes arrived
    /// * `echo-env:<name>` — report an environment variable
    /// * `exit:<code>` — exit with `<code>` instead of 0
    /// * `abort` — end without exiting normally, which is a signal on unix
    ///
    /// Only `chatter` writes the engine's JSON protocol: the rest report in plain text, which the
    /// scanner logs at INFO, so that no assertion depends on hand-built JSON escaping.
    const FAKE_ENGINE: &str = r#"
        fn main() {
            let mut code = 0;
            for argument in std::env::args().skip(1).take_while(|argument| argument != "-jar") {
                let (command, value) = argument.split_once(':').unwrap_or((argument.as_str(), ""));
                // Java options are split on whitespace, so an instruction carries none: see `encoded`.
                let value = value.replace("\\s", " ").replace("\\n", "\n");
                match command {
                    "stdout" => println!("{value}"),
                    "stderr" => eprintln!("{value}"),
                    "chatter" => {
                        for index in 0..value.parse::<usize>().unwrap() {
                            println!("{{\"level\":\"INFO\",\"message\":\"working {index}\"}}");
                        }
                    }
                    "echo-stdin" => {
                        let mut input = String::new();
                        std::io::Read::read_to_string(&mut std::io::stdin(), &mut input).unwrap();
                        println!("stdin {} bytes", input.len());
                    }
                    "echo-env" => {
                        let found = std::env::var(&value).unwrap_or_else(|_| "unset".to_string());
                        println!("{value}={found}");
                    }
                    "exit" => code = value.parse().unwrap(),
                    "abort" => std::process::abort(),
                    other => panic!("unknown instruction {other}"),
                }
            }
            std::process::exit(code);
        }
    "#;

    /// The compiled fake engine, built once per test process.
    ///
    /// It lands under `target/`, so `cargo clean` removes it, and it is rebuilt on the first call of
    /// every test binary rather than trusted from an older run.
    pub(crate) fn fake_engine() -> &'static Path {
        static COMPILED: OnceLock<PathBuf> = OnceLock::new();
        COMPILED
            .get_or_init(|| {
                let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("target").join("test-fixtures");
                // Best-effort: a test run leaves a binary behind, and they are not worth accumulating.
                let _ = std::fs::remove_dir_all(&dir);
                std::fs::create_dir_all(&dir).unwrap();
                // The process id keeps two test binaries running at once out of each other's files.
                let id = std::process::id();
                let source = dir.join(format!("fake-engine-{id}.rs"));
                let binary = dir.join(format!("fake-engine-{id}{}", std::env::consts::EXE_SUFFIX));
                std::fs::write(&source, FAKE_ENGINE).unwrap();

                let rustc = Command::new("rustc")
                    .arg("--edition=2021")
                    .args(["-o".as_ref(), binary.as_os_str(), source.as_os_str()])
                    .output()
                    .expect("rustc is available: it just compiled this test");
                assert!(rustc.status.success(), "{}", String::from_utf8_lossy(&rustc.stderr));
                binary
            })
            .as_path()
    }

    /// Run the fake engine with the given instructions, collecting what the scanner logged.
    ///
    /// The instructions ride along as java options, because that is where the fake engine reads them
    /// and the JVM reads the real ones.
    fn run_fake(instructions: &[&str], pairs: &[(&str, &str)]) -> (crate::error::Result<ExitCode>, Vec<String>) {
        let mut properties: Properties =
            pairs.iter().map(|(key, value)| ((*key).to_string(), (*value).to_string())).collect();
        properties.set(JAVA_OPTS, instructions.join(" "));

        capturing(|| run(fake_engine(), Path::new("scanner-engine.jar"), &properties))
    }

    /// The engine's re-emitted log records, collected instead of printed.
    ///
    /// Only records targeted at [`ENGINE`] are kept: the test binary runs its tests in parallel, so
    /// everything else — including this module's own diagnostics — belongs to somebody else's test.
    static RECORDED: Mutex<Option<Vec<String>>> = Mutex::new(None);

    /// Serialises the tests that record, since the logger the facade allows is process-wide.
    static RECORDING: Mutex<()> = Mutex::new(());

    struct Recorder;

    static RECORDER: Recorder = Recorder;

    impl log::Log for Recorder {
        fn enabled(&self, _: &log::Metadata) -> bool {
            true
        }

        fn log(&self, record: &log::Record) {
            if record.target() != ENGINE {
                return;
            }
            if let Some(records) = RECORDED.lock().unwrap().as_mut() {
                records.push(format!("{}: {}", record.level(), record.args()));
            }
        }

        fn flush(&self) {}
    }

    fn capturing<T>(body: impl FnOnce() -> T) -> (T, Vec<String>) {
        // Poisoning can only mean an earlier test failed; the buffer is replaced below regardless.
        let _serialised = RECORDING.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        static INSTALLED: OnceLock<()> = OnceLock::new();
        INSTALLED.get_or_init(|| {
            log::set_logger(&RECORDER).expect("no other logger is installed in the test binary");
            log::set_max_level(log::LevelFilter::Trace);
        });

        *RECORDED.lock().unwrap() = Some(Vec::new());
        let outcome = body();
        let records = RECORDED.lock().unwrap().take().unwrap_or_default();
        (outcome, records)
    }

    /// Encode the text of an instruction, which travels as a java option and therefore cannot carry a
    /// space or a newline of its own.
    fn encoded(text: &str) -> String {
        text.replace(' ', "\\s").replace('\n', "\\n")
    }

    /// The number of bytes an `echo-stdin` line reports.
    fn bytes_received(record: &str) -> usize {
        let digits = record.strip_prefix("INFO: stdin ").and_then(|rest| rest.strip_suffix(" bytes"));
        digits.unwrap_or_else(|| panic!("not an echo-stdin record: {record}")).parse().unwrap()
    }

    #[test]
    fn re_emits_the_log_records_of_the_engine() {
        let events = [
            r#"{"level":"INFO","message":"Analysis starting"}"#,
            r#"{"level":"WARN","message":"Something to look at"}"#,
            r#"{"level":"ERROR","message":"It failed","stacktrace":"java.lang.RuntimeException"}"#,
            r#"{"level":"DEBUG","message":"Verbose detail"}"#,
        ]
        .join("\n");

        let (outcome, records) = run_fake(&[&format!("stdout:{}", encoded(&events))], &[]);

        assert!(outcome.is_ok());
        assert_eq!(
            records,
            [
                "INFO: Analysis starting",
                "WARN: Something to look at",
                "ERROR: It failed",
                "ERROR: java.lang.RuntimeException",
                "DEBUG: Verbose detail",
            ]
        );
    }

    /// Specified behaviour, not a fallback that hides a parsing bug: anything on stdout that is not a
    /// log event is still reported, at INFO.
    #[test]
    fn logs_unparseable_output_as_info() {
        let lines = ["Picked up JAVA_TOOL_OPTIONS: -Xmx2g", "{not json", r#"{"level":"WARN"}"#, "42"].join("\n");

        let (_, records) = run_fake(&[&format!("stdout:{}", encoded(&lines))], &[]);

        assert_eq!(
            records,
            [
                "INFO: Picked up JAVA_TOOL_OPTIONS: -Xmx2g",
                "INFO: {not json",
                // A log event with no message carries nothing to re-emit, so the line is passed
                // through as it is rather than logged as an empty record.
                r#"INFO: {"level":"WARN"}"#,
                "INFO: 42",
            ]
        );
    }

    #[test]
    fn logs_everything_on_stderr_as_an_error() {
        let message = encoded("Error occurred during initialization of VM");

        let (_, records) = run_fake(&[&format!("stderr:{message}")], &[]);

        assert_eq!(records, ["ERROR: Error occurred during initialization of VM"]);
    }

    #[test]
    fn passes_the_properties_on_standard_input() {
        let (outcome, records) = run_fake(&["echo-stdin"], &[(TOKEN, "s3cr3t")]);

        assert!(outcome.is_ok());
        let mut expected: Properties = [(TOKEN.to_string(), "s3cr3t".to_string())].into_iter().collect();
        expected.set(JAVA_OPTS, "echo-stdin");
        assert_eq!(bytes_received(&records[0]), ScannerPayload::from_properties(&expected).to_json().len());
    }

    /// The deadlock this module exists to avoid: a property document larger than a pipe buffer, sent
    /// to an engine that is producing output of its own. A single-threaded handoff hangs here.
    #[test]
    fn passes_a_payload_larger_than_a_pipe_buffer() {
        // A pipe holds 64 KiB on Linux, so both directions are comfortably past it: 360 KB of
        // properties going in, and 2000 log events — some 70 KB — coming back before stdin is read.
        let inclusions = "src/**/*.rs,".repeat(30_000);
        let chatter = 2_000;

        let (outcome, records) =
            run_fake(&[&format!("chatter:{chatter}"), "echo-stdin"], &[("sonar.inclusions", &inclusions)]);

        assert!(outcome.is_ok());
        assert!(bytes_received(records.last().unwrap()) > 360_000, "the whole document arrived: {:?}", records.last());
        assert_eq!(records.iter().filter(|record| record.starts_with("INFO: working ")).count(), chatter);
    }

    #[test]
    fn inherits_the_environment_of_the_scanner() {
        // A variable that is certainly set, and is not the scanner's to invent: the engine needs the
        // whole environment, not a curated copy.
        let path = std::env::var("PATH").unwrap();

        let (_, records) = run_fake(&["echo-env:PATH"], &[]);

        assert_eq!(records, [format!("INFO: PATH={path}")]);
    }

    #[test]
    fn propagates_the_exit_code_of_the_engine() {
        let (success, _) = run_fake(&["exit:0"], &[]);
        let (failure, _) = run_fake(&["exit:3"], &[]);

        assert_eq!(debug_of(success.unwrap()), debug_of(ExitCode::SUCCESS));
        assert_eq!(debug_of(failure.unwrap()), debug_of(ExitCode::from(3)));
    }

    #[test]
    fn reports_an_engine_that_did_not_exit_normally() {
        let (outcome, _) = run_fake(&["abort"], &[]);

        match outcome {
            // Unix reports a signal, which carries no exit code.
            Err(failure) => {
                assert!(failure.to_string().starts_with("The scanner engine was terminated abnormally"), "{failure}");
            }
            // Windows reports an abort as an ordinary, if unusual, exit code.
            Ok(code) => assert_ne!(debug_of(code), debug_of(ExitCode::SUCCESS)),
        }
    }

    #[test]
    fn reports_a_java_runtime_that_cannot_be_started() {
        let missing = Path::new("/nowhere/bin/java");

        let failure = run(missing, Path::new("engine.jar"), &Properties::new()).unwrap_err();

        assert!(failure.to_string().starts_with("Failed to start the Java runtime /nowhere/bin/java: "), "{failure}");
    }

    /// Output the engine produced but did not terminate with a newline is still reported, and neither
    /// a blank line nor a line that is not UTF-8 costs us the rest of the stream.
    #[test]
    fn reports_every_line_of_a_stream() {
        let stream: &[u8] = b"{\"level\":\"WARN\",\"message\":\"first\"}\n\n\xff\xfe not utf-8\nlast, unterminated";

        let (_, records) = capturing(|| report(stream, emit));

        assert_eq!(records, ["WARN: first", "INFO: \u{fffd}\u{fffd} not utf-8", "INFO: last, unterminated"]);
    }

    #[test]
    fn passes_the_configured_java_options_to_the_jvm() {
        let properties: Properties =
            [(JAVA_OPTS.to_string(), "  -Xmx1g   -Dfile.encoding=UTF-8 ".to_string())].into_iter().collect();

        assert_eq!(java_opts(&properties), ["-Xmx1g", "-Dfile.encoding=UTF-8"]);
        assert!(java_opts(&Properties::new()).is_empty(), "no options means no arguments before -jar");
    }

    fn debug_of(code: ExitCode) -> String {
        format!("{code:?}")
    }
}
