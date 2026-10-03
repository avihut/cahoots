//! A directory inside the user's home, held open, and the reads and writes
//! `install` and `refresh` make in it. A path is looked up again on every
//! use, and a link can be swapped into it in between; a directory handle is
//! not. So the stamp is read, and the file replaced, in the one directory
//! that was judged to be inside the home — and never through a link: a link
//! where a file should be is reported, not followed, and a new file is
//! renamed over the name, so a link swapped in after the check is replaced.

use std::fs::{self, File};
use std::io::{ErrorKind, Read, Write};
use std::os::fd::{AsFd, OwnedFd};
use std::path::{Path, PathBuf};

use nix::errno::Errno;
use nix::fcntl::{AtFlags, OFlag, open, openat, renameat};
use nix::sys::stat::{FileStat, Mode, SFlag, fstat, fstatat, stat};
use nix::unistd::{UnlinkatFlags, unlinkat};

use crate::exit::{Fail, Res};

/// A directory inside the user's home, held open: everything below happens
/// in THIS directory, whatever its path points at afterwards.
pub struct Dir {
    fd: OwnedFd,
    path: PathBuf,
}

/// What is at a name in a `Dir`, never following a link.
#[derive(Debug, PartialEq, Eq)]
pub enum Entry {
    Absent,
    /// A link, a directory, a FIFO — anything that is not a plain file.
    NotRegular,
    /// A plain file's text: "" when it is not UTF-8 or cannot be read, which
    /// no stamp is found in, so it reads as a person's.
    Regular(String),
}

fn is_regular(meta: &FileStat) -> bool {
    SFlag::from_bits_truncate(meta.st_mode) & SFlag::S_IFMT == SFlag::S_IFREG
}

fn same_file(a: &FileStat, b: &FileStat) -> bool {
    (a.st_dev, a.st_ino) == (b.st_dev, b.st_ino)
}

impl Dir {
    /// `Ok(Err(why))` when `dir` resolves outside `home`.
    pub fn open_confined(home: &Path, dir: &Path) -> Res<Result<Dir, String>> {
        if let Some(why) = outside(home, dir)? {
            return Ok(Err(why));
        }
        let canonical = fs::canonicalize(dir).map_err(|error| {
            Fail::internal(format!("cannot resolve {}: {error}", dir.display()))
        })?;
        let fd = open(
            &canonical,
            OFlag::O_RDONLY | OFlag::O_DIRECTORY | OFlag::O_NOFOLLOW | OFlag::O_CLOEXEC,
            Mode::empty(),
        )
        .map_err(|error| Fail::internal(format!("cannot open {}: {error}", dir.display())))?;
        // What was opened is what was judged: the same directory the
        // canonical path names now, and that path still inside the home.
        let held = fstat(&fd)
            .map_err(|error| Fail::internal(format!("cannot read {}: {error}", dir.display())))?;
        let named = stat(&canonical).ok();
        if !named.is_some_and(|named| same_file(&held, &named)) {
            return Ok(Err(format!(
                "{} changed while it was being opened",
                dir.display()
            )));
        }
        if let Some(why) = outside(home, &canonical)? {
            return Ok(Err(why));
        }
        Ok(Ok(Dir {
            fd,
            path: dir.to_path_buf(),
        }))
    }

    pub fn entry(&self, name: &str) -> Res<Entry> {
        match fstatat(self.fd.as_fd(), name, AtFlags::AT_SYMLINK_NOFOLLOW) {
            Err(Errno::ENOENT) => return Ok(Entry::Absent),
            Err(error) => {
                return Err(Fail::internal(format!(
                    "cannot read {}: {error}",
                    self.path.join(name).display()
                )));
            }
            Ok(meta) if !is_regular(&meta) => return Ok(Entry::NotRegular),
            Ok(_) => {}
        }
        // O_NOFOLLOW for a link swapped in since the stat, O_NONBLOCK for a
        // FIFO, and the opened file itself checked again.
        let fd = match openat(
            self.fd.as_fd(),
            name,
            OFlag::O_RDONLY | OFlag::O_NOFOLLOW | OFlag::O_NONBLOCK | OFlag::O_CLOEXEC,
            Mode::empty(),
        ) {
            Ok(fd) => fd,
            Err(Errno::ENOENT) => return Ok(Entry::Absent),
            Err(Errno::ELOOP) => return Ok(Entry::NotRegular),
            Err(_) => return Ok(Entry::Regular(String::new())),
        };
        match fstat(&fd) {
            Ok(meta) if is_regular(&meta) => {}
            _ => return Ok(Entry::NotRegular),
        }
        let mut bytes = Vec::new();
        if File::from(fd).read_to_end(&mut bytes).is_err() {
            return Ok(Entry::Regular(String::new()));
        }
        Ok(Entry::Regular(String::from_utf8(bytes).unwrap_or_default()))
    }

    /// Writes `text` to a new temp file here and renames it over `name`.
    pub fn replace(&self, name: &str, text: &str) -> Res<()> {
        // Never `.md` or `.toml`: a harness loads every one in `agents/`.
        let temp = format!(".{name}.{}.tmp", std::process::id());
        let _ = unlinkat(self.fd.as_fd(), temp.as_str(), UnlinkatFlags::NoRemoveDir);
        let written = openat(
            self.fd.as_fd(),
            temp.as_str(),
            OFlag::O_WRONLY | OFlag::O_CREAT | OFlag::O_EXCL | OFlag::O_NOFOLLOW | OFlag::O_CLOEXEC,
            Mode::from_bits_truncate(0o666),
        )
        .map_err(std::io::Error::from)
        .and_then(|fd| File::from(fd).write_all(text.as_bytes()))
        .and_then(|()| {
            renameat(self.fd.as_fd(), temp.as_str(), self.fd.as_fd(), name)
                .map_err(std::io::Error::from)
        });
        written.map_err(|error| {
            if error.kind() != ErrorKind::AlreadyExists {
                let _ = unlinkat(self.fd.as_fd(), temp.as_str(), UnlinkatFlags::NoRemoveDir);
            }
            Fail::internal(format!(
                "cannot write {}: {error}",
                self.path.join(name).display()
            ))
        })
    }
}

