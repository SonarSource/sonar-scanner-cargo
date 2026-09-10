# SonarScanner for Cargo

Run SonarQube Server and SonarQube Cloud analysis on a Cargo project with one command:

```console
$ export SONAR_TOKEN=...
$ cargo sonar-scanner
```

No Java installation is needed: the scanner provisions its own JRE and analysis engine on first run
and caches them under `~/.sonar`. With nothing else configured the analysis targets SonarQube Cloud;
SonarQube Server must be 10.6 or newer.

## Install

```console
$ cargo binstall cargo-sonar-scanner  # a prebuilt binary
$ cargo install cargo-sonar-scanner   # compiled from source
```

The binary is called `cargo-sonar-scanner`; once it is on `PATH`, Cargo resolves
`cargo sonar-scanner`. [`cargo binstall`](https://github.com/cargo-bins/cargo-binstall) downloads a
prebuilt archive for your platform instead of compiling, and offers a source build on platforms with
no published archive — see
[Prebuilt binaries](https://docs.sonarsource.com/sonarqube-server/analyzing-source-code/scanners/sonarscanner-for-cargo/#prebuilt-binaries)
in the SonarScanner for Cargo documentation for direct downloads and signature verification.

## Usage

```console
$ cargo sonar-scanner --help
```

Analysis parameters are Sonar properties, resolved from the command line first, then environment
variables, then the `[package.metadata.sonar]` table in `Cargo.toml`:

```toml
[package.metadata.sonar]
project-key = "my-org_my-crate"
host-url = "https://sonarqube.example.com"
exclusions = ["vendor/**"]
```

In a virtual workspace — a root `Cargo.toml` with no `[package]` — use `[workspace.metadata.sonar]`
instead.

> **Do not put your token in `Cargo.toml`.** It gets committed, and for a published crate it ships
> inside the `.crate` archive on crates.io, where it cannot be deleted. Use `SONAR_TOKEN`.

See the **SonarScanner for Cargo** documentation for the full reference — configuration precedence,
key naming, custom certificates, endpoint resolution, and troubleshooting — on
[SonarQube Server](https://docs.sonarsource.com/sonarqube-server/analyzing-source-code/scanners/sonarscanner-for-cargo/)
or
[SonarQube Cloud](https://docs.sonarsource.com/sonarqube-cloud/analyzing-source-code/scanners/sonarscanner-for-cargo/).
This repository's [user guide](crate-docs/user-guide.md) covers the same ground for offline reading.

## Getting Help

Start with `cargo sonar-scanner --dry-run`: it resolves the configuration and prints every property
with its origin, making no network request. That answers most "where did that value come from?"
questions before anything else.

For problems specific to this scanner, see the
[troubleshooting](https://docs.sonarsource.com/sonarqube-server/analyzing-source-code/scanners/sonarscanner-for-cargo/#troubleshooting)
section of the SonarScanner for Cargo documentation (also on
[SonarQube Cloud](https://docs.sonarsource.com/sonarqube-cloud/analyzing-source-code/scanners/sonarscanner-for-cargo/#troubleshooting)).
If you can't find an answer there, reach out in the
[community forum and help center](https://community.sonarsource.com/).

## Development

```console
$ cargo test
$ cargo fmt --check
$ cargo clippy --all-targets -- -D warnings
$ cargo install --path .   # build and install your local changes as `cargo-sonar-scanner`
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
analyzers it provisions at run time, including the Rust analyzer used by SonarQube Server and
SonarQube Cloud, carry their own licenses.
