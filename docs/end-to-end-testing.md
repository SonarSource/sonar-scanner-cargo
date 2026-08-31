# End-to-end testing against a live SonarQube Server

`tests/e2e.rs` is the only test that exercises the whole chain against real components: the binary
resolves the endpoint, provisions a JRE and the scanner engine, hands over, and the real Rust
analyzer analyses a fixture. Everything else stops at the network boundary.

It is skipped unless `SONAR_HOST_URL` and `SONAR_TOKEN` are set, so nobody needs a server to run
`cargo test`.

## Stand up a disposable server

```bash
docker run -d --name sq-e2e -p 9111:9000 sonarqube:community
```

Wait for it, which takes a couple of minutes on a first run:

```bash
until [ "$(curl -s localhost:9111/api/system/status | jq -r .status)" = UP ]; do sleep 5; done
```

**Give Docker at least 4 GB.** SonarQube runs Elasticsearch, a web server and a compute engine in
one container. With less, one of them is OOM-killed during startup and the container exits — look for
`Process exited with exit value [Web Server]: 137` in `docker logs sq-e2e`. Reducing the heaps with
`SONAR_SEARCH_JAVAOPTS` and friends does not rescue a 2 GB limit; it only changes which process dies.

Community Build bundles the legacy `sonar-rust` analyzer, which runs Clippy: it shells out to
`cargo clippy` in the analysed project, so `cargo` has to be on `PATH`, and a run leaves a
`Cargo.lock` and a `target/` beside the scanner's own `.sonar/` working directory. All three are
gitignored under `tests/fixtures/`, so a run does not dirty the working tree.

Which analyzer serves the rules does not matter here. The scanner hands over to the engine and never
sees the analysis, so this harness is testing the same thing either way.

## Get a token

Log in at <http://localhost:9111> as `admin` / `admin`, change the password when prompted, then
create a user token under **My Account → Security**.

## Run

```bash
export SONAR_HOST_URL=http://localhost:9111
export SONAR_TOKEN=<the token>
cargo test --test e2e -- --nocapture
```

`--nocapture` is worth it: the test prints the scanner's own output, which is what you want when it
fails.

Set `SCANCARGO_E2E_PROJECT_KEY` to something unique when running against a shared server, so two
runs do not fight over one project. The default is `scancargo-e2e-single-crate`.

## What it asserts, and why

The scanner returns as soon as the report is uploaded — the server analyses it afterwards, on its
own queue — so the test waits for the Compute Engine task before querying. Without that wait a
perfectly good analysis looks like one that indexed nothing.

| Assertion | What a failure means |
| --- | --- |
| Exit status is 0 | The chain broke somewhere the scanner noticed — read the logs it printed. |
| The token appears in no log stream | Redaction regressed. |
| `ncloc` on the project is greater than zero | The analysis reported success but nothing was indexed. `ncloc` comes from the Rust analyzer, so a value here means the analyzer ran, not merely that something was uploaded. |
| At least one issue exists | Files were indexed but no rule fired. The `single-crate` fixture raises `rust:S1488` on purpose, so check the quality profile the project was analysed with before assuming the chain is broken. |

## Tear down

```bash
docker rm -f sq-e2e
```

## What this does not cover

Auto-configuration. This test supplies the project key explicitly, and the fixtures carry
`[package.metadata.sonar]` tables, so nothing here depends on the engine deriving anything from
`Cargo.toml`. Analysing a project with no configuration at all is SCANCARGO-6, and it needs the
Cargo build-system reader on the engine side.

Note also that auto-configuration is off by default on SonarQube Server, so a run that means to
exercise it needs `-Dsonar.buildsystem.autoconfig.disabled=false` — without it the run looks
identical to this one and proves nothing about derivation.

Derivation itself is asserted engine-side, per slice, against a test plugin rather than a real
server. This harness is for outcomes: issues on the right files, coverage ingested, no build output
indexed.
