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
//! The provisioning cache, shared by the JRE and the engine:
//!
//! ```text
//! <sonar.userHome>/cache/<sha256>/<filename>
//! <sonar.userHome>/cache/<sha256>/<filename>_extracted/
//! ```
//!
//! Several scanners run against the same cache at the same time — think of a CI agent building three
//! branches of the same repository — so nothing here may assume it is alone. Every artefact is
//! written to a temporary location beside its destination, verified, and then moved into place with a
//! rename that refuses to overwrite: the loser of a race throws its own copy away and uses the file
//! that is already there. A reader therefore never sees a partial file, whatever any writer is doing.
//!
//! Because the cache is keyed by the checksum, a file that is already in place was verified when it
//! was written and is not hashed again.

use std::ffi::OsStr;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use log::{debug, warn};
use sha2::{Digest as _, Sha256};
use tempfile::{NamedTempFile, TempDir};
use thiserror::Error;

/// The cache lives under the user home, next to nothing else the scanner writes.
const CACHE_DIR: &str = "cache";

/// Appended to the archive name, so the extraction sits beside the archive it came from.
const EXTRACTED_SUFFIX: &str = "_extracted";

/// The length of a SHA-256 checksum in hexadecimal.
const CHECKSUM_LENGTH: usize = 64;

#[derive(Debug, Error)]
pub enum CacheError {
    #[error("Failed to create the cache directory {path}: {source}")]
    CreateDirectory {
        path: PathBuf,
        #[source]
        source: io::Error,
    },

    #[error("Failed to write to the cache directory {path}: {source}")]
    Temporary {
        path: PathBuf,
        #[source]
        source: io::Error,
    },

    #[error("Failed to add {path} to the cache: {source}")]
    Install {
        path: PathBuf,
        #[source]
        source: io::Error,
    },

    #[error(
        "The downloaded {filename} has checksum {actual}, but the server reported {expected}. \
         The download is corrupted, or something in between altered it."
    )]
    ChecksumMismatch { filename: String, expected: String, actual: String },

    #[error("The server reported {filename:?}, which is not a usable file name.")]
    UnusableFilename { filename: String },

    #[error("The server reported {checksum:?} as the checksum of {filename}, which is not a SHA-256 checksum.")]
    UnusableChecksum { filename: String, checksum: String },
}

impl CacheError {
    fn is_checksum_mismatch(&self) -> bool {
        matches!(self, CacheError::ChecksumMismatch { .. })
    }
}

/// Run `attempt` again, once, if what it downloaded did not match the checksum it was given.
///
/// The whole attempt is repeated, metadata call included, rather than just the download: an artefact
/// republished between the metadata call and the download has a new checksum, so the one being
/// compared against is stale and no number of downloads would ever match it.
pub fn retrying_a_checksum_mismatch<T>(
    mut attempt: impl FnMut() -> crate::error::Result<T>,
) -> crate::error::Result<T> {
    match attempt() {
        Err(failure) if is_checksum_mismatch(&failure) => {
            warn!("{failure}");
            warn!("Asking the server again, in case the file was replaced while it was being downloaded");
            attempt()
        }
        result => result,
    }
}

fn is_checksum_mismatch(failure: &crate::error::ScannerError) -> bool {
    matches!(failure, crate::error::ScannerError::Cache(error) if error.is_checksum_mismatch())
}

/// Where an artefact ended up, and whether the cache already had it.
///
/// The engine is told about the hit, as `sonar.scanner.wasJreCacheHit` and
/// `sonar.scanner.wasEngineCacheHit`.
#[derive(Debug, PartialEq, Eq)]
pub struct Cached {
    pub path: PathBuf,
    pub hit: bool,
}

/// The provisioning cache under `<sonar.userHome>/cache`.
pub struct Cache {
    root: PathBuf,
}

/// One artefact, identified by the checksum the server reported for it, present or not.
#[derive(Debug)]
pub struct Entry {
    /// `<root>/<sha256>`, created lazily: a lookup must not litter the cache with empty directories.
    dir: PathBuf,
    path: PathBuf,
    filename: String,
    checksum: String,
}

