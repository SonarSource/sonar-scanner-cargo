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
use std::ffi::OsString;

use clap::Parser;

/// Run SonarQube Server and SonarQube Cloud analysis on a Cargo project.
///
/// All analysis parameters are Sonar properties. They can be set with `-Dkey=value`, with the
/// `--sonar-*` options below, with environment variables, or in `Cargo.toml`.
#[derive(Debug, Parser)]
#[command(
    bin_name = "cargo sonar-scanner",
    name = "cargo sonar-scanner",
    version,
    about,
    long_about = None
)]
pub struct Cli {
    /// Define a Sonar property, repeatable (e.g. -Dsonar.projectKey=my-crate)
    #[arg(short = 'D', long = "define", value_name = "key=value", value_parser = parse_define)]
    pub define: Vec<(String, String)>,

    /// Authentication token (sonar.token)
    #[arg(long, value_name = "TOKEN")]
    pub sonar_token: Option<String>,

    /// SonarQube Server URL (sonar.host.url). Omit to analyse on SonarQube Cloud
    #[arg(long, value_name = "URL")]
    pub sonar_host_url: Option<String>,

    /// SonarQube Cloud region (sonar.region), e.g. `us`
    #[arg(long, value_name = "REGION")]
    pub sonar_region: Option<String>,

    /// SonarQube Cloud organization key (sonar.organization)
    #[arg(long, value_name = "KEY")]
    pub sonar_organization: Option<String>,

    /// Project key (sonar.projectKey)
    #[arg(long, value_name = "KEY")]
    pub sonar_project_key: Option<String>,

    /// Project version (sonar.projectVersion)
    #[arg(long, value_name = "VERSION")]
    pub sonar_project_version: Option<String>,

    /// Base directory of the analysis (sonar.projectBaseDir). Defaults to the current directory
    #[arg(long, value_name = "DIR")]
    pub sonar_project_base_dir: Option<String>,

    /// Scanner home directory holding the cache (sonar.userHome). Defaults to ~/.sonar
    #[arg(long, value_name = "DIR")]
    pub sonar_user_home: Option<String>,

    /// Resolve and print the configuration without contacting any server, then exit
    #[arg(long)]
    pub dry_run: bool,

    /// Enable debug logging (sonar.verbose)
    #[arg(short, long)]
    pub verbose: bool,
}

impl Cli {
    /// Parse an argument vector, stripping the subcommand name Cargo injects.
    ///
    /// Cargo runs external subcommands as `cargo-sonar-scanner sonar-scanner <args…>`; the binary
    /// must behave identically when invoked directly as `cargo-sonar-scanner <args…>`.
    pub fn parse_argv(argv: impl IntoIterator<Item = impl Into<OsString> + Clone>) -> Self {
        Self::parse_from(strip_cargo_subcommand(argv))
    }
}

/// Drop the subcommand name that Cargo passes to an external subcommand.
///
/// clap removes `argv[0]`, the binary name, and nothing else. Cargo invokes external subcommands
/// as `cargo-sonar-scanner sonar-scanner <args…>`, so the subcommand name arrives as `argv[1]` —
/// a Cargo convention that clap has no knowledge of. clap's cookbook does offer a cargo-subcommand
/// pattern built on a wrapper enum, but it makes the `sonar-scanner` argument mandatory, and the
/// binary has to keep working when it is run directly as `cargo-sonar-scanner <args…>`.
fn strip_cargo_subcommand(argv: impl IntoIterator<Item = impl Into<OsString> + Clone>) -> Vec<OsString> {
    let mut argv: Vec<OsString> = argv.into_iter().map(Into::into).collect();
    if argv.len() > 1 && argv[1] == "sonar-scanner" {
        argv.remove(1);
    }
    argv
}

/// `key=value` — an empty value is legal (`-Dsonar.token=`), an empty key is not.
fn parse_define(define: &str) -> Result<(String, String), String> {
    match define.split_once('=') {
        Some((key, value)) if !key.trim().is_empty() => Ok((key.trim().to_string(), value.to_string())),
        _ => Err("expected key=value, for example sonar.projectKey=my-crate".to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_the_subcommand_name_injected_by_cargo() {
        let argv = strip_cargo_subcommand(["cargo-sonar-scanner", "sonar-scanner", "--dry-run"]);
        assert_eq!(argv, ["cargo-sonar-scanner", "--dry-run"]);
    }

    #[test]
    fn leaves_a_direct_invocation_untouched() {
        let argv = strip_cargo_subcommand(["cargo-sonar-scanner", "--dry-run"]);
        assert_eq!(argv, ["cargo-sonar-scanner", "--dry-run"]);
    }

    #[test]
    fn strips_only_the_first_occurrence() {
        let argv = strip_cargo_subcommand(["cargo-sonar-scanner", "sonar-scanner", "-Dx=sonar-scanner"]);
        assert_eq!(argv, ["cargo-sonar-scanner", "-Dx=sonar-scanner"]);
    }

    #[test]
    fn parses_defines_and_named_options() {
        let cli = Cli::parse_argv([
            "cargo-sonar-scanner",
            "sonar-scanner",
            "--sonar-token",
            "from-option",
            "-Dsonar.projectKey=my-crate",
            "-D",
            "sonar.verbose=true",
        ]);
        assert_eq!(cli.sonar_token.as_deref(), Some("from-option"));
        assert_eq!(
            cli.define,
            [
                ("sonar.projectKey".to_string(), "my-crate".to_string()),
                ("sonar.verbose".to_string(), "true".to_string()),
            ]
        );
    }

    #[test]
    fn accepts_an_empty_value_but_rejects_a_missing_key() {
        assert_eq!(parse_define("sonar.token=").unwrap(), ("sonar.token".to_string(), String::new()));
        assert_eq!(parse_define("a=b=c").unwrap(), ("a".to_string(), "b=c".to_string()));
        assert!(parse_define("sonar.token").is_err());
        assert!(parse_define("=value").is_err());
    }

    #[test]
    fn verify_cli() {
        use clap::CommandFactory;
        Cli::command().debug_assert();
    }
}
