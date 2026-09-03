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
    Archive(#[from] crate::archive::ArchiveError),

    #[error("{0}")]
    Cache(#[from] crate::cache::CacheError),

    #[error("{0}")]
    Endpoint(#[from] crate::endpoint::EndpointError),

    #[error("{0}")]
    Engine(#[from] crate::engine::EngineError),

    #[error("{0}")]
    Http(#[from] crate::http::HttpError),

    #[error("{0}")]
    Jre(#[from] crate::jre::JreError),

    #[error("{0}")]
    Process(#[from] crate::process::ProcessError),

    #[error("{0}")]
    Version(#[from] crate::version::VersionError),

    #[error("Unable to determine the current directory: {0}")]
    CurrentDir(std::io::Error),

    #[error(
        "Unable to determine the home directory. Set the user home explicitly with -Dsonar.userHome=<dir> \
         or the SONAR_USER_HOME environment variable."
    )]
    NoHomeDirectory,
}

pub type Result<T> = std::result::Result<T, ScannerError>;