impl Cache {
    pub fn new(user_home: &Path) -> Self {
        Self { root: user_home.join(CACHE_DIR) }
    }

    /// The entry for `filename` with the given checksum.
    ///
    /// Both halves come off the wire and both become path components, so both are validated here,
    /// where the path is built, rather than trusted because the server is authenticated.
    pub fn entry(&self, filename: &str, checksum: &str) -> Result<Entry, CacheError> {
        if Path::new(filename).file_name() != Some(OsStr::new(filename)) {
            return Err(CacheError::UnusableFilename { filename: filename.to_string() });
        }
        if checksum.len() != CHECKSUM_LENGTH || !checksum.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(CacheError::UnusableChecksum {
                filename: filename.to_string(),
                checksum: checksum.to_string(),
            });
        }
        let dir = self.root.join(checksum);
        Ok(Entry {
            path: dir.join(filename),
            dir,
            filename: filename.to_string(),
            checksum: checksum.to_ascii_lowercase(),
        })
    }
}

impl Entry {
    /// The cached file, written by `fill` when the cache does not have it yet.
    ///
    /// `fill` receives a sink that hashes what it writes, so the download is verified as it streams
    /// and a large artefact is never held in memory.
    pub fn file(&self, fill: impl FnOnce(&mut dyn Write) -> crate::error::Result<()>) -> crate::error::Result<Cached> {
        if self.path.is_file() {
            debug!("Found {} in the cache at {}", self.filename, self.path.display());
            return Ok(Cached { path: self.path.clone(), hit: true });
        }
        self.create_dir()?;

        // In the destination directory, so that installing it is a rename within one filesystem, and
        // so that a crashed scanner leaves its debris where the next one can be seen to clean it up.
        let temporary = NamedTempFile::new_in(&self.dir)
            .map_err(|source| CacheError::Temporary { path: self.dir.clone(), source })?;
        let mut sink = Hashing::new(temporary);
        fill(&mut sink)?;
        let (temporary, actual) = sink.finish();

        // Data before metadata: without this, a crash between the write and the rename could leave a
        // truncated file at a path that is trusted from then on, since a cache hit is never re-hashed.
        temporary.as_file().sync_all().map_err(|source| CacheError::Temporary { path: self.dir.clone(), source })?;

        if actual != self.checksum {
            // Dropping the temporary file removes it, so a bad download is never left behind.
            return Err(CacheError::ChecksumMismatch {
                filename: self.filename.clone(),
                expected: self.checksum.clone(),
                actual,
            }
            .into());
        }
        self.install(temporary)
    }

    /// The directory the cached archive is extracted into, filled by `extract` when it is not there
    /// yet. `extract` is given the archive and the directory to unpack it into.
    pub fn extracted(
        &self,
        extract: impl FnOnce(&Path, &Path) -> crate::error::Result<()>,
    ) -> crate::error::Result<Cached> {
        let target = self.dir.join(format!("{}{EXTRACTED_SUFFIX}", self.filename));
        if target.is_dir() {
            debug!("Found {} extracted in the cache at {}", self.filename, target.display());
            return Ok(Cached { path: target, hit: true });
        }
        self.create_dir()?;

        let temporary =
            TempDir::new_in(&self.dir).map_err(|source| CacheError::Temporary { path: self.dir.clone(), source })?;
        extract(&self.path, temporary.path())?;

        match std::fs::rename(temporary.path(), &target) {
            // The directory moved: there is nothing left for the temporary to remove.
            Ok(()) => {
                let _ = temporary.keep();
                // The JRE that runs the analysis lives in here, so this is the directory a shared
                // cache has to let other users through. What the extraction put inside keeps the
                // modes the archive recorded.
                shared(&target, DIRECTORY_MODE)
                    .map_err(|source| CacheError::Install { path: target.clone(), source })?;
            }
            // Another scanner extracted the same archive first. Its copy is as good as ours.
            Err(_) if target.is_dir() => {
                debug!("Another scanner extracted {} first, keeping {}", self.filename, target.display());
            }
            Err(source) => return Err(CacheError::Install { path: target, source }.into()),
        }
        Ok(Cached { path: target, hit: false })
    }

