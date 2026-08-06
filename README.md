# SonarScanner for Cargo

Run SonarQube Server and SonarQube Cloud analysis on a Cargo project with one command:

```console
$ export SONAR_TOKEN=...
$ cargo sonar-scanner
```

> **Status: work in progress.** This build is the crate skeleton and the command line interface
> only. Configuration resolution, JRE and scanner engine provisioning, and the handoff to the
> scanner engine are still to come, so `cargo sonar-scanner` currently stops with an explanatory
> error. See RUST-593.

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

Analysis parameters are Sonar properties, set with `-Dkey=value` or with the `--sonar-*` options.

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
and fails the build if the two disagree. MSRV is 1.85 (edition 2024).

New source files must carry the header in [`license-header.txt`](license-header.txt); CI enforces
it.

## License

Copyright (C) SonarSource Sàrl. Licensed under the
[Sonar Source-Available License v1.0](LICENSE.txt), the same license as
[sonar-rust](https://github.com/SonarSource/sonar-rust). See also [NOTICE.txt](NOTICE.txt).
