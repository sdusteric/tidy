//! A read-only reader of Git's index file (`.git/index`), so the project guard can tell
//! whether Git tracks anything inside a build output folder without running `git`.
//!
//! The format is the one Git documents in `gitformat-index`: a 12-byte header (`DIRC`, the
//! version 2, 3 or 4, the entry count), then the entries in path order. Each entry has 40
//! bytes of stat data, the object name (20 bytes for SHA-1, 32 for SHA-256), 16 bits of flags
//! (and 16 more when the extended bit is set, version 3 and up), then the path. Versions 2 and
//! 3 end the path with NULs that pad the entry to a multiple of 8 bytes; version 4 starts the
//! path with a number of bytes to drop from the end of the previous path and ends it with one
//! NUL. Extensions and the checksum that follow the entries are not read.
//!
//! Only the paths are read. Anything unexpected is an error, and the caller treats an error
//! as "Git may track files here".

use std::io;
use std::path::{Path, PathBuf};

/// The stat fields at the start of every entry: ctime, mtime (seconds and nanoseconds each),
/// dev, ino, mode, uid, gid and size, 32 bits each.
const STAT_BYTES: usize = 40;
const FLAG_EXTENDED: u16 = 0x4000;

/// Why an index could not be read.
#[derive(Debug, PartialEq, Eq)]
pub enum IndexError {
    /// Not a Git index, or a version Tidy does not know.
    Format(&'static str),
    /// The file ends inside an entry.
    Truncated,
}

impl std::fmt::Display for IndexError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            IndexError::Format(why) => write!(f, "not a Git index Tidy can read: {why}"),
            IndexError::Truncated => write!(f, "the Git index ends early"),
        }
    }
}

/// The paths Git tracks, as stored in the index (relative to the work tree, `/` separated).
pub fn tracked_paths(bytes: &[u8]) -> Result<Vec<Vec<u8>>, IndexError> {
    if bytes.len() < 12 || &bytes[..4] != b"DIRC" {
        return Err(IndexError::Format("no DIRC header"));
    }
    let version = be32(&bytes[4..8]);
    if !(2..=4).contains(&version) {
        return Err(IndexError::Format("unknown version"));
    }
    let count = be32(&bytes[8..12]) as usize;
    // SHA-1 first; a SHA-256 repository has 32-byte object names.
    match parse_entries(bytes, version, count, 20) {
        Ok(paths) => Ok(paths),
        Err(first) => parse_entries(bytes, version, count, 32).map_err(|_| first),
    }
}

fn parse_entries(
    bytes: &[u8],
    version: u32,
    count: usize,
    hash: usize,
) -> Result<Vec<Vec<u8>>, IndexError> {
    let mut at = 12usize;
    let mut paths: Vec<Vec<u8>> = Vec::with_capacity(count.min(1 << 20));
    let mut previous: Vec<u8> = Vec::new();
    for _ in 0..count {
        let start = at;
        let flags_at = at + STAT_BYTES + hash;
        let flags = bytes
            .get(flags_at..flags_at + 2)
            .map(|b| u16::from_be_bytes([b[0], b[1]]))
            .ok_or(IndexError::Truncated)?;
        at = flags_at + 2;
        if flags & FLAG_EXTENDED != 0 {
            if version < 3 {
                return Err(IndexError::Format("extended flag in a version 2 index"));
            }
            at += 2;
        }
        let path = if version == 4 {
            let (strip, used) = varint(bytes.get(at..).ok_or(IndexError::Truncated)?)?;
            at += used;
            let rest = bytes.get(at..).ok_or(IndexError::Truncated)?;
            let end = rest
                .iter()
                .position(|b| *b == 0)
                .ok_or(IndexError::Truncated)?;
            if strip > previous.len() {
                return Err(IndexError::Format(
                    "a path prefix longer than the last path",
                ));
            }
            let mut p = previous[..previous.len() - strip].to_vec();
            p.extend_from_slice(&rest[..end]);
            at += end + 1;
            p
        } else {
            let rest = bytes.get(at..).ok_or(IndexError::Truncated)?;
            let end = rest
                .iter()
                .position(|b| *b == 0)
                .ok_or(IndexError::Truncated)?;
            let p = rest[..end].to_vec();
            // The entry, padded with 1 to 8 NULs to a multiple of 8 bytes.
            let unpadded = at + end - start;
            at = start + ((unpadded + 8) & !7);
            if at > bytes.len() {
                return Err(IndexError::Truncated);
            }
            p
        };
        if path.is_empty() {
            return Err(IndexError::Format("an empty path"));
        }
        previous.clone_from(&path);
        paths.push(path);
    }
    // What follows the entries is extensions (four letters and a size) and the checksum.
    if bytes.len() < at + hash {
        return Err(IndexError::Truncated);
    }
    Ok(paths)
}

