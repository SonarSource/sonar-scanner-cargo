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
