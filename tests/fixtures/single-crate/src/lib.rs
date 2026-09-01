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

pub fn value() -> u8 {
    1
}

/// Deliberately raises `rust:S1488`, so the end-to-end test can assert that a rule fired rather
/// than only that the file was indexed. That rule is active in the built-in Sonar way profile, and
/// this is the shape it looks for: a local declared and then immediately returned.
///
/// Neither of our own gates reaches it. `cargo clippy --all-targets` covers this crate's targets,
/// and a fixture package is not one of them; this repository's own analysis excludes the fixtures
/// through `sonar.test.exclusions`, so the planted issue is never reported against our project.
pub fn immediately_returned() -> u8 {
    let value = 3;
    value
}