/// What a path holds, read through it — for a directory that resolves
/// outside the home, which is never written, only judged (`install
/// --dry-run`, and the file left alone before the refusal).
pub fn entry_by_path(path: &Path) -> Entry {
    match fs::symlink_metadata(path) {
        Err(_) => Entry::Absent,
        Ok(meta) if !meta.is_file() => Entry::NotRegular,
        Ok(_) => Entry::Regular(fs::read_to_string(path).unwrap_or_default()),
    }
}

/// `outside_home`, said as the reason: "<existing> resolves to <resolved>,
/// outside your home".
fn outside(home: &Path, dir: &Path) -> Res<Option<String>> {
    Ok(outside_home(home, dir)?.map(|(existing, resolved)| {
        format!(
            "{} resolves to {}, outside your home",
            existing.display(),
            resolved.display()
        )
    }))
}

/// Where `dir` — or its nearest ancestor that exists — resolves, when that
/// is outside the user's home: the existing path, and where it led.
pub fn outside_home(home: &Path, dir: &Path) -> Res<Option<(PathBuf, PathBuf)>> {
    let home = fs::canonicalize(home)
        .map_err(|error| Fail::internal(format!("cannot resolve {}: {error}", home.display())))?;
    let existing = dir
        .ancestors()
        .find(|ancestor| ancestor.exists())
        .ok_or_else(|| Fail::internal(format!("{} has no existing ancestor", dir.display())))?;
    let resolved = fs::canonicalize(existing).map_err(|error| {
        Fail::internal(format!("cannot resolve {}: {error}", existing.display()))
    })?;
    Ok((!resolved.starts_with(&home)).then(|| (existing.to_path_buf(), resolved)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;

    struct Place {
        _tmp: tempfile::TempDir,
        home: PathBuf,
        outside: PathBuf,
    }

    fn place() -> Place {
        let tmp = tempfile::tempdir().unwrap();
        let root = fs::canonicalize(tmp.path()).unwrap();
        let (home, outside) = (root.join("home"), root.join("outside"));
        fs::create_dir_all(home.join("agents")).unwrap();
        fs::create_dir_all(&outside).unwrap();
        Place {
            _tmp: tmp,
            home,
            outside,
        }
    }

    fn agents(place: &Place) -> Dir {
        Dir::open_confined(&place.home, &place.home.join("agents"))
            .unwrap()
            .unwrap()
    }

    #[test]
    fn a_link_swapped_in_is_replaced_not_written_through() {
        let place = place();
        let dir = agents(&place);
        let target = place.outside.join("theirs.md");
        fs::write(&target, "cahoots_version: \"1\"\nmine\n").unwrap();
        let name = place.home.join("agents/cahoots-delegate.md");
        symlink(&target, &name).unwrap();

        dir.replace("cahoots-delegate.md", "new text").unwrap();

        assert!(fs::symlink_metadata(&name).unwrap().is_file());
        assert_eq!(fs::read_to_string(&name).unwrap(), "new text");
        assert_eq!(
            fs::read_to_string(&target).unwrap(),
            "cahoots_version: \"1\"\nmine\n"
        );
    }

    #[test]
    fn entry_never_follows_a_link() {
        let place = place();
        let dir = agents(&place);
        let target = place.outside.join("theirs.md");
        fs::write(&target, "cahoots_version: \"1\"\n").unwrap();
        symlink(&target, place.home.join("agents/linked.md")).unwrap();
        symlink(&place.outside, place.home.join("agents/linked-dir")).unwrap();
        fs::write(place.home.join("agents/plain.md"), "text").unwrap();

        assert_eq!(dir.entry("linked.md").unwrap(), Entry::NotRegular);
        assert_eq!(dir.entry("linked-dir").unwrap(), Entry::NotRegular);
        assert_eq!(dir.entry("missing.md").unwrap(), Entry::Absent);
        assert_eq!(
            dir.entry("plain.md").unwrap(),
            Entry::Regular("text".to_string())
        );
    }

    #[test]
    fn a_directory_outside_home_is_not_opened() {
        let place = place();
        let link = place.home.join("elsewhere");
        symlink(&place.outside, &link).unwrap();
        for dir in [link.clone(), link.join("agents"), place.outside.clone()] {
            let why = Dir::open_confined(&place.home, &dir)
                .unwrap()
                .err()
                .unwrap_or_else(|| panic!("{} was opened", dir.display()));
            assert!(why.contains("outside your home"), "{why}");
        }
        assert!(fs::read_dir(&place.outside).unwrap().next().is_none());
    }

    #[test]
    fn no_temp_file_is_left_behind() {
        let place = place();
        let dir = agents(&place);
        dir.replace("a.md", "one").unwrap();
        dir.replace("a.md", "two").unwrap();
        // A directory where the file goes: the rename fails, and the temp
        // file it wrote goes with it.
        fs::create_dir(place.home.join("agents/b.md")).unwrap();
        fs::write(place.home.join("agents/b.md/inside"), "x").unwrap();
        assert!(dir.replace("b.md", "three").is_err());

        let mut left: Vec<String> = fs::read_dir(place.home.join("agents"))
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        left.sort();
        assert_eq!(left, ["a.md", "b.md"]);
        assert_eq!(
            fs::read_to_string(place.home.join("agents/a.md")).unwrap(),
            "two"
        );
    }
}
