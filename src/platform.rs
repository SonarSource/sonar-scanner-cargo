/*
 * SonarScanner for Cargo
 * Copyright (C) SonarSource Sàrl
 * mailto:info AT sonarsource DOT com
 *
 * This program is free software; you can redistribute it and/or
 * modify it under the terms of the GNU Lesser General Public
 * License version 3 as published by the Free Software Foundation.
 *
 * This program is distributed in the hope that it will be useful,
 * but WITHOUT ANY WARRANTY; without even the implied warranty of
 * MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the GNU
 * Lesser General Public License for more details.
 *
 * You should have received a copy of the GNU Lesser General Public License
 * along with this program; if not, write to the Free Software Foundation,
 * Inc., 51 Franklin Street, Fifth Floor, Boston, MA  02110-1301, USA.
 */
//! OS and architecture detection for JRE provisioning.
//!
//! The values are sent raw: the server accepts a broad set of aliases precisely so that
//! bootstrappers do not normalise them. The one exception is Alpine, which the server treats as a
//! distinct OS and which cannot be told apart from other Linux distributions any other way.

use std::path::Path;

use crate::config::{Properties, SCANNER_ARCH, SCANNER_OS};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Platform {
    pub os: String,
    pub arch: String,
}

/// Detect the platform, honouring the `sonar.scanner.os` / `sonar.scanner.arch` escape hatches.
pub fn detect(properties: &Properties) -> Platform {
    Platform {
        os: properties.get_non_blank(SCANNER_OS).map(str::to_string).unwrap_or_else(detect_os),
        arch: properties.get_non_blank(SCANNER_ARCH).map(str::to_string).unwrap_or_else(detect_arch),
    }
}

fn detect_os() -> String {
    if cfg!(target_os = "linux") && is_alpine() {
        return "alpine".to_string();
    }
    std::env::consts::OS.to_string()
}

/// This is the *binary's* architecture, not the CPU's — an x86_64 build under Rosetta reports
/// `x86_64`. `sonar.scanner.arch` is the escape hatch.
fn detect_arch() -> String {
    std::env::consts::ARCH.to_string()
}

fn is_alpine() -> bool {
    ["/etc/os-release", "/usr/lib/os-release"]
        .iter()
        .find_map(|path| std::fs::read_to_string(Path::new(path)).ok())
        .and_then(|contents| distribution_id(&contents))
        .is_some_and(|id| id == "alpine")
}

/// The value of the first `ID=` line of an `os-release` file.
fn distribution_id(contents: &str) -> Option<String> {
    contents.lines().find_map(|line| line.strip_prefix("ID=")).map(|id| id.trim().trim_matches(['"', '\'']).to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn properties(pairs: &[(&str, &str)]) -> Properties {
        pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
    }

    #[test]
    fn detects_the_running_platform() {
        let platform = detect(&properties(&[]));
        assert_eq!(platform.arch, std::env::consts::ARCH);
        assert!(!platform.os.is_empty());
    }

    #[test]
    fn the_properties_override_detection() {
        let platform = detect(&properties(&[(SCANNER_OS, "alpine"), (SCANNER_ARCH, "aarch64")]));
        assert_eq!(platform, Platform { os: "alpine".to_string(), arch: "aarch64".to_string() });
    }

    #[test]
    fn a_blank_override_falls_back_to_detection() {
        let platform = detect(&properties(&[(SCANNER_OS, "  ")]));
        assert_eq!(platform.os, detect_os());
    }

    #[test]
    fn reads_the_distribution_id_from_os_release() {
        assert_eq!(distribution_id("NAME=\"Alpine Linux\"\nID=alpine\n").as_deref(), Some("alpine"));
        assert_eq!(distribution_id("ID=\"ubuntu\"\nID_LIKE=debian\n").as_deref(), Some("ubuntu"));
        assert_eq!(distribution_id("ID=alpine\r\n").as_deref(), Some("alpine"));
        assert_eq!(distribution_id("NAME=Whatever\n"), None);
    }
}
