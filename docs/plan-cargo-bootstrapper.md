# Implementation plan — `cargo sonar-scanner` bootstrapper

**Repository:** `SonarSource/sonar-scanner-cargo` (scaffolded, empty — `ccbca73 Initial commit after scaffolding`)
**Language:** Rust · **Artifact:** crate `cargo-sonar-scanner` on crates.io + prebuilt binaries
**Companion plan:** [`plan-engine-cargo-autoconfig.md`](plan-engine-cargo-autoconfig.md) (repo `SonarSource/sonar-scanner-engine`)
**Design source:** [`cargo-sonar-scanner-design.md`](cargo-sonar-scanner-design.md)
**Normative specs:**
[Scanner Bootstrapping](https://xtranet-sonarsource.atlassian.net/wiki/spaces/CodeOrches/pages/3155001372) ·
[Scanner Bootstrappers implementation guidelines](https://xtranet-sonarsource.atlassian.net/wiki/spaces/CodeOrches/pages/3155001395)
**Release infrastructure:**
[PREQ-7880](https://sonarsource.atlassian.net/browse/PREQ-7880) crates.io technical account (Done) ·
[BUILD-12231](https://sonarsource.atlassian.net/browse/BUILD-12231) crates.io publish path in `gh-action_release` (Open) — see M5

> Read this first. The two Confluence pages above are the contract. Where this plan and they disagree, they
> win — but the tables below are transcribed from them so the work can be done offline.
>
> **Progress: M0 and M1 are done and merged.** §2 below records the state the repository started from, not the
> state it is in. See §6 for what each milestone delivered and what remains.

---

## 1. Goal and scope

```console
$ export SONAR_TOKEN=...
$ cargo sonar-scanner
```

One command, inside a Cargo project, no prerequisites beyond a Rust toolchain.

**The bootstrapper implements the generic bootstrapping contract and nothing else.** It never opens
`Cargo.toml`, does not know what a workspace or `target/` is, and has no Rust-specific configuration logic.
All project inspection happens in the scanner engine (companion plan). Its only project-level responsibility
is establishing `sonar.projectBaseDir`.

**Out of scope for v1:** running Clippy or coverage tooling; Cargo package-selection flags (`-p`, `--workspace`);
test-report format conversion; the legacy classloader bootstrap path (SonarQube Server < 10.6).

---

## 2. Repository starting state

Historical: this is what the scaffold looked like before M0. Kept because it explains why some things are the
way they are — the release workflow's `gh-action_release` wiring in particular predates this plan.

| Item | State |
|---|---|
| `README.md` | one line |
| `LICENSE` | present |
| `.github/CODEOWNERS` | `* @sonarsource/code-quality-ci-experience-squad` |
| `.github/workflows/build.yml` | scaffold with `# TODO: Add your build steps here`, runner `sonar-xs` |
| `.github/workflows/release.yml` | `SonarSource/gh-action_release` v7.4.0, `workflow_dispatch` with `version` |
| `.github/workflows/unified-dogfooding.yml`, `pr-cleanup.yml` | scaffold |

No Rust code, no `Cargo.toml`. Everything below is greenfield.

---

## 3. Cargo subcommand mechanics (get this right on day one)

- The binary **must** be named `cargo-sonar-scanner` and be on `PATH` for `cargo sonar-scanner` to resolve it.
- Cargo invokes it as `cargo-sonar-scanner sonar-scanner <user args…>` — **`argv[1]` is the subcommand name
  and must be stripped** when present. The binary must also work when invoked directly as
  `cargo-sonar-scanner <args…>`.
- Install path for users: `cargo install cargo-sonar-scanner`, plus prebuilt binaries for CI.
- `cargo sonar-scanner --help` must render sensibly (clap `bin_name = "cargo sonar-scanner"`).

The natural name `cargo-sonar` is taken on crates.io and clashes with the enterprise analyzer's internal
binary; `cargo-sonar-scanner` is free and unambiguous (design §Appendix).

---

## 4. Proposed crate layout

```
sonar-scanner-cargo/
├── Cargo.toml                 # package cargo-sonar-scanner, bin cargo-sonar-scanner
├── rust-toolchain.toml        # pinned toolchain for CI reproducibility
├── src/
│   ├── main.rs                # argv normalisation, top-level error rendering, exit code
│   ├── cli.rs                 # clap definition: -D key=value, --sonar-* long forms, --dry-run, -v
│   ├── config/
│   │   ├── mod.rs             # PropertySource stack + merge, pure function of (argv, env, cwd)
│   │   ├── env.rs             # SONAR_* individual vars + SONAR_SCANNER_XXX systematic mapping
│   │   ├── json_params.rs     # SONAR_SCANNER_JSON_PARAMS / SONARQUBE_SCANNER_PARAMS
│   │   └── files.rs           # project-level and user-level properties files
│   ├── endpoint.rs            # Server vs Cloud vs region resolution, apiBaseUrl derivation
│   ├── http.rs                # blocking client: bearer auth, redirects, timeouts, proxy, TLS
│   ├── version.rs             # /analysis/version + /api/server/version fallback, min-version gate
│   ├── jre.rs                 # /analysis/jres, download, verify, extract, locate javaPath
│   ├── engine.rs              # /analysis/engine, download, verify
│   ├── cache.rs               # <userHome>/cache/<sha256>/<filename> + _extracted marker
│   ├── archive.rs             # zip + tar.gz extraction, path-traversal guard, unix permissions
│   ├── process.rs             # spawn java, write JSON stdin, read NDJSON stdout, propagate exit
│   ├── platform.rs            # os/arch detection incl. Alpine
│   ├── logging.rs             # scanner-format log emitter, secret redaction
│   └── dryrun.rs              # resolve-and-print without contacting a server
└── tests/                     # integration tests against a local HTTP server + fake java
```

### Dependency recommendations

| Need | Crate | Rationale |
|---|---|---|
| CLI | `clap` (derive) | standard; supports `-D key=value` repeated |
| HTTP | `ureq` (blocking) | every bootstrap step is sequential — no async runtime needed; small dependency tree; streaming bodies |
| TLS | `native-tls` backend | uses OS trust roots so corporate TLS interception works out of the box (design §8 "Compatibility"), and gives PKCS#12 client identities for free if `sonar.scanner.keystorePath` is implemented later |
| JSON | `serde` + `serde_json` | stdin payload, API responses, NDJSON log parsing |
| Checksums | `sha2` | server returns `sha256` |
| Archives | `zip`, `flate2` + `tar` | JRE archives are `.zip` or `.tar.gz`; `tar` preserves unix permissions |
| Temp files | `tempfile` | download-to-temp then atomic rename |
| Home dir | `home` or `dirs` | `~/.sonar` default for `sonar.userHome` |
| Errors | `thiserror` (+ `anyhow` at the boundary) | typed errors per stage, one rendering point in `main.rs` |

Deliberately **not** async, and **not** `reqwest` — see design §8 "Performance".

---

## 5. The contract, transcribed

### 5.1 Properties the bootstrapper sets and the user cannot override

| Property | Value |
|---|---|
| `sonar.scanner.app` | `"cargo"` — per the guidelines' naming convention *(maven, gradle, cli, npm, dotnet, …)*. Must match the engine's auto-config allow-list byte-for-byte (engine plan M0). |
| `sonar.scanner.appVersion` | crate version (`env!("CARGO_PKG_VERSION")`) |
| `sonar.scanner.bootstrapStartTime` | ms since epoch, captured at process start |
| `sonar.scanner.wasJreCacheHit` | `HIT` \| `MISS` \| `DISABLED` |
| `sonar.scanner.wasEngineCacheHit` | `true` \| `false` |

### 5.2 Properties the bootstrapper consumes

`sonar.host.url` (`SONAR_HOST_URL`) · `sonar.region` (`SONAR_REGION`, value `us`) ·
`sonar.scanner.sonarcloudUrl` · `sonar.scanner.apiBaseUrl` · `sonar.token` (`SONAR_TOKEN`) ·
`sonar.userHome` (`SONAR_USER_HOME`, default `~/.sonar`) ·
`sonar.scanner.os` / `sonar.scanner.arch` (`SONAR_SCANNER_OS` / `_ARCH`) ·
`sonar.scanner.skipJreProvisioning` · `sonar.scanner.javaExePath` · `sonar.scanner.engineJarPath` ·
`sonar.scanner.javaOpts` (`SONAR_SCANNER_JAVA_OPTS`) ·
`sonar.scanner.connectTimeout` (5) / `socketTimeout` (60) / `responseTimeout` (0) ·
`sonar.scanner.proxy{Host,Port,User,Password}` · `sonar.verbose` ·
`sonar.scanner.internal.dumpToFile` · `sonar.scanner.internal.sqVersion` (testing hooks — implement both,
they make the whole bootstrap testable without a server).

Pass-through only (bootstrapper does not act on them in v1, but must forward them to the engine):
`sonar.scanner.truststorePath` / `truststorePassword` / `keystorePath` / `keystorePassword`.
*(pysonar does not implement these bootstrapper-side either — see OQ-4.)*

### 5.3 Configuration precedence (highest first)

1. Command-line arguments (`-Dsonar.token=…`, `--sonar-token …`)
2. Individual environment variables (`SONAR_TOKEN`, `SONAR_HOST_URL`, `SONAR_SCANNER_*`)
3. `SONAR_SCANNER_JSON_PARAMS` (fallback: `SONARQUBE_SCANNER_PARAMS`), a JSON object of key→value
4. *(JVM system properties — N/A for a Rust bootstrapper)*
5. Project-level configuration file — **`sonar-project.properties` in the base directory** (OQ-2)
6. User-level configuration file — **`<sonar.userHome>/sonar-scanner.properties`** (OQ-2)

Systematic env mapping: `SONAR_SCANNER_XXX_YYY` → `sonar.scanner.xxxYyy` (split on `_`, camel-case the tail).
Example: `SONAR_SCANNER_PROXY_PORT=123` → `sonar.scanner.proxyPort=123`.

### 5.4 Endpoint resolution

- No `sonar.host.url` and no `sonar.region` ⇒ SonarQube Cloud global: host `https://sonarcloud.io`,
  API base `https://api.sonarcloud.io`.
- `sonar.region=us` ⇒ host `https://sonarqube.us`, API base `https://api.sonarqube.us` (region is case-insensitive).
- `sonar.host.url` matching sonarcloud.io (with/without trailing slash, with/without `www.`) ⇒ equivalent to Cloud global.
- `sonar.host.url` matching sonarqube.us likewise ⇒ equivalent to `sonar.region=us`.
- Any other `sonar.host.url` ⇒ SonarQube Server, API base `<host>/api/v2`.
- Inconsistent combinations (e.g. `sonar.host.url=https://sonarcloud.io` **and** `sonar.region=us`) ⇒ **fail with a clear message**, never guess.
- `sonar.host.url` must be passed to the engine even for Cloud.
- Reference implementation to mirror, including its test cases:
  [`ScannerEndpointResolver.java`](https://github.com/SonarSource/sonar-scanner-java-library/blob/7556a8f71999ad457ac77ec2416b1d837539b478/lib/src/main/java/org/sonarsource/scanner/lib/internal/endpoint/ScannerEndpointResolver.java#L37).

### 5.5 Version check

1. `GET <apiBaseUrl>/analysis/version` (authenticated).
2. If ≠ 200, `GET <sonar.host.url>/api/server/version`.
3. Cloud, or Server ≥ 10.6 ⇒ proceed. Older Server ⇒ **actionable error pointing at the CLI scanner**;
   we carry no legacy classloader path. Minimum supported version to confirm — OQ-3.

### 5.6 JRE resolution

1. `sonar.scanner.javaExePath` set ⇒ use it.
2. Else unless `sonar.scanner.skipJreProvisioning`:
   `GET <apiBaseUrl>/analysis/jres?os=<os>&arch=<arch>` (authenticated) → take the **first** entry of the list.
   Response fields: `id`, `filename`, `sha256`, `javaPath`, `os`, `arch`, `downloadUrl?`.
   Download via `downloadUrl` if present (no auth — third-party CDN), else
   `GET <apiBaseUrl>/analysis/jres/<id>` with `Accept: application/octet-stream` (authenticated; handle 200 **and** 302).
   Extract; archive type from the filename extension (`zip`, `tar.gz`); **preserve file permissions** on unix.
   Locate the binary via `javaPath`.
   Empty list ⇒ os/arch unsupported by that server; fall through to step 3.
3. Else `JAVA_HOME/bin/java[.exe]`.
4. Else `java[.exe]` from `PATH` — **mind untrusted-search-path on Windows**: do not resolve `java` from the
   current directory; use an explicit `PATH` search that skips `.`.

### 5.7 Engine resolution

1. `sonar.scanner.engineJarPath` set ⇒ use it.
2. Else `GET <apiBaseUrl>/analysis/engine` with `Accept: application/json` (authenticated)
   → `{filename, sha256, downloadUrl?}`; download via `downloadUrl` (no auth) or re-issue the same URL with
   `Accept: application/octet-stream` (authenticated).

### 5.8 Cache

```
<sonar.userHome>/cache/<sha256>/<filename>
<sonar.userHome>/cache/<sha256>/<filename>_extracted/     # JRE only; filename incl. extension
```

Race-safe algorithm, applied identically to files and to extractions:

1. Check the final location; if present, done (cache hit).
2. Download / extract into a temporary location **in the same filesystem**.
3. Verify the sha256 (files).
4. Attempt an **atomic rename** to the final location.
5. **Tolerate a rename failure caused by the destination already existing** — another scanner won the race.

Checksum mismatch ⇒ **retry exactly once** from the metadata call, because the legitimate cause is a new
artifact published between the metadata call and the download. Second mismatch ⇒ hard error.

### 5.9 Authentication and credential safety

- All Sonar API calls are **pre-emptively authenticated**: `Authorization: Bearer <sonar.token>`.
- If both token and the deprecated `sonar.login`/`sonar.password` are configured, log a warning.
- **Downloads:** forward the token **only if the download URL shares the origin of `sonar.host.url` or
  `sonar.scanner.apiBaseUrl`**. Never send it to a third-party CDN.
- Authentication **is** preserved across redirects (301/302/307/308) to the same origin. Decide and document
  the behaviour for a cross-origin redirect — recommendation: drop the credential.
- Error messages, verbatim from the guidelines:

| Status | SonarQube Server | SonarQube Cloud |
|---|---|---|
| 401 | `Unable to authenticate on SonarQube Server. Please check your token or generate a new one at <server URL>/account/security` | `Unable to authenticate on SonarQube Cloud [<region>]. Please check your token or generate a new one at <cloud URL>/account/security` |
| 403 | `You don't have permission to execute an analysis on this SonarQube Server instance.` | `You don't have permission to execute an analysis in any organization on SonarQube Cloud [<region>].` |

- The token must never appear in any log line, including `--verbose` diagnostics and the dry-run dump.
  Redact centrally in `logging.rs`, not at each call site, and unit-test the redaction.

### 5.10 Platform detection

Send **raw, unprocessed** values — the server accepts a broad alias set precisely so bootstrappers don't
normalise. From Rust: `std::env::consts::OS` (`linux` / `macos` / `windows`) and
`std::env::consts::ARCH` (`x86_64` / `aarch64`) are all accepted aliases.

The **one** exception is Alpine, which the server treats as a distinct OS and which cannot be distinguished
otherwise:

```
is_alpine() = cfg!(target_os = "linux")
           && first_of(read("/etc/os-release"), read("/usr/lib/os-release"))
              matches /^ID=([^\r\n]*)/m  with capture == "alpine"
```

`std::env::consts::ARCH` is the *binary's* architecture, not the CPU's (same caveat as Java and Node — an
x86_64 binary under Rosetta reports `x86_64`). Accepted corner case; `sonar.scanner.arch` is the escape hatch.

### 5.11 Engine invocation

```
<java> <sonar.scanner.javaOpts…> -jar <engine.jar>
```

- **stdin:** one JSON document
  ```json
  { "scannerProperties": [ {"key": "sonar.scanner.app", "value": "cargo"}, … ] }
  ```
  Mandatory keys: `sonar.scanner.app`, `sonar.scanner.appVersion`, `sonar.host.url`, `sonar.token`.
- **stdout:** newline-delimited JSON, `{"message": "...", "level": "TRACE|DEBUG|INFO|WARN|ERROR", "stacktrace": "optional"}`.
  Re-emit in the scanner log format. **Anything unparseable on stdout is logged as INFO, anything on stderr
  as ERROR** — that is the specified behaviour, not a fallback to hide.
- A **full copy of the environment** is passed to the subprocess.
- The engine's exit code becomes ours (0 = success).
- ⚠️ **Deadlock hazard.** Writing the whole property document to the child's stdin while not draining its
  stdout deadlocks once the payload exceeds a pipe buffer (64 KiB on Linux). Write stdin from a dedicated
  thread, or drain stdout concurrently. There must be a test with an oversized payload — this is the classic
  bug in this design.

### 5.12 Properties the bootstrapper contributes on the project's behalf

| Property | Value | Note |
|---|---|---|
| `sonar.projectBaseDir` | current working directory | Set explicitly. Engines ≥ 10.6 default to cwd anyway, but setting it makes the dry-run dump honest and keeps older servers working. Do **not** walk up looking for a workspace root — that would require reading `Cargo.toml`. Running from inside a member crate therefore analyses that member; document it. |
| `sonar.buildsystem.autoconfig.disabled` | ~~`false`, **user-overridable**~~ **not set** | Engine-side auto-config became opt-in in `SCANENGINE-542`, so sending `false` looked necessary. It is not: the engine's `isBuildSystemAutoConfigurationEnabled` also requires `sonar.scanner.app == "ScannerCLI"`, so with `app = "cargo"` the property changes nothing today, and the bootstrapper does not forward a default it cannot make true. **This becomes a live decision the moment engine M0 lands:** the property's own default in the engine is `true`, so once `"cargo"` is allow-listed, auto-config still stays off unless the engine flips that default (`SCANENGINE-557`) or the bootstrapper starts sending `false` after all. Decide there, not here. |

---

## 6. Milestones

Sequenced so the riskiest part — configuration resolution — is reviewable before any network plumbing exists.
One PR per milestone; commit messages `<TICKET>-123 Message` (ticket, space, capital letter, no colon).

### M0 — Skeleton ✅ done

`Cargo.toml`, `rust-toolchain.toml`, `src/main.rs` with argv normalisation (strip `argv[1] == "sonar-scanner"`),
clap CLI, license headers, `rustfmt.toml`, `clippy` in CI. Fill in `.github/workflows/build.yml`
(build + test + fmt + clippy + SonarQube analysis of this repo). Decide and document the MSRV.

**Exit:** `cargo install --path .` then `cargo sonar-scanner --help` works.

*Delivered.* MSRV is 1.88 (`let` chains), compiled by CI so it is a verified claim.

### M1 — Configuration and connection resolution, fully offline ✅ done

- The whole property stack of §5.3 as a **pure function of (argv, env, cwd)** — no I/O beyond reading the two
  properties files. This is what makes precedence snapshot-testable with zero mocking.
- Endpoint resolution (§5.4) with the inconsistency errors.
- Platform detection (§5.10).
- `--dry-run`: resolve everything, print the final property set (token redacted), contact nothing, exit 0.
  Model it on pysonar's `DRY_RUN_MODE.md`; its extra validation of coverage-report paths is a good idea but
  belongs after M3, since for us those paths are derived engine-side.
- `sonar.scanner.internal.dumpToFile` + `sonar.scanner.internal.sqVersion`.

**Exit:** a table-driven test suite covers every precedence rule and every endpoint combination; `--dry-run`
prints a correct property set for a fixture project with no network.

*Delivered*, with two deviations from the plan as written:

- **`[package.metadata.sonar]` is read by the bootstrapper** (`src/config/manifest.rs`), which is the opposite
  of OQ-2's recommendation. It sits between the JSON params and `sonar-project.properties`, and reads exactly
  one reserved table — nothing else in the manifest is interpreted, so the bootstrapper still knows nothing
  about workspaces, targets or `target/`. Property names are *derived* (kebab-case → camelCase, nested tables →
  dotted segments) rather than allow-listed, so there is no per-property maintenance burden and no split
  namespace to explain: `sonar.host.url` is the single alias the convention cannot produce.
- **`sonar.buildsystem.autoconfig.disabled` is not set** — see §5.12, which records why the value specified
  there would have been inert.

`sonar.scanner.internal.sqVersion` was deferred to M2, where the version check it short-circuits lives.

### M2 — Provisioning

HTTP client (bearer auth, timeouts, proxy, redirects), version check, JRE and engine resolution,
cache, archive extraction, checksum verification with the single retry.

**Exit:** against a local HTTP server, a JRE and an engine jar are fetched, verified, cached, and re-used on a
second run; a corrupted download retries once then fails cleanly; a traversal archive is rejected.

Reviewable as one stacked branch per module, bottom to top: `http` → `version` → `cache` → `archive` →
`jre` → `engine`. Each layer only depends on the ones below it, so each PR is a self-contained unit with its
own tests.

### M3 — Engine handoff

Subprocess spawn, JSON stdin, NDJSON stdout re-emission, exit-code propagation, environment inheritance.
First real analysis. **The tool starts dogfooding on its own repository** via `unified-dogfooding.yml`.

**Exit:** `cargo sonar-scanner` produces an analysis on a disposable SonarQube Server.

### M4 — End-to-end validation and documentation

Fixture projects (§7), README, `CLI_ARGS.md`-equivalent, `DRY_RUN_MODE.md`-equivalent, troubleshooting guide.
**Depends on engine milestone M0** (the app allow-list) for anything to be auto-configured.

### M5 — Release automation and distribution

`release.yml` already wires `gh-action_release@7.4.0`; add the crates.io publish path and prebuilt binaries
(linux x86_64/aarch64 incl. musl, macOS x86_64/aarch64, windows x86_64).

The design doc's *Publishing on crates.io* section specifies the manifest and ownership rules — follow it
rather than improvising. The **publishing mechanism**, however, is now decided by the infrastructure work
below, which supersedes the design doc's standalone Trusted Publishing workflow.

**A published `.crate` is public and immutable — it cannot be overwritten or removed.** That is why the repo
is public: source provenance, issue tracking, release notes, and a verifiable publish identity.

#### The credential exists already — `PREQ-7880` (Done)

| Item | State |
|---|---|
| crates.io technical account | **`sonartech`** — created, and the intended crate owner |
| API token | issued |
| Vault secret | `development/kv/data/crates-io`, key `token` |
| 1Password entry | present |
| Rotation runbook | [Rotate sonartech crates.io API Token](https://xtranet-sonarsource.atlassian.net/wiki/spaces/Platform/pages/5466456069/Rotate+sonartech+crates.io+API+Token+-+Engineering+Experience+Squad) (xtranet, space Platform) |

So `cargo publish` authenticates with a **Vault-sourced `CARGO_REGISTRY_TOKEN`**, not crates.io Trusted
Publishing. PREQ-7880 was raised for a crate called `sonar-scanner`; the real name is **`cargo-sonar-scanner`**,
per `Cargo.toml`, and the ticket records the correction.

#### Remaining pipeline work — `BUILD-12231` (Open, unassigned, `eng-xp-needs-refinement`)

Four changes across three repositories. None of them are in place as of 2026-08-12 — verified against the
repositories, not just the ticket.

1. **`gh-action_release`: new `cratesio.yaml` reusable workflow.** A draft exists locally but is not pushed;
   the repo currently has only `maven-central.yaml`, `npmjs.yaml`, `pypi.yaml` and `javadoc-publication.yaml`.
   It must **check out the calling repo's source** rather than download a pre-built artifact, because
   `cargo publish` always re-packages from source — there is no "publish this pre-built file" equivalent to
   npm/PyPI. It bumps the crate version, routes dependency resolution through Repox (the same pattern as this
   repo's own CI), then publishes with `CARGO_REGISTRY_TOKEN`.
2. **`gh-action_release`: wire it into `main.yaml`.** A `publishToCratesIo` boolean in `workflow_call.inputs`
   and a `cratesIo` job, mirroring `pypi` (`main.yaml:356`) and `npmjs` (`main.yaml:369`) exactly:
   `needs: release`, `if: ${{ inputs.publishToCratesIo && inputs.dryRun != true }}`, then added to the
   `needs:` lists of the `publish` and `datadog` jobs and to the datadog status expression.
3. **`re-terraform-aws-vault`: grant the secret.** Add `development/kv/data/crates-io` to
   `sonar-scanner-cargo`'s `kv_paths` in `orders/code-quality-ci-experience-squad.yaml`. Confirmed absent —
   the repo is granted `datadog`, `jira`, `repox`, `sign`, `slack`, `mend`, `iris` and
   `cloudflare/warp-github-runner`, but nothing for crates.io.
4. **This repo: `release.yml`.** Pass `publishToCratesIo: true`. It currently passes only `version`.

⚠️ **Version format.** `main.yaml`'s `version` input is a full version *including a build number*
(`1.2.3.456`), while crates.io requires a SemVer version. Confirm what `cratesio.yaml` publishes — presumably
the three-component prefix — before the first release, because the wrong string is published immutably.

*Manifest metadata* — available crate name, new SemVer version, non-empty `description`, and either an SPDX
`license` expression or a packaged `license-file`. Also declare `readme`, `rust-version`, `documentation`,
`categories`, `keywords`, `repository`:

```toml
[package]
name = "cargo-sonar-scanner"
version = "0.1.0"
description = "Run SonarQube and SonarQube Cloud analysis from Cargo."
license = "<approved SPDX expression>"
readme = "README.md"
rust-version = "<MSRV>"
repository = "https://github.com/SonarSource/sonar-scanner-cargo"
documentation = "https://docs.rs/cargo-sonar-scanner"
publish = ["crates-io"]
```

*Pre-release gate* — CI must package the exact archive and review its contents every time. This is the
control that stops credentials, internal endpoints, or proprietary test material leaking into an immutable
public artifact:

```bash
cargo package --list
cargo publish --dry-run --locked
```

All normal and build dependencies must resolve from crates.io. Anything internal-only goes to Repox with
`publish = false`.

*Ownership* — the first publisher becomes an owner, which is exactly why the `sonartech` account exists: the
initial `0.1.0` must be published by it and never from a developer's personal account. Immediately after that
first publish, add a team owner for continuity:

```bash
cargo owner --add github:SonarSource:<team> cargo-sonar-scanner
cargo owner --list cargo-sonar-scanner
```

Team owners can publish and yank but not change ownership; keep named administrative owners to a small
number. The `sonartech` token lives in Vault and nowhere else — never copy it into CI secrets, a `.env`, or
`~/.cargo/credentials.toml` on a workstation, and if it ever appears in a log or an artifact, follow the
rotation runbook rather than improvising.

*Automated releases* — the design doc proposed crates.io Trusted Publishing with GitHub Actions OIDC and a
standalone tag-triggered workflow. **That is not the path being taken.** Publishing goes through the standard
`gh-action_release` reusable workflow with a Vault-sourced token, per BUILD-12231, so there is exactly one
publish path and it is the same one every other SonarSource artifact uses. Trusted Publishing remains a
possible later simplification — `main.yaml` already has a `useNpmTrustedPublisher` input showing the pattern
is being adopted registry by registry — but adopting it is not a prerequisite for the first release.

**Gate before the first release:** the licence choice (`MIT OR Apache-2.0` is the Rust-ecosystem norm but is
*not* a crates.io requirement, and this repo currently ships SSAL v1), the contributor-rights model, the
required notices, and the public support expectations must all be settled and signed off. This is the one item
in the plan that cannot be resolved by writing code.

---

## 7. Validation strategy

| Area | Approach |
|---|---|
| Configuration | Pure function of (argv, env, cwd) ⇒ table-driven + snapshot tests, no mocking. Cover every row of §5.3 and every branch of §5.4. |
| HTTP | Against a **real local HTTP server**, not mocks, so the real client is exercised: bearer header present/absent, redirect chains, streaming, timeouts, and specifically **that no `Authorization` header reaches a foreign-origin download URL**. |
| Cache & archives | Real temp directories. Concurrent-extraction race (two threads, same artifact). Zip-slip / tar traversal rejection. Unix permission preservation from `tar.gz`. |
| Process | Against a **real test executable** standing in for `java`: NDJSON passthrough, malformed line → INFO, stderr → ERROR, exit-code propagation, and **an input large enough to exceed a pipe buffer**. |
| Secrets | Assert the token appears in no log stream at any verbosity, and not in the dry-run dump. |
| End to end | Disposable SonarQube Server with the Rust analyzer installed, over fixture projects: single crate; virtual workspace; nested invocation from inside a member; inherited workspace fields; relocated output directory. Assert what actually landed: every member analysed, no build output indexed, coverage ingested. |
| **Cross-flavour equivalence** | The same fixtures scanned with the **CLI scanner** must produce the same configuration. This test lives in `sonar-scanner-engine` (engine plan M6) — it is the proof that the split achieved its purpose. |

`~/git/sonarsource/sonar-scanner-integration-tester` may be reusable for the end-to-end layer; check before
building anything bespoke.

---

## 8. Open questions

Decisions still to make. Each has a recommendation where one exists, so the default action is to follow it
unless the implementation turns up something that contradicts it. Anything marked *verify* is answerable from
an existing artifact — the guidelines, a reference implementation, or the server — rather than by discussion.

| # | Question | Blocks |
|---|---|---|
| OQ-1 | ~~Scanner identity~~ — **DECIDED: `sonar.scanner.app = "cargo"`**, per the guidelines' naming convention. (`ScannerCLI`/`ScannerMSBuild` are legacy spellings; pysonar already sends plain `"python"`.) Engine M0 hard-codes the identical string. Remaining action: announce the value before GA so telemetry dashboards and any server-side allow-lists learn it. | resolved |
| OQ-2 | ~~**Configuration file names.**~~ **Settled by M1.** `sonar-project.properties` in the base dir and `<sonar.userHome>/sonar-scanner.properties`, both as proposed. The third part was decided the other way: `[package.metadata.sonar]` *is* read, and outranks `sonar-project.properties` — see M1's notes for why that does not reintroduce a split namespace. | resolved |
| OQ-3 | **Minimum supported server version**, given no legacy bootstrap path. 10.6 is the protocol floor; the guidelines also say new scanners should support down to LTS 9.9, which we cannot without the legacy path — pysonar set the precedent of requiring a modern server. *(Recommendation: require 10.6 and follow pysonar; verify against what pysonar actually gates on.)* | M2 |
| OQ-4 | **Truststore / keystore.** v1 uses OS trust roots and merely forwards `sonar.scanner.truststorePath`/`keystorePath` to the engine (pysonar does the same). Acceptable, or must the bootstrapper itself honour a PKCS#12 truststore for its own API calls? *(Recommendation: forward only in v1; `native-tls` leaves the door open.)* | M2 scope |
| OQ-5 | **Licensing and crates.io metadata.** The licence expression, contributor-rights model, required notices and public support expectations. The *publication pipeline* half of this question is answered: `sonartech` + Vault token + `gh-action_release`, see M5. What remains needs a licensing sign-off, which is not a technical decision. | M5 |
| OQ-6 | **Cross-origin redirect credential policy** — the guidelines say auth is preserved on redirect, but that conflicts with "never leak the token to a third party" when the redirect leaves the origin. *(Recommendation: preserve same-origin, drop cross-origin; assert it in a test.)* | M2 |
| OQ-7 | **Log format.** Match `sonar-scanner-cli`'s exact output shape (`INFO: …`) — verify against the CLI's real output before freezing, since users grep it in CI. | M3 |

---

## 9. Cross-repo dependency

```
engine M0  (open the auto-config app allow-list)      ← blocks → bootstrapper M4 (end-to-end)
engine M1–M4 (Cargo reader + converter + registration) ← blocks → bootstrapper M4 assertions
bootstrapper M1–M3                                     ← independent, start immediately
```

Land **engine M0 early**. It is small and self-contained, but it gates every end-to-end test on this side and
is exactly the kind of thing that gets discovered late.