    fn create_dir(&self) -> Result<(), CacheError> {
        std::fs::create_dir_all(&self.dir)
            .map_err(|source| CacheError::CreateDirectory { path: self.dir.clone(), source })
    }

    fn install(&self, temporary: NamedTempFile) -> crate::error::Result<Cached> {
        match temporary.persist_noclobber(&self.path) {
            Ok(_) => shared(&self.path, FILE_MODE)
                .map_err(|source| CacheError::Install { path: self.path.clone(), source })?,
            // Another scanner installed the same file first. It has the same checksum, so it is the
            // same file, and its copy is already visible to everyone else.
            Err(failure) if self.path.is_file() => {
                debug!("Another scanner cached {} first, keeping {}", self.filename, self.path.display());
                drop(failure);
            }
            Err(failure) => {
                return Err(CacheError::Install { path: self.path.clone(), source: failure.error }.into());
            }
        }
        Ok(Cached { path: self.path.clone(), hit: false })
    }
}

/// Mode of an installed file: readable by everyone, as a cache entry has to be.
#[cfg(unix)]
const FILE_MODE: u32 = 0o644;
/// Mode of an installed directory, which also has to be traversable to be of any use.
#[cfg(unix)]
const DIRECTORY_MODE: u32 = 0o755;
#[cfg(not(unix))]
const FILE_MODE: u32 = 0;
#[cfg(not(unix))]
const DIRECTORY_MODE: u32 = 0;

/// What `tempfile` creates is private to its creator, but a cache can be shared between users — a CI
/// image that ships a warm `SONAR_USER_HOME` is the usual case — so what is installed is opened up.
#[cfg(unix)]
fn shared(path: &Path, mode: u32) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;

    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))
}

#[cfg(not(unix))]
fn shared(_path: &Path, _mode: u32) -> io::Result<()> {
    Ok(())
}

/// A sink that hashes everything on its way to `inner`.
struct Hashing<W> {
    inner: W,
    hasher: Sha256,
}

impl<W: Write> Hashing<W> {
    fn new(inner: W) -> Self {
        Self { inner, hasher: Sha256::new() }
    }

    /// The sink back, and the checksum of everything written to it, in lowercase hexadecimal.
    fn finish(self) -> (W, String) {
        let digest = self.hasher.finalize();
        (self.inner, digest.iter().map(|byte| format!("{byte:02x}")).collect())
    }
}

impl<W: Write> Write for Hashing<W> {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        // Only what was actually written: a short write must not be hashed twice.
        let written = self.inner.write(buffer)?;
        self.hasher.update(&buffer[..written]);
        Ok(written)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::ScannerError;

    /// `sha256sum` of "hello", and of one million 'a' — the classic test vector.
    const HELLO_SHA256: &str = "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824";
    const MILLION_A_SHA256: &str = "cdc76e5c9914fb9281a1c7e284d73e67f1809a48a497200e046d39ccc7112cd0";

    fn user_home() -> TempDir {
        TempDir::with_prefix("cargo-sonar-scanner-cache-").unwrap()
    }

    /// A `fill` that writes `contents`, like a download that succeeds.
    fn writes(contents: &'static str) -> impl FnOnce(&mut dyn Write) -> crate::error::Result<()> {
        move |sink| {
            sink.write_all(contents.as_bytes()).unwrap();
            Ok(())
        }
    }

