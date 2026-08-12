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
//! - **Nothing is written outside the target directory.** The archive is checksum-verified against
//!   what an authenticated server reported, so this is defence in depth rather than the first line
//!   of it: an entry that would land outside is skipped with a warning, not extracted and not fatal.
//!
//! The second one is worth stating precisely, because comparing paths textually is not enough to
//! get it. Once a symlink exists, the depth the kernel resolves a path to is no longer the depth
//! its components suggest: with `a/b` a symlink to `..`, the path `a/b/../..` reads as two levels
//! down and one back up, but actually resolves a level *above* the directory it started in. Three
//! rules together give the property, and each is load-bearing:
//!
//! 1. Every directory an entry needs is created here, one component at a time, and a component that
//!    already exists as a symlink stops the entry. Nothing is ever written *through* a link.
//! 2. A symlink is only created when its target stays inside the directory by component count, and
//!    does not pass through a symlink this extraction has already created — the case that cannot be
//!    judged textually is refused rather than guessed at.
//! 3. The paths of entries themselves are relative and may not climb out, which both archive
//!    readers check for us.
//!
//! Rule 1 is what actually makes the guarantee, and it holds regardless of the other two.

use std::collections::HashSet;
use std::fs::File;
use std::io::{self, BufReader, Read};
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

    let mut symlinks = HashSet::new();
    let mut directories = Vec::new();

    for entry in tar.entries()? {
        let mut entry = entry?;
        let path = entry.path()?.into_owned();
        let is_directory = entry.header().entry_type().is_dir();
        let directory = if is_directory { path.as_path() } else { path.parent().unwrap_or(Path::new("")) };

        // Directories are created here rather than left to `unpack_in`, so that an entry whose path
        // runs through a symlink is stopped before anything is written through it.
        if !prepare_directory(into, directory)? {
            warn!(
                "Skipping {} from {}: it would be extracted outside the target directory",
                path.display(),
                archive.display()
            );
            continue;
        }

        // The directory now exists, and its mode waits until the whole archive is out. `unpack_in`
        // would apply it here, and a JRE records `conf` as read-only before the files inside it.
        if is_directory {
            directories.push((into.join(&path), entry.header().mode()?));
            continue;
        }

        // A symlink is recreated here rather than by `unpack_in`, for two reasons. It does not
        // validate the target, which it writes verbatim; and on Windows it calls `symlink_file`
        // unconditionally, which fails with error 1314 unless the account holds the privilege or
        // the machine is in developer mode — so a `.tar.gz` there would fail on the first link
        // rather than skipping it the way the zip path does.
        if entry.header().entry_type().is_symlink() {
            if let Some(link) = entry.link_name()? {
                restore_symlink(&path, &link, into, archive, &mut symlinks)?;
            }
            continue;
        }

        // `unpack_in` is what refuses to leave `into`; it reports a skipped entry rather than failing.
        if !entry.unpack_in(into)? {
            warn!(
                "Skipping {} from {}: it would be extracted outside the target directory",
                path.display(),
                archive.display()
            );
        }
    }
    apply_directory_modes(directories)
}

fn unzip(archive: &Path, into: &Path) -> Result<(), ArchiveError> {
    let read = |source| ArchiveError::Read { archive: archive.to_path_buf(), source };
    let extract = |source| ArchiveError::Extract { archive: archive.to_path_buf(), source };

    let file = BufReader::new(File::open(archive).map_err(extract)?);
    let mut zip = zip::ZipArchive::new(file).map_err(read)?;
    let mut symlinks = HashSet::new();
    let mut directories = Vec::new();

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
        let directory = if entry.is_dir() { relative.as_path() } else { relative.parent().unwrap_or(Path::new("")) };

        if !prepare_directory(into, directory).map_err(extract)? {
            warn!(
                "Skipping {} from {}: it would be extracted outside the target directory",
                relative.display(),
                archive.display()
            );
            continue;
        }
        if entry.is_dir() {
            if let Some(mode) = entry.unix_mode() {
                directories.push((target, mode));
            }
            continue;
        }
        if entry.is_symlink() {
            let mut destination = String::new();
            entry.read_to_string(&mut destination).map_err(extract)?;
            restore_symlink(&relative, Path::new(&destination), into, archive, &mut symlinks).map_err(extract)?;
            continue;
        }

        let mut file = File::create(&target).map_err(extract)?;
        io::copy(&mut entry, &mut file).map_err(extract)?;
        set_mode(&target, entry.unix_mode()).map_err(extract)?;
    }

    apply_directory_modes(directories).map_err(extract)
}

