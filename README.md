# SonarScanner for Cargo

Analyse a Cargo project with SonarQube Server or SonarQube Cloud, from the project's directory:

```console
$ export SONAR_TOKEN=...

# SonarQube Cloud
$ cargo sonar-scanner -Dsonar.projectKey=my_project -Dsonar.organization=my_organization

# SonarQube Server
$ cargo sonar-scanner -Dsonar.projectKey=my_project -Dsonar.host.url=https://sonarqube.example.com
```

With no host URL set the analysis goes to SonarQube Cloud, which is why only the first command names
an organization: that is a SonarQube Cloud concept, and there is nothing to name on a server. A
project key is required either way. Put the properties that apply in `Cargo.toml` (see
[Configure](#configure)) and the command comes down to a bare `cargo sonar-scanner`.

Full documentation, including the configuration reference and troubleshooting:
**[SonarQube Server](https://docs.sonarsource.com/sonarqube-server/analyzing-source-code/scanners/sonarscanner-for-cargo/)**
| **[SonarQube Cloud](https://docs.sonarsource.com/sonarqube-cloud/analyzing-source-code/scanners/sonarscanner-for-cargo/)**

## Install

```console
$ cargo binstall cargo-sonar-scanner  # a prebuilt binary
$ cargo install cargo-sonar-scanner   # compiled from source
```

The binary is called `cargo-sonar-scanner`; once it is on `PATH`, Cargo resolves
`cargo sonar-scanner`. [`cargo binstall`](https://github.com/cargo-bins/cargo-binstall) downloads a
prebuilt archive for your platform instead of compiling, and prompts to build from source on
platforms with no published archive (`-y` accepts).

You need Cargo, and SonarQube Server 2026.1 or newer. No Java installation is required: the scanner
provisions its own JRE and analysis engine on first run and caches them under `~/.sonar`.

## Configure

Identify the project in the `[package.metadata.sonar]` table of your `Cargo.toml`:

```toml
[package.metadata.sonar]
project-key = "my_project"

# SonarQube Cloud
organization = "my_organization"

# SonarQube Server
host-url = "https://sonarqube.example.com"

# Cargo build output is not excluded for you
exclusions = ["target/**"]
```

In a virtual workspace (a root `Cargo.toml` with no `[package]`), use `[workspace.metadata.sonar]`
instead.

> **Do not put your token in `Cargo.toml`.** It gets committed, and for a published crate it ships
> inside the `.crate` archive on crates.io, where it cannot be deleted. Use `SONAR_TOKEN`.

Properties can also come from the command line (`-Dsonar.projectKey=my_project`), from environment
variables, or from a `sonar-project.properties` file. A project key is always required, wherever you
set it.

## Run

```console
$ cargo sonar-scanner              # with the project identified in Cargo.toml
$ cargo sonar-scanner --dry-run    # resolve the configuration and contact nothing
$ cargo sonar-scanner --help
```

## Getting help

Start with `--dry-run`: it prints the endpoint, the base directory, and every resolved property with
its origin, making no network request. That answers most "where did that value come from?" questions
on its own.

Beyond that, see the troubleshooting section of the documentation for
[SonarQube Server](https://docs.sonarsource.com/sonarqube-server/analyzing-source-code/scanners/sonarscanner-for-cargo/#troubleshooting)
or
[SonarQube Cloud](https://docs.sonarsource.com/sonarqube-cloud/analyzing-source-code/scanners/sonarscanner-for-cargo/#troubleshooting),
then the [community forum and help center](https://community.sonarsource.com/).

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
