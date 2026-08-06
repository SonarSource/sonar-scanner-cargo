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
use thiserror::Error;

/// Every failure the bootstrapper can report. Rendered once, in `main`.
#[derive(Debug, Error)]
pub enum ScannerError {
    #[error("Invalid property definition '{0}'. Expected the form -Dkey=value")]
    InvalidDefine(String),

    #[error("{0}")]
    NotImplemented(String),
}

pub type Result<T> = std::result::Result<T, ScannerError>;