/// Git's offset varint: 7 bits a byte, high bit set on every byte but the last, and one added
/// to the value before each shift.
fn varint(bytes: &[u8]) -> Result<(usize, usize), IndexError> {
    let mut used = 0usize;
    let mut c = *bytes.first().ok_or(IndexError::Truncated)?;
    used += 1;
    let mut val = usize::from(c & 0x7f);
    while c & 0x80 != 0 {
        c = *bytes.get(used).ok_or(IndexError::Truncated)?;
        used += 1;
        val = val
            .checked_add(1)
            .and_then(|v| v.checked_mul(128))
            .and_then(|v| v.checked_add(usize::from(c & 0x7f)))
            .ok_or(IndexError::Format("a path prefix that overflows"))?;
    }
    Ok((val, used))
}

fn be32(b: &[u8]) -> u32 {
    u32::from_be_bytes([b[0], b[1], b[2], b[3]])
}

/// The index file of the repository whose work tree is `repo`: `<repo>/.git/index`, or for a
/// `.git` file (a linked work tree or a submodule) the `index` in the folder it names.
pub fn index_of(repo: &Path) -> io::Result<PathBuf> {
    let dot_git = repo.join(".git");
    let meta = dot_git.symlink_metadata()?;
    if meta.is_dir() {
        return Ok(dot_git.join("index"));
    }
    if !meta.is_file() {
        return Err(io::Error::other(".git is neither a folder nor a file"));
    }
    let text = std::fs::read_to_string(&dot_git)?;
    let target = text
        .lines()
        .find_map(|l| l.strip_prefix("gitdir:"))
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .ok_or_else(|| io::Error::other(".git names no gitdir"))?;
    let dir = Path::new(target);
    let dir = if dir.is_absolute() {
        dir.to_path_buf()
    } else {
        repo.join(dir)
    };
    Ok(dir.join("index"))
}

