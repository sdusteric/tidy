//! Small file primitives: atomic whole-file writes (settings, saved scans) and durable
//! line appends (history.jsonl).

use std::io::{self, Write};
use std::path::Path;

use serde::Serialize;
use serde::de::DeserializeOwned;

/// Writes `bytes` to `file` through a temp file in the same directory: write, fsync, rename,
/// then fsync the directory. Readers see the old or the new content, never a partial file.
pub fn write_atomic(file: &Path, bytes: &[u8]) -> io::Result<()> {
    let dir = file
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    std::fs::create_dir_all(dir)?;
    let mut tmp_name = file.file_name().unwrap_or_default().to_os_string();
    tmp_name.push(".tmp");
    let tmp = dir.join(tmp_name);
    let result = (|| {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
        drop(f);
        std::fs::rename(&tmp, file)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
        return result;
    }
    if let Ok(d) = std::fs::File::open(dir) {
        let _ = d.sync_all();
    }
    Ok(())
}

/// Serializes `value` as pretty JSON and writes it with [`write_atomic`].
pub fn write_json_atomic<T: Serialize>(file: &Path, value: &T) -> io::Result<()> {
    let json = serde_json::to_vec_pretty(value).map_err(io::Error::other)?;
    write_atomic(file, &json)
}

/// Reads a JSON file. A missing file gives `Ok(None)`; a file that does not parse is an
/// `InvalidData` error.
pub fn read_json<T: DeserializeOwned>(file: &Path) -> io::Result<Option<T>> {
    let bytes = match std::fs::read(file) {
        Ok(b) => b,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e),
    };
    serde_json::from_slice(&bytes)
        .map(Some)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
}

/// Appends one line to `file` with `O_APPEND`, then fsyncs it. A newline is added, and `line`
/// must not contain one (that would split the record). The parent folder is created first.
///
/// One `write` call carries the whole line, so two appenders never interleave inside a line.
pub fn append_line(file: &Path, line: &str) -> io::Result<()> {
    if line.contains('\n') {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "a line to append must not contain a newline",
        ));
    }
    if let Some(dir) = file.parent().filter(|d| !d.as_os_str().is_empty()) {
        std::fs::create_dir_all(dir)?;
    }
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(file)?;
    let mut buf = Vec::with_capacity(line.len() + 1);
    buf.extend_from_slice(line.as_bytes());
    buf.push(b'\n');
    f.write_all(&buf)?;
    f.sync_all()
}

/// Serializes `value` as one compact JSON line and appends it with [`append_line`].
pub fn append_json_line<T: Serialize>(file: &Path, value: &T) -> io::Result<()> {
    let line = serde_json::to_string(value).map_err(io::Error::other)?;
    append_line(file, &line)
}

/// Reads a JSON-lines file. A missing file gives an empty list. Blank lines are ignored, and a
/// line that does not parse is skipped and logged, never fatal.
pub fn read_json_lines<T: DeserializeOwned>(file: &Path) -> io::Result<Vec<T>> {
    let text = match std::fs::read(file) {
        Ok(b) => String::from_utf8_lossy(&b).into_owned(),
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e),
    };
    let mut out = Vec::new();
    for (n, line) in text.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        match serde_json::from_str(line) {
            Ok(v) => out.push(v),
            Err(e) => tracing::warn!("{}: skipping line {}: {e}", file.display(), n + 1),
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn write_atomic_replaces_and_leaves_no_temp_file() {
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("a/b/x.json");
        write_atomic(&file, b"one").unwrap();
        write_atomic(&file, b"two").unwrap();
        assert_eq!(std::fs::read(&file).unwrap(), b"two");
        let names: Vec<_> = std::fs::read_dir(file.parent().unwrap())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, ["x.json"]);
    }

    #[test]
    fn write_atomic_failure_keeps_the_old_file() {
        let tmp = tempfile::tempdir().unwrap();
        // The target is a directory, so the rename fails.
        let file = tmp.path().join("dir");
        std::fs::create_dir(&file).unwrap();
        std::fs::write(file.join("inner"), b"keep").unwrap();
        assert!(write_atomic(&file, b"new").is_err());
        assert_eq!(std::fs::read(file.join("inner")).unwrap(), b"keep");
        assert!(!tmp.path().join("dir.tmp").exists());
    }

    #[test]
    fn json_round_trip_and_missing() {
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("v.json");
        assert_eq!(read_json::<Vec<u32>>(&file).unwrap(), None);
        write_json_atomic(&file, &vec![1u32, 2, 3]).unwrap();
        assert_eq!(read_json::<Vec<u32>>(&file).unwrap(), Some(vec![1, 2, 3]));
        std::fs::write(&file, b"{oops").unwrap();
        let err = read_json::<Vec<u32>>(&file).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
    }

    #[test]
    fn append_and_read_lines_skip_bad_ones() {
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("data/h.jsonl");
        assert!(read_json_lines::<u32>(&file).unwrap().is_empty());
        append_json_line(&file, &1u32).unwrap();
        append_json_line(&file, &2u32).unwrap();
        // A torn or foreign line in the middle.
        let mut f = std::fs::OpenOptions::new()
            .append(true)
            .open(&file)
            .unwrap();
        f.write_all(b"{\"broken\n\n").unwrap();
        drop(f);
        append_json_line(&file, &3u32).unwrap();
        assert_eq!(read_json_lines::<u32>(&file).unwrap(), [1, 2, 3]);
        let text = std::fs::read_to_string(&file).unwrap();
        assert!(text.ends_with("3\n"));
    }

    #[test]
    fn append_rejects_embedded_newlines() {
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("h.jsonl");
        let err = append_line(&file, "a\nb").unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
        assert!(!file.exists());
    }

    #[test]
    fn appends_from_threads_never_interleave() {
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("h.jsonl");
        let handles: Vec<_> = (0..8)
            .map(|t| {
                let file = file.clone();
                std::thread::spawn(move || {
                    for i in 0..25 {
                        append_line(
                            &file,
                            &format!("{{\"t\":{t},\"i\":{i},\"pad\":\"{}\"}}", "x".repeat(200)),
                        )
                        .unwrap();
                    }
                })
            })
            .collect();
        for h in handles {
            h.join().unwrap();
        }
        let rows: Vec<serde_json::Value> = read_json_lines(&file).unwrap();
        assert_eq!(rows.len(), 200);
        let lines = std::fs::read_to_string(&file).unwrap().lines().count();
        assert_eq!(lines, 200);
    }
}