/// Apply the modes the archive recorded on its directories, once everything is extracted.
///
/// Doing it as the directories are created does not work: a JRE records `conf` as read-only, and a
/// read-only directory refuses `conf/net.properties`. Deepest first, so that a directory left
/// unreadable does not hide the ones below it.
fn apply_directory_modes(mut directories: Vec<(PathBuf, u32)>) -> io::Result<()> {
    directories.sort_by_key(|(path, _)| std::cmp::Reverse(path.components().count()));
    for (path, mode) in directories {
        set_mode(&path, Some(mode))?;
    }
    Ok(())
}

/// Create every directory of `relative` under `into` that does not exist yet.
///
/// Returns `false`, having created nothing further, when a component is already present as a
/// symlink or when `relative` is not a plain relative path. Both mean the entry has to be skipped:
/// writing through a link is how an archive escapes a directory that every textual check on it
/// says it stays inside.
fn prepare_directory(into: &Path, relative: &Path) -> io::Result<bool> {
    let mut current = into.to_path_buf();
    for component in relative.components() {
        match component {
            Component::Normal(part) => current.push(part),
            Component::CurDir => continue,
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => return Ok(false),
        }
        match current.symlink_metadata() {
            Ok(metadata) if metadata.is_symlink() => return Ok(false),
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => std::fs::create_dir(&current)?,
            Err(error) => return Err(error),
        }
    }
    Ok(true)
}

/// Recreate a symlink read from an archive, if its target can be shown to stay inside `into`.
///
/// `created` accumulates the links made so far, by path relative to `into`, and grows as they are.
fn restore_symlink(
    relative: &Path,
    destination: &Path,
    into: &Path,
    archive: &Path,
    created: &mut HashSet<PathBuf>,
) -> io::Result<()> {
    let parent = relative.parent().unwrap_or(Path::new(""));
    if !link_stays_inside(parent, destination, created) {
        warn!(
            "Skipping the symlink {} from {}: it points outside the target directory",
            relative.display(),
            archive.display()
        );
        return Ok(());
    }
    if create_symlink(destination, &into.join(relative))? {
        created.insert(relative.to_path_buf());
    }
    Ok(())
}

/// Whether a relative path stays within the directory it is resolved against.
///
/// For a path that is only read, rather than a link that is created: no extraction has happened
/// yet, so there is no symlink to account for.
pub(crate) fn is_inside(path: &Path) -> bool {
    link_stays_inside(Path::new(""), path, &HashSet::new())
}

/// Whether `link`, resolved against the directory `from` that holds it, stays inside the extraction
/// root — `bin/java -> ../lib/java` does, `bin/x -> ../../etc/passwd` does not.
///
/// A target that runs through one of the symlinks in `created` is reported as outside. It may well
/// not be, but where it lands depends on what that link points at, and walking the path textually
/// is exactly the reasoning that does not survive a symlink. Refusing is the safe answer, and the
/// case does not arise in a JRE.
fn link_stays_inside(from: &Path, link: &Path, created: &HashSet<PathBuf>) -> bool {
    let mut resolved = PathBuf::new();
    for component in from.join(link).components() {
        match component {
            Component::Normal(part) => {
                resolved.push(part);
                if created.contains(&resolved) {
                    return false;
                }
            }
            Component::CurDir => {}
            // `pop` is false only with nothing left to pop, which is the step out of the directory.
            Component::ParentDir => {
                if !resolved.pop() {
                    return false;
                }
            }
            // Anything rooted or prefixed leaves the directory by definition.
            Component::RootDir | Component::Prefix(_) => return false,
        }
    }
    true
}

/// Whether the link was created, so the caller only records the ones that exist.
#[cfg(unix)]
fn create_symlink(destination: &Path, at: &Path) -> io::Result<bool> {
    std::os::unix::fs::symlink(destination, at)?;
    Ok(true)
}

