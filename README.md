# SonarScanner for Cargo

Run SonarQube Server and SonarQube Cloud analysis on a Cargo project with one command:

```console
$ export SONAR_TOKEN=...
$ cargo sonar-scanner
```

## Install

```console
$ cargo binstall cargo-sonar-scanner  # a prebuilt binary
$ cargo install cargo-sonar-scanner   # compiled from source
```

The binary is called `cargo-sonar-scanner`; once it is on `PATH`, Cargo resolves
`cargo sonar-scanner`. [`cargo binstall`](https://github.com/cargo-bins/cargo-binstall) downloads a
prebuilt archive for your platform instead of compiling — see the
[prebuilt binaries](docs/user-guide.md#prebuilt-binaries) section of the user guide for direct
downloads and signature verification.

## Usage

```console
$ cargo sonar-scanner --help
```

Analysis parameters are Sonar properties, resolved from the command line, environment variables, or
the `[package.metadata.sonar]` table in `Cargo.toml`:

```toml
[package.metadata.sonar]
project-key = "my-org_my-crate"
host-url = "https://sonarqube.example.com"
```

See the **[user guide](docs/user-guide.md)** for the full reference: configuration precedence, key
naming, custom certificates, endpoint resolution, and troubleshooting.

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
[GNU Lesser General Public License, version 3](LICENSE.txt) (`LGPL-3.0-only`) — the same license
family as [SonarScanner CLI](https://github.com/SonarSource/sonar-scanner-cli), except that this
crate grants version 3 only, with no "or any later version" option.

The LGPL is a set of additional permissions on top of the GPL rather than a standalone license, so
[`COPYING.GPL-3.0.txt`](COPYING.GPL-3.0.txt) ships alongside it — that is the GPL-3.0 text the LGPL
incorporates, **not** an alternative license you may choose. See also [NOTICE.txt](NOTICE.txt).

The bootstrapper is licensed independently of what it downloads and runs: the scanner engine and the
analyzers it provisions at run time carry their own licenses, as does
[sonar-rust](https://github.com/SonarSource/sonar-rust).
