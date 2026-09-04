# SonarScanner for Cargo

Run SonarQube Server and SonarQube Cloud analysis on a Cargo project with one command:

```console
$ export SONAR_TOKEN=...
$ cargo sonar-scanner
```

> **Status: work in progress.** The bootstrapper is complete — configuration resolution, JRE and
> scanner engine provisioning, and the handoff to the engine — but it has not been released yet, so
> it has to be built from a checkout. See SCANCARGO-2.

## Install

```console
$ cargo binstall cargo-sonar-scanner  # a prebuilt binary — once released
$ cargo install cargo-sonar-scanner   # compiled from source — once released
$ cargo install --path .              # from a checkout
```

The binary is called `cargo-sonar-scanner`; once it is on `PATH`, Cargo resolves
`cargo sonar-scanner`. It also works when invoked directly as `cargo-sonar-scanner <args…>`.

[`cargo binstall`](https://github.com/cargo-bins/cargo-binstall) is the fast route: it downloads the
archive for your platform from `binaries.sonarsource.com` rather than compiling the crate. It falls
back to `cargo install` on a platform we publish no binary for.

### Prebuilt binaries

Every build also produces a self-contained binary per platform, so CI does not need a Rust
toolchain to run an analysis. Released archives are published to
`https://binaries.sonarsource.com/Distribution/cargo-sonar-scanner/`:

| Platform | Archive |
| --- | --- |
| Linux x86_64 | `cargo-sonar-scanner-<version>-x86_64-unknown-linux-musl.tar.gz` |
| Linux aarch64 | `cargo-sonar-scanner-<version>-aarch64-unknown-linux-musl.tar.gz` |
| macOS x86_64 | `cargo-sonar-scanner-<version>-x86_64-apple-darwin.tar.gz` |
| macOS aarch64 | `cargo-sonar-scanner-<version>-aarch64-apple-darwin.tar.gz` |
| Windows x86_64 | `cargo-sonar-scanner-<version>-x86_64-pc-windows-gnu.zip` |

`<version>` there is the full build version, `<semver>-<build number>` — `0.1.0-46`, not `0.1.0`.
The crates.io version is the SemVer part alone, so `cargo binstall` reconstructs the rest from
metadata baked into the published crate.

Each archive is accompanied by a `.asc` detached signature, verifiable against the SonarSource
public key at <https://binaries.sonarsource.com/sonarsource-public.key>, and by `.md5`, `.sha1` and
`.sha256` sums.

A `cargo-sonar-scanner-<version>-checksums.txt` collecting the SHA-256 sums of all five archives in
`sha256sum -c` format is built alongside them, but stays in Repox — it is not part of the public
distribution.

The Linux builds are statically linked against musl, so one archive per architecture covers musl and
glibc distributions alike. The macOS binaries are **not signed or notarised yet**, so Gatekeeper
quarantines one that a browser downloaded — see [troubleshooting](#troubleshooting).

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

## Configuration precedence

Highest first:

1. Command line — `-Dsonar.token=…`, `--sonar-token …`
2. Individual environment variables — `SONAR_TOKEN`, `SONAR_HOST_URL`, `SONAR_REGION`,
   `SONAR_USER_HOME`, and the systematic `SONAR_SCANNER_XXX_YYY` → `sonar.scanner.xxxYyy` mapping
   (`SONAR_SCANNER_PROXY_PORT=3128` → `sonar.scanner.proxyPort=3128`)
3. `SONAR_SCANNER_JSON_PARAMS` — a JSON object of properties (fallback: `SONARQUBE_SCANNER_PARAMS`)
4. `[package.metadata.sonar]` / `[workspace.metadata.sonar]` in `<base dir>/Cargo.toml`
5. `sonar-project.properties` in the base directory — supported so that a project migrating from
   the CLI scanner keeps working; `Cargo.toml` is the recommended place
6. `<sonar.userHome>/sonar-scanner.properties`, where `sonar.userHome` defaults to `~/.sonar` — the
   place for machine-wide settings such as a host URL or a token

Layers 1–3 and 6 are the generic scanner bootstrapping contract, shared with every other Sonar
scanner. Layers 4 and 5 are this bootstrapper's project-level configuration files, which the
contract leaves to each bootstrapper to choose (Maven uses `pom.xml`, the CLI scanner uses
`sonar-project.properties`).

When the same key is given as both `--sonar-token` and `-Dsonar.token=…`, the `-D` form wins.

`--dry-run` prints the origin of every resolved property, which is the quickest way to answer
"where did that value come from?".

## Custom certificates

Behind a TLS-intercepting proxy, or against a server with a private certificate authority, point the
scanner at a PKCS#12 truststore:

```console
$ cargo sonar-scanner -Dsonar.scanner.truststorePath=/path/to/truststore.p12 \
                      -Dsonar.scanner.truststorePassword=...
```

Trust is **widened, not replaced** — the operating system's certificates keep working, so a
truststore holding only your corporate root does not cut off anything else.

If the server asks for a client certificate, supply a keystore holding the private key and its
chain, with `sonar.scanner.keystorePath` and `sonar.scanner.keystorePassword`.

| | Default path | Default password |
| --- | --- | --- |
| Truststore | `<sonar.userHome>/ssl/truststore.p12` | `changeit`, then `sonar` |
| Keystore | `<sonar.userHome>/ssl/keystore.p12` | `changeit`, then `sonar` |

**Both default paths are read whether or not you set a property**, so dropping a `truststore.p12`
into `~/.sonar/ssl` is enough on its own. A file that is missing there is not an error; a file at a
path you *did* configure and that is missing is. When you set a password, only that password is
tried — the defaults are not substituted for it, so a typo reports a password failure rather than
silently opening a differently protected store.

The same four properties reach the scanner engine, which applies them to its own connections, so one
truststore covers the whole analysis.

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

## What an analysis does

1. Asks the server for its version, which is also the first check of the endpoint and the token.
   SonarQube Server must be 10.6 or newer; SonarQube Cloud is never asked.
2. Provisions a JRE for this platform, unless one is named with `sonar.scanner.javaExePath` or
   provisioning is turned off with `sonar.scanner.skipJreProvisioning=true`, in which case `java`
   comes from `JAVA_HOME` or from `PATH`.
3. Downloads the scanner engine the server wants, unless one is named with
   `sonar.scanner.engineJarPath`.
4. Runs `<java> <sonar.scanner.javaOpts> -jar <engine>`, hands the resolved properties to it on
   standard input, and relays its log output.

Both downloads are checksum-verified and cached under `<sonar.userHome>/cache`, so later analyses
on the same machine reuse them. A checksum mismatch is retried once from the metadata call, because
an artefact republished mid-download makes the checksum being compared against the stale one.

## Properties set by the scanner

`sonar.scanner.app` (`cargo`), `sonar.scanner.appVersion` and `sonar.scanner.bootstrapStartTime` are
owned by the bootstrapper; a user-supplied value is ignored with a warning. `sonar.projectBaseDir`
and `sonar.userHome` are defaults you can override.

### Secrets

`sonar.token`, `sonar.login`, `sonar.password` and the proxy, truststore and keystore passwords are
never written to a log stream at any verbosity, and are masked as `******` in the `--dry-run` dump.
They are of course present in the payload handed to the scanner engine, including the one written by
`-Dsonar.scanner.internal.dumpToFile=<path>`.

## Output streams

`INFO`, `WARN`, `DEBUG` and the dry-run dump go to stdout; `ERROR` goes to stderr. The engine's log
records are re-emitted at their own levels, so they are indistinguishable from the bootstrapper's;
anything the engine writes to its standard error is reported as `ERROR`.

The exit code is `0` on success and `1` on a bootstrap failure. Once the engine runs, its exit code
becomes ours. A malformed command line is a usage error and exits `2`.

## Troubleshooting

Start by separating configuration from connectivity. This command resolves the configuration and
makes no network request:

```console
$ cargo sonar-scanner --dry-run
```

It prints the endpoint, base directory, user home, every resolved property, and the origin of each
property. Sensitive values are masked. Add `--verbose` (or set `sonar.verbose=true`) to also show
the loaded files and the resolved properties in the log.

To inspect the resolved scanner-property payload before provisioning or analysis, write it to a
local file:

```console
$ cargo sonar-scanner -Dsonar.scanner.internal.dumpToFile=payload.json
```

The payload contains the real token. The scanner creates the file with owner-only permissions on
Unix; keep it out of source control, CI artifacts, and support requests, and delete it when done.

### Configuration and command-line options

There is no `SONAR_SCANNER_CLI_ARGS` equivalent. Pass scanner properties directly to Cargo:

```console
$ cargo sonar-scanner -Dsonar.projectKey=my-project -Dsonar.scanner.connectTimeout=30
```

For CI, use the corresponding environment variables instead. Named properties such as
`sonar.token` use `SONAR_TOKEN`; any `sonar.scanner.*` property maps to `SONAR_SCANNER_*`, so
`sonar.scanner.connectTimeout` becomes `SONAR_SCANNER_CONNECT_TIMEOUT` and
`sonar.scanner.skipJreProvisioning` becomes `SONAR_SCANNER_SKIP_JRE_PROVISIONING`. Use
`--dry-run` to confirm the final value and its source.

### Authentication and network failures

For a rejected token (HTTP 401), generate or select a token for the target shown by `--dry-run`,
then set `SONAR_TOKEN` again. Do not put it in `Cargo.toml`. A 403 means that the token was
accepted but cannot run analysis for the selected organization or server; check its analysis
permissions and the project and organization properties.

For an unreachable host, first check the resolved host URL and API base URL with `--dry-run`. The
HTTP client uses the platform trust store. If a corporate TLS-inspecting proxy is in use, install
its root certificate in that trust store rather than disabling certificate verification. Standard
proxy environment variables are used unless scanner proxy properties override them:

```console
$ cargo sonar-scanner \
    -Dsonar.scanner.proxyHost=proxy.example.com \
    -Dsonar.scanner.proxyPort=3128
```

`sonar.scanner.proxyUser` and `sonar.scanner.proxyPassword` configure proxy credentials when they
are required. Increase `sonar.scanner.connectTimeout`, `sonar.scanner.socketTimeout`, or
`sonar.scanner.responseTimeout` for slow but healthy connections; all values are seconds, and a
response timeout of `0` means no overall limit.

### Provisioning cache and downloads

When JRE and scanner-engine provisioning is enabled, downloads are stored below
`<sonar.userHome>/cache`, where `sonar.userHome` defaults to `~/.sonar`. The cache is keyed by the
expected SHA-256 checksum, and incomplete or checksum-mismatched downloads are not installed.

The scanner automatically retries a checksum mismatch once, including fresh metadata. If it still
fails, investigate the server, proxy, or TLS interception: failed downloads are discarded and never
installed in the cache. Remove a checksum directory only when a cached artifact is known to have
been modified or corrupted, after stopping concurrent scanner runs. For JRE download failures, use
the same network, TLS, proxy, and timeout checks above before clearing the cache.

### A downloaded macOS binary will not run

```
"cargo-sonar-scanner" cannot be opened because the developer cannot be verified.
```

The macOS archives are not signed or notarised yet, so Gatekeeper quarantines a binary that arrived
through a browser. Either fetch it with `curl`, which does not set the quarantine attribute, or clear
the attribute:

```console
$ xattr -d com.apple.quarantine cargo-sonar-scanner
```

CI is unaffected: runners download with `curl` or an action, not a browser.

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