#[cfg(not(unix))]
fn create_symlink(destination: &Path, at: &Path) -> io::Result<bool> {
    warn!("Not creating the symlink {} to {}: symlinks are not supported here", at.display(), destination.display());
    Ok(false)
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
pub(crate) mod tests {
    use super::*;
    use std::io::Write;

    use tempfile::TempDir;
    use zip::write::SimpleFileOptions;

    fn tempdir() -> TempDir {
        TempDir::with_prefix("cargo-sonar-scanner-archive-").unwrap()
    }

    /// A `.tar.gz` of the given entries, as `(path, contents, mode)`.
    pub(crate) fn tar_gz(dir: &Path, name: &str, entries: &[(&str, &str, u32)]) -> PathBuf {
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

    /// A `.tar.gz` of the given symlinks, as `(path, target)`. Only the unix tests build one.
    #[cfg(unix)]
    fn tar_gz_of_symlinks(dir: &Path, name: &str, links: &[(&str, &str)]) -> PathBuf {
        let path = dir.join(name);
        let gzip = flate2::write::GzEncoder::new(File::create(&path).unwrap(), flate2::Compression::fast());
        let mut builder = tar::Builder::new(gzip);
        for (name, target) in links {
            let mut header = tar::Header::new_gnu();
            header.set_entry_type(tar::EntryType::Symlink);
            header.set_size(0);
            header.set_mode(0o777);
            builder.append_link(&mut header, name, target).unwrap();
        }
        builder.into_inner().unwrap().finish().unwrap();
        path
    }

    #[cfg(unix)]
    #[test]
    fn restores_a_symlink_from_a_tar_gz() {
        let dir = tempdir();
        let archive = tar_gz_of_symlinks(dir.path(), "jre.tgz", &[("bin/java", "../lib/java")]);
        let into = dir.path().join("into");
        std::fs::create_dir(&into).unwrap();

        extract(&archive, &into).unwrap();

        let link = std::fs::read_link(into.join("bin/java")).unwrap();
        assert_eq!(link, Path::new("../lib/java"), ".tgz is accepted as well");
    }

    /// `unpack_in` refuses an entry *path* that escapes, but creates a symlink with whatever target
    /// the archive gave it, so this is the tar counterpart of the zip check below.
    #[cfg(unix)]
    #[test]
    fn skips_a_tar_symlink_that_escapes_the_target() {
        let dir = tempdir();
        let archive = tar_gz_of_symlinks(
            dir.path(),
            "jre.tar.gz",
            &[("bin/java", "../lib/java"), ("bin/escape", "../../../../etc/passwd"), ("bin/absolute", "/etc/passwd")],
        );
        let into = dir.path().join("into");
        std::fs::create_dir(&into).unwrap();

        extract(&archive, &into).unwrap();

        assert_eq!(std::fs::read_link(into.join("bin/java")).unwrap(), Path::new("../lib/java"));
        // `symlink_metadata` rather than `exists`, which follows the link and so cannot tell a
        // skipped symlink from one that was created and dangles.
        assert!(into.join("bin/escape").symlink_metadata().is_err(), "a symlink out of the target is not created");
        assert!(into.join("bin/absolute").symlink_metadata().is_err(), "nor is an absolute one");
    }

    /// Counting components says `up/out/..` is one level down from the root. It is not: `up/out`
    /// is a link to the root, so the kernel resolves it a level above.
    #[cfg(unix)]
    #[test]
    fn skips_a_tar_symlink_that_escapes_through_another_symlink() {
        let dir = tempdir();
        let archive = tar_gz_of_symlinks(dir.path(), "jre.tar.gz", &[("up/out", ".."), ("escape", "up/out/..")]);
        let into = dir.path().join("into");
        std::fs::create_dir(&into).unwrap();

        extract(&archive, &into).unwrap();

        assert_eq!(std::fs::read_link(into.join("up/out")).unwrap(), Path::new(".."), "the first link is inside");
        assert!(into.join("escape").symlink_metadata().is_err(), "the one resolving through it is not created");
    }

    /// The guarantee that does not depend on judging a link's target: whatever a link points at,
    /// no entry is written through it.
    #[cfg(unix)]
    #[test]
    fn skips_a_tar_entry_whose_path_runs_through_a_symlink() {
        let dir = tempdir();
        let path = dir.path().join("jre.tar.gz");
        let gzip = flate2::write::GzEncoder::new(File::create(&path).unwrap(), flate2::Compression::fast());
        let mut builder = tar::Builder::new(gzip);
        let mut link = tar::Header::new_gnu();
        link.set_entry_type(tar::EntryType::Symlink);
        link.set_size(0);
        link.set_mode(0o777);
        builder.append_link(&mut link, "up/out", "..").unwrap();
        let mut file = tar::Header::new_gnu();
        file.set_size(4);
        file.set_mode(0o644);
        builder.append_data(&mut file, "up/out/evil", &b"evil"[..]).unwrap();
        builder.into_inner().unwrap().finish().unwrap();
        let into = dir.path().join("into");
        std::fs::create_dir(&into).unwrap();

        extract(&path, &into).unwrap();

        assert!(!into.join("evil").exists(), "the entry is not written through the link");
        assert!(!dir.path().join("evil").exists(), "nor anywhere else");
    }

    /// The tar counterpart of `keeps_the_mode_of_a_zip_directory`, and the format this actually
    /// matters for: a JRE `.tar.gz` records `conf` as read-only before the files inside it.
    #[cfg(unix)]
    #[test]
    fn keeps_the_mode_of_a_tar_directory() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempdir();
        let path = dir.path().join("jre.tar.gz");
        let gzip = flate2::write::GzEncoder::new(File::create(&path).unwrap(), flate2::Compression::fast());
        let mut builder = tar::Builder::new(gzip);
        let mut conf = tar::Header::new_gnu();
        conf.set_entry_type(tar::EntryType::Directory);
        conf.set_size(0);
        conf.set_mode(0o555);
        builder.append_data(&mut conf, "conf", io::empty()).unwrap();
        let mut properties = tar::Header::new_gnu();
        properties.set_size(31);
        properties.set_mode(0o644);
        builder.append_data(&mut properties, "conf/net.properties", &b"java.net.useSystemProxies=true\n"[..]).unwrap();
        builder.into_inner().unwrap().finish().unwrap();
        let into = dir.path().join("into");
        std::fs::create_dir(&into).unwrap();

        extract(&path, &into).unwrap();

        let conf = into.join("conf");
        assert!(conf.join("net.properties").is_file(), "a read-only directory still receives its contents");
        assert_eq!(std::fs::metadata(&conf).unwrap().permissions().mode() & 0o777, 0o555);

        // Leave it removable, so the temporary directory can clean itself up.
        std::fs::set_permissions(&conf, std::fs::Permissions::from_mode(0o755)).unwrap();
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

    /// A JRE records modes on its directories too, and `conf` arriving world-writable because the
    /// umask decided it is not what the archive said.
    #[cfg(unix)]
    #[test]
    fn keeps_the_mode_of_a_zip_directory() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempdir();
        let path = dir.path().join("jre.zip");
        let mut writer = zip::ZipWriter::new(File::create(&path).unwrap());
        writer.add_directory("conf", SimpleFileOptions::default().unix_permissions(0o555)).unwrap();
        writer.start_file("conf/net.properties", SimpleFileOptions::default().unix_permissions(0o644)).unwrap();
        writer.write_all(b"java.net.useSystemProxies=true\n").unwrap();
        writer.finish().unwrap();
        let into = dir.path().join("into");
        std::fs::create_dir(&into).unwrap();

        extract(&path, &into).unwrap();

        // The file first: a directory made read-only before its contents are written would stop
        // them being written at all, which is the reason the modes are applied at the end.
        let conf = into.join("conf");
        assert!(conf.join("net.properties").is_file(), "a read-only directory still receives its contents");
        assert_eq!(std::fs::metadata(&conf).unwrap().permissions().mode() & 0o777, 0o555);

        // Leave it removable, so the temporary directory can clean itself up.
        std::fs::set_permissions(&conf, std::fs::Permissions::from_mode(0o755)).unwrap();
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
    fn recognises_a_link_that_stays_inside() {
        let none = HashSet::new();
        let inside = |from: &str, link: &str| link_stays_inside(Path::new(from), Path::new(link), &none);

        assert!(inside("", "lib/java"));
        assert!(inside("", "./lib/java"));
        assert!(inside("", "lib/../bin/java"));
        assert!(inside("bin", "../lib/java"), "a link is resolved against the directory holding it");
        assert!(!inside("", "../lib/java"));
        assert!(!inside("", "lib/../../java"));
        assert!(!inside("", "/etc/passwd"));
        assert!(!inside("bin", "../../etc/passwd"));
    }

    /// The case a textual check cannot see: `a/b` points at the extraction root, so `a/b/..` is
    /// above it even though counting components says it is one level down.
    #[test]
    fn refuses_a_link_whose_target_runs_through_another_link() {
        let mut created = HashSet::new();
        created.insert(PathBuf::from("a/b"));

        assert!(!link_stays_inside(Path::new(""), Path::new("a/b/.."), &created));
        assert!(!link_stays_inside(Path::new(""), Path::new("a/b/c"), &created));
        assert!(link_stays_inside(Path::new(""), Path::new("a/other"), &created), "only that link is affected");
    }
}
