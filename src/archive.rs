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
//! Extraction of the provisioned JRE archive: `.tar.gz` on Linux and macOS, `.zip` on Windows.
//!
//! The format is taken from the file name the server reported, because that is the only thing that
//! describes it — the metadata carries no media type.
//!
//! Two properties matter beyond "the files appear":
//!
//! - **Permissions survive on unix.** A JRE whose `bin/java` is not executable is useless, and a
//!   `.tar.gz` is exactly the format that carries the bit.
//! - **No entry escapes the target directory.** The archive is checksum-verified against what an
//!   authenticated server reported, so this is defence in depth rather than the first line of it: an
//!   entry that points outside is skipped with a warning, not extracted and not fatal.

use std::fs::File;
use std::io::{self, BufReader};
use std::path::{Component, Path, PathBuf};

use log::{debug, warn};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum ArchiveError {
    #[error("Cannot extract {filename}: only .tar.gz and .zip archives are supported.")]
    UnsupportedFormat { filename: String },

    #[error("Failed to extract {archive}: {source}")]
    Extract {
        archive: PathBuf,
        #[source]
        source: io::Error,
    },

    #[error("Failed to read the archive {archive}: {source}")]
    Read {
        archive: PathBuf,
        #[source]
        source: zip::result::ZipError,
    },
}

/// Extract `archive` into `into`, which must exist and should be empty.
pub fn extract(archive: &Path, into: &Path) -> Result<(), ArchiveError> {
    let filename = archive.file_name().unwrap_or_default().to_string_lossy();
    let lowercase = filename.to_ascii_lowercase();
    debug!("Extracting {} into {}", archive.display(), into.display());

    if lowercase.ends_with(".tar.gz") || lowercase.ends_with(".tgz") {
        untar(archive, into).map_err(|source| ArchiveError::Extract { archive: archive.to_path_buf(), source })
    } else if lowercase.ends_with(".zip") {
        unzip(archive, into)
    } else {
        Err(ArchiveError::UnsupportedFormat { filename: filename.into_owned() })
    }
}

fn untar(archive: &Path, into: &Path) -> io::Result<()> {
    let file = BufReader::new(File::open(archive)?);
    // `MultiGzDecoder` rather than `GzDecoder`: a tarball compressed by pigz is several concatenated
    // gzip members, and reading only the first one would silently extract a truncated JRE.
    let mut tar = tar::Archive::new(flate2::read::MultiGzDecoder::new(file));
    tar.set_preserve_permissions(true);
    tar.set_overwrite(true);

    for entry in tar.entries()? {
        let mut entry = entry?;
        let path = entry.path()?.into_owned();
        // `unpack_in` is what refuses to leave `into`; it reports a skipped entry rather than failing.
        if !entry.unpack_in(into)? {
            warn!(
                "Skipping {} from {}: it would be extracted outside the target directory",
                path.display(),
                archive.display()
            );
        }
    }
    Ok(())
}

fn unzip(archive: &Path, into: &Path) -> Result<(), ArchiveError> {
    let read = |source| ArchiveError::Read { archive: archive.to_path_buf(), source };
    let extract = |source| ArchiveError::Extract { archive: archive.to_path_buf(), source };

    let file = BufReader::new(File::open(archive).map_err(extract)?);
    let mut zip = zip::ZipArchive::new(file).map_err(read)?;

    for index in 0..zip.len() {
        let mut entry = zip.by_index(index).map_err(read)?;
        // `None` for anything that is absolute, walks up, or is not representable as a path.
        let Some(relative) = entry.enclosed_name() else {
            warn!(
                "Skipping {} from {}: it would be extracted outside the target directory",
                entry.name(),
                archive.display()
            );
            continue;
        };
        let target = into.join(&relative);

        if entry.is_dir() {
            std::fs::create_dir_all(&target).map_err(extract)?;
            continue;
        }
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent).map_err(extract)?;
        }
        if entry.is_symlink() {
            symlink(&mut entry, &relative, &target, archive).map_err(extract)?;
            continue;
        }

        let mut file = File::create(&target).map_err(extract)?;
        io::copy(&mut entry, &mut file).map_err(extract)?;
        set_mode(&target, entry.unix_mode()).map_err(extract)?;
    }
    Ok(())
}

/// Recreate a symlink stored in a zip, whose body is the link target.
///
/// A JRE archive for Windows does not contain any, and Windows cannot create one without a
/// privilege, so this exists for the case of an unusual archive on unix rather than as a hot path.
fn symlink(entry: &mut impl io::Read, relative: &Path, target: &Path, archive: &Path) -> io::Result<()> {
    let mut destination = String::new();
    entry.read_to_string(&mut destination)?;

    // The link is resolved against its own directory, so that is where the check has to start:
    // `bin/java -> ../lib/java` stays inside, `bin/x -> ../../etc/passwd` does not.
    let resolved = relative.parent().unwrap_or(Path::new("")).join(&destination);
    if !is_inside(&resolved) {
        warn!(
            "Skipping the symlink {} from {}: it points outside the target directory",
            target.display(),
            archive.display()
        );
        return Ok(());
    }
    create_symlink(&destination, target)
}

