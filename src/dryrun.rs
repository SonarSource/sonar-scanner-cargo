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
//! `--dry-run`: resolve everything, print it, contact nothing.

use crate::config::{self, Configuration};
use crate::endpoint::Endpoint;
use log::info;

use crate::logging::print;
use crate::platform::Platform;

/// Value substituted for a sensitive property in the dump.
pub const MASK: &str = "******";

pub fn report(config: &Configuration, endpoint: &Endpoint, platform: &Platform) {
    info!("Dry-run mode enabled: resolving the configuration without contacting any server.");
    print("");
    print(format!("Product:        {}", endpoint.product()));
    print(format!("Host URL:       {}", endpoint.host_url));
    print(format!("API base URL:   {}", endpoint.api_base_url));
    print(format!("Platform:       {}/{}", platform.os, platform.arch));
    print(format!("Base directory: {}", config.project_base_dir.display()));
    print(format!("User home:      {}", config.user_home.display()));
    print(format!(
        "Token:          {}",
        if config.properties.get_non_blank(config::TOKEN).is_some() { "set" } else { "not set" }
    ));
    if config.loaded_files.is_empty() {
        print("Config files:   none");
    } else {
        for (index, path) in config.loaded_files.iter().enumerate() {
            let label = if index == 0 { "Config files:  " } else { "               " };
            print(format!("{label} {}", path.display()));
        }
    }

    print("");
    print(format!("Properties ({}):", config.properties.len()));
    for (key, value) in config.properties.iter() {
        print(format!("  {key}={}   [{}]", display_value(key, value), config.origin_of(key).label()));
    }
    print("");
}

/// Sensitive values are masked rather than redacted, so that "set but empty" stays visible.
fn display_value(key: &str, value: &str) -> String {
    if config::is_sensitive(key) && !value.is_empty() { MASK.to_string() } else { value.to_string() }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn masks_sensitive_values_only() {
        assert_eq!(display_value("sonar.token", "squ_secret"), MASK);
        assert_eq!(display_value("sonar.password", "hunter2"), MASK);
        assert_eq!(display_value("sonar.token", ""), "");
        assert_eq!(display_value("sonar.projectKey", "my-crate"), "my-crate");
    }
}