/// Whether Git tracks `rel` (a folder relative to the work tree, `/` separated) or anything
/// inside it.
pub fn tracks_inside(paths: &[Vec<u8>], rel: &str) -> bool {
    let rel = rel.trim_end_matches('/').as_bytes();
    paths.iter().any(|p| {
        p.as_slice() == rel || (p.len() > rel.len() && p.starts_with(rel) && p[rel.len()] == b'/')
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A version 2 or 3 index with these paths and SHA-1 names, written the way Git does.
    fn index_v2(paths: &[&str], version: u32) -> Vec<u8> {
        let mut b = b"DIRC".to_vec();
        b.extend_from_slice(&version.to_be_bytes());
        b.extend_from_slice(&(paths.len() as u32).to_be_bytes());
        for p in paths {
            let start = b.len();
            b.extend_from_slice(&[0u8; STAT_BYTES]);
            b.extend_from_slice(&[0xabu8; 20]);
            b.extend_from_slice(&(p.len().min(0xfff) as u16).to_be_bytes());
            b.extend_from_slice(p.as_bytes());
            let unpadded = b.len() - start;
            let total = (unpadded + 8) & !7;
            b.resize(start + total, 0);
        }
        b.extend_from_slice(&[0u8; 20]);
        b
    }

    #[test]
    fn a_version_2_index_lists_its_paths() {
        let b = index_v2(&["Cargo.toml", "dist/keep.js", "src/main.rs"], 2);
        let paths = tracked_paths(&b).unwrap();
        assert_eq!(
            paths,
            [&b"Cargo.toml"[..], b"dist/keep.js", b"src/main.rs"].map(|p| p.to_vec())
        );
        assert!(tracks_inside(&paths, "dist"));
        assert!(tracks_inside(&paths, "src/"));
        assert!(!tracks_inside(&paths, "target"));
        // A name that only starts the same is not inside.
        assert!(!tracks_inside(&paths, "Cargo"));
        assert!(!tracks_inside(&paths, "dis"));
        // Version 3 without extended entries reads the same.
        assert_eq!(
            tracked_paths(&index_v2(&["a"], 3)).unwrap(),
            [b"a".to_vec()]
        );
    }

    #[test]
    fn broken_indexes_are_errors_and_never_panic() {
        assert!(tracked_paths(b"").is_err());
        assert!(tracked_paths(b"DIRX\0\0\0\x02\0\0\0\0").is_err());
        assert!(tracked_paths(b"DIRC\0\0\0\x09\0\0\0\0").is_err());
        let b = index_v2(&["one/two.txt", "three"], 2);
        for n in 0..b.len() - 20 {
            assert!(tracked_paths(&b[..n]).is_err(), "{n}");
        }
        // More entries promised than present.
        let mut more = b.clone();
        more[11] = 9;
        assert!(tracked_paths(&more).is_err());
    }

    #[test]
    fn offset_varints_decode_like_git() {
        assert_eq!(varint(&[0x05]).unwrap(), (5, 1));
        // 0x80 0x00 is (0 + 1) * 128 + 0.
        assert_eq!(varint(&[0x80, 0x00]).unwrap(), (128, 2));
        assert_eq!(varint(&[0x81, 0x01]).unwrap(), (2 * 128 + 1, 2));
        assert!(varint(&[0x80]).is_err());
    }

    fn git(dir: &Path, args: &[&str]) -> bool {
        std::process::Command::new("/usr/bin/git")
            .args(args)
            .current_dir(dir)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("HOME", dir)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .is_ok_and(|s| s.success())
    }

    /// Indexes written by Git itself, in versions 2 and 4, inside a temp folder. Skipped when
    /// git is not installed (it needs the Command Line Tools).
    #[test]
    fn indexes_written_by_git_read_back() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path();
        if !git(repo, &["init", "-q"]) {
            eprintln!("git is not available; skipped");
            return;
        }
        for rel in [
            "src/lib.rs",
            "target/deploy/app.so",
            "dist/a/b/c.js",
            "README.md",
        ] {
            let p = repo.join(rel);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(&p, rel).unwrap();
        }
        assert!(git(repo, &["add", "src", "dist", "README.md"]));
        for version in ["2", "4"] {
            assert!(git(repo, &["update-index", "--index-version", version]));
            let index = index_of(repo).unwrap();
            let paths = tracked_paths(&std::fs::read(&index).unwrap()).unwrap();
            let names: Vec<String> = paths
                .iter()
                .map(|p| String::from_utf8_lossy(p).into_owned())
                .collect();
            assert_eq!(
                names,
                ["README.md", "dist/a/b/c.js", "src/lib.rs"],
                "v{version}"
            );
            assert!(tracks_inside(&paths, "dist"));
            assert!(tracks_inside(&paths, "dist/a"));
            assert!(!tracks_inside(&paths, "target"));
        }
    }

    #[test]
    fn a_dot_git_file_points_at_its_index() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("wt");
        std::fs::create_dir_all(&repo).unwrap();
        std::fs::write(repo.join(".git"), "gitdir: ../main/.git/worktrees/wt\n").unwrap();
        assert_eq!(
            index_of(&repo).unwrap(),
            repo.join("../main/.git/worktrees/wt/index")
        );
        std::fs::write(repo.join(".git"), "nothing here\n").unwrap();
        assert!(index_of(&repo).is_err());
        let plain = tmp.path().join("plain");
        std::fs::create_dir_all(plain.join(".git")).unwrap();
        assert_eq!(index_of(&plain).unwrap(), plain.join(".git/index"));
        assert!(index_of(&tmp.path().join("none")).is_err());
    }
}
