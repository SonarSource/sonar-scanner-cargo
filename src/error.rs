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
use std::path::PathBuf;

use thiserror::Error;

/// Every failure the bootstrapper can report. Rendered once, in `main`.
#[derive(Debug, Error)]
pub enum ScannerError {
    #[error("Failed to read {path}: {source}")]
    FileRead {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("Failed to write {path}: {source}")]
    FileWrite {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("{var} is not a valid JSON object of string properties: {message}")]
    InvalidJsonParams { var: String, message: String },

    #[error("Invalid Sonar configuration in {path}: {message}")]
    InvalidManifestConfig { path: PathBuf, message: String },

    #[error("{0}")]
    Cache(#[from] crate::cache::CacheError),

    #[error("{0}")]
    Endpoint(#[from] crate::endpoint::EndpointError),

    #[error("{0}")]
    Http(#[from] crate::http::HttpError),

    #[error("{0}")]
    Version(#[from] crate::version::VersionError),

    #[error("Unable to determine the current directory: {0}")]
    CurrentDir(std::io::Error),

    #[error(
        "Unable to determine the home directory. Set the user home explicitly with -Dsonar.userHome=<dir> \
         or the SONAR_USER_HOME environment variable."
    )]
    NoHomeDirectory,

    #[error("{0}")]
    NotImplemented(String),
}

pub type Result<T> = std::result::Result<T, ScannerError>;
