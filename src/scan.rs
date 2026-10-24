//! Finds active transcripts and reads only their recent tail.

use std::fs::{self, File};
use std::io::{self, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use crate::parse::parse_ts;

const CHUNK: u64 = 256 * 1024;
/// Session dirs hold `subagents/`; nothing worth reading sits deeper.
const MAX_DEPTH: usize = 4;

/// Lists `*.jsonl` files under `root` modified at or after `since`.
pub fn discover(root: &Path, since: SystemTime) -> io::Result<Vec<PathBuf>> {
    let mut out = Vec::new();
    walk(root, since, 0, &mut out)?;
    Ok(out)
}

fn walk(dir: &Path, since: SystemTime, depth: usize, out: &mut Vec<PathBuf>) -> io::Result<()> {
    // Entries can vanish or turn unreadable mid-walk; skip them, not the walk.
    for entry in fs::read_dir(dir)?.flatten() {
        let Ok(ft) = entry.file_type() else { continue };
        let path = entry.path();
        if ft.is_dir() && depth < MAX_DEPTH {
            let _ = walk(&path, since, depth + 1, out);
        } else if ft.is_file()
            && path.extension().is_some_and(|e| e == "jsonl")
            && entry
                .metadata()
                .and_then(|m| m.modified())
                .is_ok_and(|t| t >= since)
        {
            out.push(path);
        }
    }
    Ok(())
}

/// Reads complete lines from the end of `path` back to the first chunk that
/// holds a timestamp older than `cutoff_ms` (or to the start of the file).
pub fn read_tail(path: &Path, cutoff_ms: i64) -> io::Result<Vec<u8>> {
    let mut f = File::open(path)?;
    let len = f.metadata()?.len();
    let mut pos = len;
    let mut buf = Vec::new();
    while pos > 0 {
        let n = CHUNK.min(pos);
        pos -= n;
        let mut chunk = vec![0; n as usize];
        f.seek(SeekFrom::Start(pos))?;
        f.read_exact(&mut chunk)?;
        let old_enough = oldest_ts(&chunk).is_some_and(|t| t < cutoff_ms);
        chunk.extend_from_slice(&buf);
        buf = chunk;
        if old_enough {
            break;
        }
    }
    if pos > 0 {
        // Drop the partial first line.
        let start = buf
            .iter()
            .position(|&b| b == b'\n')
            .map_or(buf.len(), |i| i + 1);
        buf.drain(..start);
    }
    Ok(buf)
}

/// Oldest `"timestamp":"…"` value in a raw chunk. Inside JSON strings the
/// quotes would be escaped, so this only matches real keys.
fn oldest_ts(chunk: &[u8]) -> Option<i64> {
    const KEY: &[u8] = b"\"timestamp\":\"";
    let mut oldest = None;
    let mut i = 0;
    while let Some(off) = find(&chunk[i..], KEY) {
        let s = i + off + KEY.len();
        let Some(len) = chunk[s..].iter().take(40).position(|&b| b == b'"') else {
            break;
        };
        if let Some(t) = std::str::from_utf8(&chunk[s..s + len])
            .ok()
            .and_then(parse_ts)
        {
            oldest = Some(oldest.map_or(t, |o: i64| o.min(t)));
        }
        i = s + len;
    }
    oldest
}

fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::time::Duration;

    fn tmpdir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("cc-tps-scan-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn discover_filters_by_extension_and_mtime() {
        let d = tmpdir("discover");
        let sub = d.join("proj/sess/subagents");
        fs::create_dir_all(&sub).unwrap();
        for p in [
            "proj/a.jsonl",
            "proj/old.jsonl",
            "proj/notes.txt",
            "proj/sess/subagents/agent-1.jsonl",
        ] {
            File::create(d.join(p)).unwrap();
        }
        let old = SystemTime::now() - Duration::from_secs(3600);
        File::options()
            .write(true)
            .open(d.join("proj/old.jsonl"))
            .unwrap()
            .set_modified(old)
            .unwrap();
        let mut got = discover(&d, SystemTime::now() - Duration::from_secs(60)).unwrap();
        got.sort();
        let want = vec![
            d.join("proj/a.jsonl"),
            d.join("proj/sess/subagents/agent-1.jsonl"),
        ];
        assert_eq!(got, want);
        fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn read_tail_stops_at_old_chunk_and_keeps_whole_lines() {
        let d = tmpdir("tail");
        let p = d.join("t.jsonl");
        let mut f = File::create(&p).unwrap();
        let pad = "x".repeat(1000);
        // About 600 KB of old lines, then 600 KB of new ones.
        for i in 0..600 {
            writeln!(
                f,
                r#"{{"timestamp":"2026-01-01T00:00:00Z","n":{i},"p":"{pad}"}}"#
            )
            .unwrap();
        }
        for i in 0..600 {
            writeln!(
                f,
                r#"{{"timestamp":"2026-01-01T01:00:00Z","n":{i},"p":"{pad}"}}"#
            )
            .unwrap();
        }
        drop(f);
        let cutoff = parse_ts("2026-01-01T00:30:00Z").unwrap();
        let buf = read_tail(&p, cutoff).unwrap();
        let total = fs::metadata(&p).unwrap().len() as usize;
        assert!(buf.len() < total, "read the whole file");
        let text = std::str::from_utf8(&buf).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        assert!(lines.iter().all(|l| l.starts_with('{') && l.ends_with('}')));
        assert_eq!(lines.iter().filter(|l| l.contains("01:00:00")).count(), 600);
        // A cutoff before every line reads the whole file.
        let all = read_tail(&p, 0).unwrap();
        assert_eq!(all.len(), total);
        fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn oldest_ts_ignores_escaped_keys() {
        let chunk =
            br#"{"timestamp":"2026-01-01T00:00:05Z","c":"{\"timestamp\":\"2020-01-01T00:00:00Z\"}"}
{"timestamp":"2026-01-01T00:00:03Z"}"#;
        assert_eq!(oldest_ts(chunk), parse_ts("2026-01-01T00:00:03Z"));
        assert_eq!(oldest_ts(b"no keys"), None);
    }
}