    fn entries_of(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(dir)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    #[test]
    fn caches_a_file_under_its_checksum() {
        let home = user_home();
        let cache = Cache::new(home.path());

        let cached = cache.entry("hello.txt", HELLO_SHA256).unwrap().file(writes("hello")).unwrap();

        assert_eq!(cached, Cached { path: home.path().join("cache").join(HELLO_SHA256).join("hello.txt"), hit: false });
        assert_eq!(std::fs::read_to_string(&cached.path).unwrap(), "hello");
        assert_eq!(entries_of(cached.path.parent().unwrap()), ["hello.txt"], "no temporary file is left behind");
    }

    #[test]
    fn serves_a_second_request_from_the_cache() {
        let home = user_home();
        let cache = Cache::new(home.path());
        let first = cache.entry("hello.txt", HELLO_SHA256).unwrap().file(writes("hello")).unwrap();

        let second = cache
            .entry("hello.txt", HELLO_SHA256)
            .unwrap()
            .file(|_| unreachable!("a cached file must not be downloaded again"))
            .unwrap();

        assert_eq!(second, Cached { path: first.path, hit: true });
    }

    #[test]
    fn hashes_a_download_written_in_many_chunks() {
        let home = user_home();
        let cache = Cache::new(home.path());

        let cached = cache
            .entry("many.bin", MILLION_A_SHA256)
            .unwrap()
            .file(|sink| {
                let chunk = vec![b'a'; 8 * 1024];
                for _ in 0..122 {
                    sink.write_all(&chunk).unwrap();
                }
                sink.write_all(&vec![b'a'; 1_000_000 - 122 * 8 * 1024]).unwrap();
                Ok(())
            })
            .unwrap();

        assert_eq!(std::fs::metadata(&cached.path).unwrap().len(), 1_000_000);
    }

    #[test]
    fn refuses_a_download_whose_checksum_does_not_match() {
        let home = user_home();
        let cache = Cache::new(home.path());
        let entry = cache.entry("hello.txt", HELLO_SHA256).unwrap();

        let failure = entry.file(writes("goodbye")).unwrap_err();

        assert_eq!(
            failure.to_string(),
            format!(
                "The downloaded hello.txt has checksum \
                 82e35a63ceba37e9646434c5dd412ea577147f1e4a41ccde1614253187e3dbf9, but the server reported \
                 {HELLO_SHA256}. The download is corrupted, or something in between altered it."
            )
        );
        assert!(is_checksum_mismatch(&failure), "the caller has to be able to tell this failure apart: {failure}");
        assert!(!entry.path.exists(), "the corrupted download is not installed");
        assert_eq!(entries_of(&entry.dir), [] as [String; 0], "the corrupted download is not left behind either");
    }

    #[test]
    fn leaves_nothing_behind_when_the_download_fails() {
        let home = user_home();
        let cache = Cache::new(home.path());
        let entry = cache.entry("hello.txt", HELLO_SHA256).unwrap();

        let failure = entry
            .file(|sink| {
                sink.write_all(b"hel").unwrap();
                Err(ScannerError::NotImplemented("the connection dropped".to_string()))
            })
            .unwrap_err();

        assert_eq!(failure.to_string(), "the connection dropped");
        assert!(!entry.path.exists());
        assert_eq!(entries_of(&entry.dir), [] as [String; 0]);
    }

    /// The race the whole design exists for: two scanners download the same artefact at once.
    #[test]
    fn keeps_the_file_another_scanner_installed_first() {
        let home = user_home();
        let cache = Cache::new(home.path());
        let entry = cache.entry("hello.txt", HELLO_SHA256).unwrap();

        let cached = entry
            .file(|sink| {
                sink.write_all(b"hello").unwrap();
                // The other scanner wins the race, between our download and our rename.
                std::fs::write(home.path().join("cache").join(HELLO_SHA256).join("hello.txt"), "hello").unwrap();
                Ok(())
            })
            .unwrap();

        assert_eq!(std::fs::read_to_string(&cached.path).unwrap(), "hello");
        assert_eq!(entries_of(&entry.dir), ["hello.txt"], "the copy that lost the race is removed");
    }

    #[test]
    fn extracts_the_archive_beside_itself() {
        let home = user_home();
        let cache = Cache::new(home.path());
        let entry = cache.entry("jre.tar.gz", HELLO_SHA256).unwrap();
        entry.file(writes("hello")).unwrap();

        let extracted = entry
            .extracted(|archive, into| {
                assert_eq!(archive, entry.path, "the extraction reads the cached archive");
                std::fs::write(into.join("java"), "binary").unwrap();
                Ok(())
            })
            .unwrap();

        assert_eq!(extracted, Cached { path: entry.dir.join("jre.tar.gz_extracted"), hit: false });
        assert_eq!(std::fs::read_to_string(extracted.path.join("java")).unwrap(), "binary");
        assert_eq!(entries_of(&entry.dir), ["jre.tar.gz", "jre.tar.gz_extracted"]);
    }

    #[test]
    fn serves_a_second_extraction_from_the_cache() {
        let home = user_home();
        let cache = Cache::new(home.path());
        let entry = cache.entry("jre.tar.gz", HELLO_SHA256).unwrap();
        entry.file(writes("hello")).unwrap();
        let first = entry
            .extracted(|_, into| {
                std::fs::write(into.join("java"), "binary").unwrap();
                Ok(())
            })
            .unwrap();

        let second = entry.extracted(|_, _| unreachable!("an extracted archive must not be extracted again")).unwrap();

        assert_eq!(second, Cached { path: first.path, hit: true });
    }

    #[test]
    fn leaves_nothing_behind_when_the_extraction_fails() {
        let home = user_home();
        let cache = Cache::new(home.path());
        let entry = cache.entry("jre.tar.gz", HELLO_SHA256).unwrap();
        entry.file(writes("hello")).unwrap();

        let failure = entry
            .extracted(|_, into| {
                std::fs::write(into.join("half-a-jre"), "truncated").unwrap();
                Err(ScannerError::NotImplemented("the archive is not an archive".to_string()))
            })
            .unwrap_err();

        assert_eq!(failure.to_string(), "the archive is not an archive");
        assert_eq!(entries_of(&entry.dir), ["jre.tar.gz"], "a half-extracted directory is never left in the cache");
    }

    #[test]
    fn refuses_a_file_name_that_is_not_a_file_name() {
        let cache = Cache::new(Path::new("/does-not-matter"));

        for filename in ["../evil", "sub/dir", "..", ".", "", "/absolute"] {
            let failure = cache.entry(filename, HELLO_SHA256).unwrap_err();
            assert_eq!(
                failure.to_string(),
                format!("The server reported {filename:?}, which is not a usable file name.")
            );
        }
    }

    #[test]
    fn refuses_a_checksum_that_is_not_a_checksum() {
        let cache = Cache::new(Path::new("/does-not-matter"));

        for checksum in ["", "../..", "abc", &HELLO_SHA256.replace('2', "z")] {
            let failure = cache.entry("hello.txt", checksum).unwrap_err();
            assert_eq!(
                failure.to_string(),
                format!(
                    "The server reported {checksum:?} as the checksum of hello.txt, which is not a SHA-256 checksum."
                )
            );
        }
    }

    /// The checksum is a hexadecimal string either way, and the cache is keyed by what the server
    /// said, so a server that shouts must still find its own entries.
    #[test]
    fn accepts_an_uppercase_checksum() {
        let home = user_home();
        let cache = Cache::new(home.path());
        let upper = HELLO_SHA256.to_uppercase();

        let cached = cache.entry("hello.txt", &upper).unwrap().file(writes("hello")).unwrap();

        assert_eq!(cached.path, home.path().join("cache").join(&upper).join("hello.txt"));
    }

    #[cfg(unix)]
    #[test]
    fn installs_a_file_the_whole_machine_can_read() {
        use std::os::unix::fs::PermissionsExt;

        let home = user_home();
        let cache = Cache::new(home.path());

        let cached = cache.entry("hello.txt", HELLO_SHA256).unwrap().file(writes("hello")).unwrap();

        let mode = std::fs::metadata(&cached.path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o644, "a temporary file is created 0600, which a shared cache cannot use");
    }

    /// The extracted directory, not the archive, is what the JRE is run from, so this is the one a
    /// shared cache cannot do without.
    #[cfg(unix)]
    #[test]
    fn installs_a_directory_the_whole_machine_can_enter() {
        use std::os::unix::fs::PermissionsExt;

        let home = user_home();
        let cache = Cache::new(home.path());
        let entry = cache.entry("jre.tar.gz", HELLO_SHA256).unwrap();
        entry.file(writes("hello")).unwrap();

        let extracted = entry
            .extracted(|_, into| {
                std::fs::write(into.join("java"), "binary").unwrap();
                Ok(())
            })
            .unwrap();

        let mode = std::fs::metadata(&extracted.path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o755, "a temporary directory is created 0700, which another user cannot enter");
    }
}
