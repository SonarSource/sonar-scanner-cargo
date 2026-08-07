# SonarScanner for Cargo

Run SonarQube Server and SonarQube Cloud analysis on a Cargo project with one command:

```console
$ export SONAR_TOKEN=...
$ cargo sonar-scanner
```

> **Status: work in progress.** This build is the crate skeleton and the command line interface
> only. Configuration resolution, JRE and scanner engine provisioning, and the handoff to the
> scanner engine are still to come, so `cargo sonar-scanner` currently stops with an explanatory
> error. See SCANCARGO-2.

## Install

```console
$ cargo install --path .          # from a checkout
```

The binary is called `cargo-sonar-scanner`; once it is on `PATH`, Cargo resolves
`cargo sonar-scanner`. It also works when invoked directly as `cargo-sonar-scanner <args…>`.

## Usage

```console
$ cargo sonar-scanner --help
```

Analysis parameters are Sonar properties. Set them with `-Dkey=value`, with the `--sonar-*`
options, with environment variables, or in `Cargo.toml`.

## Configuring the analysis in `Cargo.toml`

The Cargo-native place to configure the analysis is the `[package.metadata.sonar]` table — the
table Cargo reserves for third-party tools and otherwise ignores completely:

```toml
[package]
name = "my-crate"
version = "0.1.0"

[package.metadata.sonar]
project-key = "my-org_my-crate"
host-url = "https://sonarqube.example.com"
exclusions = ["vendor/**"]

[package.metadata.sonar.scanner]
java-opts = "-Xmx1g"
```

resolves to:

```
sonar.projectKey=my-org_my-crate
sonar.host.url=https://sonarqube.example.com
sonar.exclusions=vendor/**
sonar.scanner.javaOpts=-Xmx1g
```

In a **virtual workspace** — a root `Cargo.toml` with no `[package]` — use
`[workspace.metadata.sonar]` instead. If a manifest has both tables, `[package.metadata.sonar]`
wins key by key, so a workspace root can hold shared settings and a member can override one of
them.

Only this one table is read, and only from the manifest in the base directory. The scanner does not
otherwise interpret `Cargo.toml`: workspace membership, targets, source layout and inherited fields
are derived by the scanner engine during the analysis.

### Key naming

| You write | It becomes |
|---|---|
| `project-key = "x"` | `sonar.projectKey=x` |
| a nested table, `[package.metadata.sonar.scanner] java-opts = "…"` | `sonar.scanner.javaOpts=…` |
| `exclusions = ["a/**", "b/**"]` | `sonar.exclusions=a/**,b/**` |
| `verbose = true`, `connect-timeout = 30` | `sonar.verbose=true`, `…connectTimeout=30` |
| `host-url = "…"` | `sonar.host.url=…` — the one alias |
| `"sonar.cpd.exclusions" = "…"` | `sonar.cpd.exclusions=…` — verbatim |

Bare keys are kebab-case and get a `sonar.` prefix; nested tables become dotted segments. Every
property is accepted — the name is derived, not looked up in a list — so `[…sonar.scanner]
proxy-port = 3128` gives `sonar.scanner.proxyPort=3128` with no special handling. The exception is
`sonar.host.url`, whose real name is dotted where the convention would produce `sonar.hostUrl`;
it has an alias. Anything else the convention cannot express can be written as a quoted,
fully-qualified property name.

> **Do not put your token in `Cargo.toml`.** It is committed, and for a library crate it is
> published inside the `.crate` archive on crates.io, where it cannot be deleted. The scanner warns
> if it finds one. Use `SONAR_TOKEN` instead.

## Endpoint resolution

| Configuration | Product | Host URL | API base URL |
|---|---|---|---|
| nothing set | SonarQube Cloud | `https://sonarcloud.io` | `https://api.sonarcloud.io` |
| `sonar.region=us` | SonarQube Cloud (US) | `https://sonarqube.us` | `https://api.sonarqube.us` |
| `sonar.host.url` = a Cloud URL | SonarQube Cloud | as above | as above |
| any other `sonar.host.url` | SonarQube Server | as given | `<host>/api/v2` |

The region is case-insensitive, and Cloud URLs are recognised with or without a trailing slash or a
`www.` prefix. Inconsistent combinations — a region together with a Server URL, or a region that
contradicts the Cloud URL — are rejected with an explicit error rather than guessed.
`sonar.scanner.apiBaseUrl` overrides the derived API base URL in every case.

## Base directory

`sonar.projectBaseDir` defaults to the current working directory and can be overridden. The
bootstrapper deliberately does **not** walk up looking for a workspace root, so running it from
inside a member crate analyses that member. Everything Cargo-specific — workspaces, targets, build
output — is the scanner engine's job.

## Output streams

`INFO` and `DEBUG` go to stdout, `ERROR` goes to stderr. The exit code is `0` on success and `1` on
failure.

## Development

```console
$ cargo test
$ cargo fmt --check
$ cargo clippy --all-targets -- -D warnings
```

The Rust toolchain is pinned in `rust-toolchain.toml`, which rustup and IDEs pick up
automatically. CI provisions the same version through [mise](https://mise.jdx.dev) (`mise.toml`),
and fails the build if the two disagree.

The MSRV in `Cargo.toml` (`rust-version`) is compiled by CI on every build, so it is a verified
claim rather than an aspiration. Raising it is a deliberate act: change `rust-version` and say why.

New source files must carry the header in [`license-header.txt`](license-header.txt); CI enforces
it.

## License

Copyright (C) SonarSource Sàrl. Licensed under the
[Sonar Source-Available License v1.0](LICENSE.txt), the same license as
[sonar-rust](https://github.com/SonarSource/sonar-rust). See also [NOTICE.txt](NOTICE.txt).