/// Whether a relative path stays within the directory it is resolved against.
fn is_inside(path: &Path) -> bool {
    let mut depth: i32 = 0;
    for component in path.components() {
        match component {
            Component::Normal(_) => depth += 1,
            Component::CurDir => {}
            Component::ParentDir => depth -= 1,
            // Anything rooted or prefixed leaves the directory by definition.
            Component::RootDir | Component::Prefix(_) => return false,
        }
        if depth < 0 {
            return false;
        }
    }
    true
}

#[cfg(unix)]
fn create_symlink(destination: &str, at: &Path) -> io::Result<()> {
    std::os::unix::fs::symlink(destination, at)
}

#[cfg(not(unix))]
fn create_symlink(destination: &str, at: &Path) -> io::Result<()> {
    warn!("Not creating the symlink {} to {destination}: symlinks are not supported here", at.display());
    Ok(())
}

/// Apply the mode a zip entry recorded. Windows has nothing to apply it to.
#[cfg(unix)]
fn set_mode(path: &Path, mode: Option<u32>) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;

    match mode {
        Some(mode) => std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)),
        None => Ok(()),
    }
}

#[cfg(not(unix))]
fn set_mode(_path: &Path, _mode: Option<u32>) -> io::Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    use tempfile::TempDir;
    use zip::write::SimpleFileOptions;

    fn tempdir() -> TempDir {
        TempDir::with_prefix("cargo-sonar-scanner-archive-").unwrap()
    }

    /// A `.tar.gz` of the given entries, as `(path, contents, mode)`.
    fn tar_gz(dir: &Path, name: &str, entries: &[(&str, &str, u32)]) -> PathBuf {
        let path = dir.join(name);
        let gzip = flate2::write::GzEncoder::new(File::create(&path).unwrap(), flate2::Compression::fast());
        let mut builder = tar::Builder::new(gzip);
        for (name, contents, mode) in entries {
            let mut header = tar::Header::new_gnu();
            // The name is written into the header directly: `set_path` refuses a path with `..`, and
            // one of these tests is about what happens when an archive contains exactly that.
            let gnu = header.as_gnu_mut().unwrap();
            gnu.name[..name.len()].copy_from_slice(name.as_bytes());
            header.set_size(contents.len() as u64);
            header.set_mode(*mode);
            header.set_cksum();
            builder.append(&header, contents.as_bytes()).unwrap();
        }
        builder.into_inner().unwrap().finish().unwrap();
        path
    }

    fn zip_of(dir: &Path, name: &str, entries: &[(&str, &str, u32)]) -> PathBuf {
        let path = dir.join(name);
        let mut writer = zip::ZipWriter::new(File::create(&path).unwrap());
        for (name, contents, mode) in entries {
            writer.start_file(*name, SimpleFileOptions::default().unix_permissions(*mode)).unwrap();
            writer.write_all(contents.as_bytes()).unwrap();
        }
        writer.finish().unwrap();
        path
    }

    #[test]
    fn extracts_a_tar_gz() {
        let dir = tempdir();
        let archive = tar_gz(
            dir.path(),
            "jre.tar.gz",
            &[("jre/bin/java", "#!/bin/sh\n", 0o755), ("jre/release", "JAVA=17\n", 0o644)],
        );
        let into = dir.path().join("into");
        std::fs::create_dir(&into).unwrap();

        extract(&archive, &into).unwrap();

        assert_eq!(std::fs::read_to_string(into.join("jre/bin/java")).unwrap(), "#!/bin/sh\n");
        assert_eq!(std::fs::read_to_string(into.join("jre/release")).unwrap(), "JAVA=17\n");
    }

    /// The reason the JRE is not simply copied file by file: `bin/java` has to stay executable.
    #[cfg(unix)]
    #[test]
    fn keeps_the_executable_bit_of_a_tar_gz() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempdir();
        let archive =
            tar_gz(dir.path(), "jre.tar.gz", &[("bin/java", "#!/bin/sh\n", 0o755), ("release", "JAVA=17\n", 0o644)]);
        let into = dir.path().join("into");
        std::fs::create_dir(&into).unwrap();

        extract(&archive, &into).unwrap();

        let mode = |path: &str| std::fs::metadata(into.join(path)).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode("bin/java"), 0o755);
        assert_eq!(mode("release"), 0o644);
    }

    #[cfg(unix)]
    #[test]
    fn restores_a_symlink_from_a_tar_gz() {
        let dir = tempdir();
        let path = dir.path().join("jre.tgz");
        let gzip = flate2::write::GzEncoder::new(File::create(&path).unwrap(), flate2::Compression::fast());
        let mut builder = tar::Builder::new(gzip);
        let mut header = tar::Header::new_gnu();
        header.set_entry_type(tar::EntryType::Symlink);
        header.set_size(0);
        header.set_mode(0o777);
        builder.append_link(&mut header, "bin/java", "../lib/java").unwrap();
        builder.into_inner().unwrap().finish().unwrap();
        let into = dir.path().join("into");
        std::fs::create_dir(&into).unwrap();

        extract(&path, &into).unwrap();

        let link = std::fs::read_link(into.join("bin/java")).unwrap();
        assert_eq!(link, Path::new("../lib/java"), ".tgz is accepted as well");
    }

    #[test]
    fn skips_a_tar_entry_that_escapes_the_target() {
        let dir = tempdir();
        let archive =
            tar_gz(dir.path(), "jre.tar.gz", &[("../escaped", "evil", 0o644), ("release", "JAVA=17\n", 0o644)]);
        let into = dir.path().join("into");
        std::fs::create_dir(&into).unwrap();

        extract(&archive, &into).unwrap();

        assert!(!dir.path().join("escaped").exists(), "the entry is not written outside the target directory");
        assert!(into.join("release").is_file(), "the rest of the archive is still extracted");
    }

    #[test]
    fn extracts_a_zip() {
        let dir = tempdir();
        let archive =
            zip_of(dir.path(), "jre.zip", &[("jre/bin/java.exe", "MZ", 0o755), ("jre/release", "JAVA=17\n", 0o644)]);
        let into = dir.path().join("into");
        std::fs::create_dir(&into).unwrap();

        extract(&archive, &into).unwrap();

        assert_eq!(std::fs::read_to_string(into.join("jre/bin/java.exe")).unwrap(), "MZ");
        assert_eq!(std::fs::read_to_string(into.join("jre/release")).unwrap(), "JAVA=17\n");
    }

    #[cfg(unix)]
    #[test]
    fn keeps_the_executable_bit_of_a_zip() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempdir();
        let archive = zip_of(dir.path(), "jre.zip", &[("bin/java", "#!/bin/sh\n", 0o755)]);
        let into = dir.path().join("into");
        std::fs::create_dir(&into).unwrap();

        extract(&archive, &into).unwrap();

        let mode = std::fs::metadata(into.join("bin/java")).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o755);
    }

    #[test]
    fn skips_a_zip_entry_that_escapes_the_target() {
        let dir = tempdir();
        let archive = zip_of(dir.path(), "jre.zip", &[("../escaped", "evil", 0o644), ("release", "JAVA=17\n", 0o644)]);
        let into = dir.path().join("into");
        std::fs::create_dir(&into).unwrap();

        extract(&archive, &into).unwrap();

        assert!(!dir.path().join("escaped").exists());
        assert!(into.join("release").is_file());
    }

    #[cfg(unix)]
    #[test]
    fn restores_a_symlink_from_a_zip() {
        let dir = tempdir();
        let path = dir.path().join("jre.zip");
        let mut writer = zip::ZipWriter::new(File::create(&path).unwrap());
        writer.add_symlink("bin/java", "../lib/java", SimpleFileOptions::default()).unwrap();
        writer.add_symlink("bin/escape", "../../../../etc/passwd", SimpleFileOptions::default()).unwrap();
        writer.finish().unwrap();
        let into = dir.path().join("into");
        std::fs::create_dir(&into).unwrap();

        extract(&path, &into).unwrap();

        assert_eq!(std::fs::read_link(into.join("bin/java")).unwrap(), Path::new("../lib/java"));
        assert!(!into.join("bin/escape").exists(), "a symlink out of the target directory is skipped");
    }

    #[test]
    fn refuses_a_format_it_cannot_read() {
        let dir = tempdir();
        let archive = dir.path().join("jre.tar.bz2");
        std::fs::write(&archive, "irrelevant").unwrap();

        let failure = extract(&archive, dir.path()).unwrap_err();

        assert_eq!(failure.to_string(), "Cannot extract jre.tar.bz2: only .tar.gz and .zip archives are supported.");
    }

    #[test]
    fn reports_an_archive_that_is_not_an_archive() {
        let dir = tempdir();
        let archive = dir.path().join("jre.zip");
        std::fs::write(&archive, "this is not a zip file").unwrap();

        let failure = extract(&archive, dir.path()).unwrap_err();

        assert!(failure.to_string().starts_with("Failed to read the archive "), "{failure}");
    }

    #[test]
    fn recognises_a_path_that_stays_inside() {
        assert!(is_inside(Path::new("lib/java")));
        assert!(is_inside(Path::new("./lib/java")));
        assert!(is_inside(Path::new("lib/../bin/java")));
        assert!(!is_inside(Path::new("../lib/java")));
        assert!(!is_inside(Path::new("lib/../../java")));
        assert!(!is_inside(Path::new("/etc/passwd")));
    }
}
