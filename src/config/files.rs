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
//! Reading `.properties` files, in the `java.util.Properties` dialect the other scanners accept.

use std::path::Path;

use super::Properties;
use crate::error::{Result, ScannerError};

/// Load a properties file, or `Ok(None)` if it does not exist.
pub fn load_if_present(path: &Path) -> Result<Option<Properties>> {
    match std::fs::read(path) {
        Ok(bytes) => Ok(Some(parse(&String::from_utf8_lossy(&bytes)))),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(source) => Err(ScannerError::FileRead { path: path.to_path_buf(), source }),
    }
}

/// Parse the `java.util.Properties` text format.
///
/// Supported: `#`/`!` comments, `=`, `:` and whitespace separators, trailing-backslash line
/// continuation, and the `\n \r \t \f \\ \= \: \uXXXX` escapes.
pub fn parse(contents: &str) -> Properties {
    let mut properties = Properties::new();
    let mut logical = String::new();

    for raw_line in contents.lines() {
        // Leading whitespace is insignificant, on a first line as on a continuation line.
        let line = raw_line.trim_start();
        if logical.is_empty() && (line.is_empty() || line.starts_with('#') || line.starts_with('!')) {
            continue;
        }
        if has_trailing_continuation(line) {
            logical.push_str(&line[..line.len() - 1]);
            continue;
        }
        logical.push_str(line);
        if let Some((key, value)) = parse_entry(&logical) {
            properties.set(key, value);
        }
        logical.clear();
    }
    // A continuation on the very last line: keep what we have rather than dropping it.
    if !logical.is_empty()
        && let Some((key, value)) = parse_entry(&logical)
    {
        properties.set(key, value);
    }
    properties
}

/// A line continues if it ends with an odd number of backslashes.
fn has_trailing_continuation(line: &str) -> bool {
    line.chars().rev().take_while(|c| *c == '\\').count() % 2 == 1
}

fn parse_entry(line: &str) -> Option<(String, String)> {
    let chars: Vec<char> = line.chars().collect();
    let (key, key_end) = scan_key(&chars);
    if key.is_empty() {
        return None;
    }
    let value: String = chars[skip_separator(&chars, key_end)..].iter().collect();
    // Trailing whitespace is significant: `java.util.Properties` strips it before the value but
    // keeps it after, and `key=value\ ` is a deliberate trailing space. `str::lines` has already
    // removed the line terminator, including the `\r` of a CRLF file.
    Some((key, unescape(&value)))
}

/// The key runs to the first unescaped separator or whitespace. Returns it with the offset of the
/// character that ended it.
fn scan_key(chars: &[char]) -> (String, usize) {
    let mut key = String::new();
    let mut index = 0;
    while index < chars.len() {
        let c = chars[index];
        if c == '\\' && index + 1 < chars.len() {
            key.push(unescape_char(chars[index + 1]));
            index += 2;
            continue;
        }
        if c == '=' || c == ':' || c.is_whitespace() {
            break;
        }
        key.push(c);
        index += 1;
    }
    (key, index)
}

/// Skip an optional `=` or `:` separator and any whitespace around it.
fn skip_separator(chars: &[char], mut index: usize) -> usize {
    while index < chars.len() && chars[index].is_whitespace() {
        index += 1;
    }
    if index < chars.len() && (chars[index] == '=' || chars[index] == ':') {
        index += 1;
        while index < chars.len() && chars[index].is_whitespace() {
            index += 1;
        }
    }
    index
}

fn unescape(value: &str) -> String {
    let mut chars = value.chars().peekable();
    let mut unescaped = String::with_capacity(value.len());
    while let Some(c) = chars.next() {
        if c != '\\' {
            unescaped.push(c);
            continue;
        }
        match chars.next() {
            Some('u') => {
                let hex: String = (0..4).filter_map(|_| chars.next()).collect();
                match u32::from_str_radix(&hex, 16).ok().and_then(char::from_u32) {
                    Some(decoded) => unescaped.push(decoded),
                    None => {
                        unescaped.push_str("\\u");
                        unescaped.push_str(&hex);
                    }
                }
            }
            Some(escaped) => unescaped.push(unescape_char(escaped)),
            None => unescaped.push('\\'),
        }
    }
    unescaped
}

fn unescape_char(c: char) -> char {
    match c {
        'n' => '\n',
        'r' => '\r',
        't' => '\t',
        'f' => '\u{000c}',
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_the_common_shapes() {
        let properties = parse(
            "# a comment\n\
             ! another comment\n\
             \n\
             sonar.projectKey=my-crate\n\
             sonar.projectName = My Crate\n\
             sonar.host.url:https://sq.example.com\n\
             sonar.sources sources-value\n",
        );
        assert_eq!(properties.get("sonar.projectKey"), Some("my-crate"));
        assert_eq!(properties.get("sonar.projectName"), Some("My Crate"));
        assert_eq!(properties.get("sonar.host.url"), Some("https://sq.example.com"));
        assert_eq!(properties.get("sonar.sources"), Some("sources-value"));
    }

    #[test]
    fn joins_continued_lines() {
        let properties = parse("sonar.exclusions=target/**,\\\n  vendor/**\n");
        assert_eq!(properties.get("sonar.exclusions"), Some("target/**,vendor/**"));
    }

    #[test]
    fn keeps_a_windows_path_with_an_even_backslash_count() {
        let properties = parse(r"sonar.projectBaseDir=C:\\projects\\demo");
        assert_eq!(properties.get("sonar.projectBaseDir"), Some(r"C:\projects\demo"));
    }

    #[test]
    fn decodes_escapes() {
        let properties = parse(r"key=a\tb\nc\u00e9");
        assert_eq!(properties.get("key"), Some("a\tb\nc\u{e9}"));
    }

    #[test]
    fn keeps_trailing_whitespace_in_a_value() {
        // `java.util.Properties` skips whitespace before the value and preserves it after.
        let properties = parse("sonar.projectName=  My Crate   \n");
        assert_eq!(properties.get("sonar.projectName"), Some("My Crate   "));
    }

    #[test]
    fn keeps_an_escaped_trailing_space() {
        let properties = parse(r"key=value\ ");
        assert_eq!(properties.get("key"), Some("value "));
    }

    #[test]
    fn strips_the_carriage_return_of_a_crlf_file() {
        let properties = parse("sonar.projectKey=my-crate\r\n");
        assert_eq!(properties.get("sonar.projectKey"), Some("my-crate"));
    }

    #[test]
    fn accepts_an_empty_value() {
        let properties = parse("sonar.token=\n");
        assert_eq!(properties.get("sonar.token"), Some(""));
    }

    #[test]
    fn a_missing_file_is_not_an_error() {
        let missing = std::env::temp_dir().join("cargo-sonar-scanner-does-not-exist.properties");
        assert!(load_if_present(&missing).unwrap().is_none());
    }
}
