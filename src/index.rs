//! Memory-mapped line index for log files.
//!
//! The file is mapped read-only and scanned in parallel with `memchr`, producing a
//! table of line start offsets plus a one-byte severity level per line. Appends to
//! the file (e.g. a live log) are indexed incrementally.

use std::{
    fs::File,
    ops::Range,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};

use memmap2::Mmap;
use rayon::prelude::*;

/// Bytes handed to a single rayon task while indexing.
const INDEX_CHUNK: usize = 4 << 20;
/// Only the first bytes of a line are inspected when detecting its level.
const LEVEL_SCAN: usize = 192;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u8)]
pub enum Level {
    None = 0,
    Trace = 1,
    Debug = 2,
    Info = 3,
    Warn = 4,
    Error = 5,
}

impl Level {
    pub const COUNT: usize = 6;

    pub fn from_u8(v: u8) -> Level {
        match v {
            1 => Level::Trace,
            2 => Level::Debug,
            3 => Level::Info,
            4 => Level::Warn,
            5 => Level::Error,
            _ => Level::None,
        }
    }

    pub fn bit(self) -> u8 {
        1 << self as u8
    }

    pub fn label(self) -> &'static str {
        match self {
            Level::None => "Other",
            Level::Trace => "Trace",
            Level::Debug => "Debug",
            Level::Info => "Info",
            Level::Warn => "Warn",
            Level::Error => "Error",
        }
    }
}

pub const ALL_LEVELS: u8 = (1 << Level::COUNT) - 1;

fn level_for_word(word: &[u8]) -> Level {
    let mut buf = [0u8; 8];
    if word.len() > buf.len() {
        return Level::None;
    }
    for (dst, src) in buf.iter_mut().zip(word) {
        *dst = src.to_ascii_uppercase();
    }
    match &buf[..word.len()] {
        b"ERROR" | b"ERR" | b"FATAL" | b"CRIT" | b"CRITICAL" | b"PANIC" | b"SEVERE" | b"EMERG"
        | b"ALERT" => Level::Error,
        b"WARN" | b"WARNING" => Level::Warn,
        b"INFO" | b"NOTICE" => Level::Info,
        b"DEBUG" | b"DBG" => Level::Debug,
        b"TRACE" | b"VERBOSE" => Level::Trace,
        _ => Level::None,
    }
}

/// Detects the severity of a log line and the byte range of the token that announced it.
///
/// Upper-case tokens (`ERROR`, `WARN`, ...) are accepted anywhere near the start of the
/// line. Lower/title-case tokens only count when they look structured (`level=error`,
/// `"level":"warn"`, `[info]`), which keeps prose like "no error found" from matching.
pub fn detect_level(line: &[u8]) -> Option<(Level, Range<usize>)> {
    let line = &line[..line.len().min(LEVEL_SCAN)];
    let mut i = 0;
    while i < line.len() {
        if !line[i].is_ascii_alphabetic() {
            i += 1;
            continue;
        }
        let start = i;
        while i < line.len() && line[i].is_ascii_alphabetic() {
            i += 1;
        }
        let word = &line[start..i];
        if !(3..=8).contains(&word.len()) {
            continue;
        }
        let level = level_for_word(word);
        if level == Level::None {
            continue;
        }
        let upper = word.iter().all(u8::is_ascii_uppercase);
        let structured = start > 0 && matches!(line[start - 1], b'[' | b'=' | b'"' | b'<' | b'|');
        if upper || structured {
            return Some((level, start..i));
        }
    }
    None
}

/// An immutable-ish snapshot of an indexed file. Cheap to share via `Arc`; appends
/// go through `Arc::make_mut`, which only copies when a background job holds a
/// reference at the same moment.
#[derive(Clone)]
pub struct LogIndex {
    pub path: PathBuf,
    map: Option<Arc<Mmap>>,
    /// Number of bytes covered by the index.
    pub len: u64,
    /// Byte offset of the first byte of every line.
    starts: Vec<u64>,
    /// `Level as u8` for every line.
    levels: Vec<u8>,
    pub level_counts: [usize; Level::COUNT],
}

pub struct Delta {
    map: Option<Arc<Mmap>>,
    len: u64,
    /// Lines from this index onwards are replaced.
    from_line: usize,
    starts: Vec<u64>,
    levels: Vec<u8>,
}

impl Delta {
    pub fn first_line(&self) -> usize {
        self.from_line
    }
}

impl LogIndex {
    pub fn open(path: &Path, progress: &AtomicU64) -> std::io::Result<LogIndex> {
        let file = File::open(path)?;
        let len = file.metadata()?.len();
        let map = map_file(&file, len)?;
        let data: &[u8] = map.as_deref().map(|m| &m[..]).unwrap_or(&[]);
        let (starts, levels) = index_bytes(data, 0, progress);
        let mut level_counts = [0; Level::COUNT];
        count_levels(&levels, &mut level_counts);
        Ok(LogIndex {
            path: path.to_path_buf(),
            map,
            len,
            starts,
            levels,
            level_counts,
        })
    }

    pub fn line_count(&self) -> usize {
        self.starts.len()
    }

    pub fn data(&self) -> &[u8] {
        match &self.map {
            Some(m) => &m[..self.len as usize],
            None => &[],
        }
    }

    pub fn levels(&self) -> &[u8] {
        &self.levels
    }

    pub fn starts(&self) -> &[u64] {
        &self.starts
    }

    pub fn level(&self, line: usize) -> Level {
        Level::from_u8(self.levels[line])
    }

    /// Byte range of a line including its terminating newline, if any.
    pub fn raw_range(&self, line: usize) -> Range<usize> {
        let start = self.starts[line] as usize;
        let end = self
            .starts
            .get(line + 1)
            .map_or(self.len as usize, |&e| e as usize);
        start..end
    }

    /// Line contents without the trailing `\n` / `\r\n`.
    pub fn line(&self, line: usize) -> &[u8] {
        let mut bytes = &self.data()[self.raw_range(line)];
        if let [rest @ .., b'\n'] = bytes {
            bytes = rest;
        }
        if let [rest @ .., b'\r'] = bytes {
            bytes = rest;
        }
        bytes
    }

    fn ends_with_newline(&self) -> bool {
        self.data().last().is_none_or(|&b| b == b'\n')
    }

    /// Indexes bytes appended since this snapshot was taken. Returns `Ok(None)` when the
    /// file is unchanged and `Err` when it shrank (truncated / rotated) and must be reopened.
    pub fn scan_append(&self) -> std::io::Result<Option<Delta>> {
        let file = File::open(&self.path)?;
        let len = file.metadata()?.len();
        if len == self.len {
            return Ok(None);
        }
        if len < self.len {
            return Err(std::io::Error::other("file truncated"));
        }
        let map = map_file(&file, len)?;
        let data: &[u8] = map.as_deref().map(|m| &m[..]).unwrap_or(&[]);
        // A trailing line without a newline may have been extended, so re-index it.
        let (from_line, from_byte) = if self.ends_with_newline() {
            (self.starts.len(), self.len as usize)
        } else {
            let last = self.starts.len() - 1;
            (last, self.starts[last] as usize)
        };
        let (starts, levels) = index_bytes(data, from_byte, &AtomicU64::new(0));
        Ok(Some(Delta {
            map,
            len,
            from_line,
            starts,
            levels,
        }))
    }

    pub fn apply(&mut self, delta: Delta) {
        let mut removed = [0; Level::COUNT];
        count_levels(&self.levels[delta.from_line..], &mut removed);
        for (count, removed) in self.level_counts.iter_mut().zip(removed) {
            *count -= removed;
        }
        self.starts.truncate(delta.from_line);
        self.levels.truncate(delta.from_line);
        count_levels(&delta.levels, &mut self.level_counts);
        self.starts.extend_from_slice(&delta.starts);
        self.levels.extend_from_slice(&delta.levels);
        self.map = delta.map;
        self.len = delta.len;
    }
}

fn map_file(file: &File, len: u64) -> std::io::Result<Option<Arc<Mmap>>> {
    if len == 0 {
        return Ok(None);
    }
    // SAFETY: the mapping is read-only. Log files are normally append-only; truncation
    // while mapped is detected by `scan_append` and triggers a reopen.
    let map = unsafe { memmap2::MmapOptions::new().len(len as usize).map(file)? };
    #[cfg(unix)]
    let _ = map.advise(memmap2::Advice::Sequential);
    Ok(Some(Arc::new(map)))
}

fn count_levels(levels: &[u8], counts: &mut [usize; Level::COUNT]) {
    for &l in levels {
        counts[l as usize] += 1;
    }
}

/// Finds all line starts at or after `from` and classifies each line's level.
fn index_bytes(data: &[u8], from: usize, progress: &AtomicU64) -> (Vec<u64>, Vec<u8>) {
    if from >= data.len() {
        return (Vec::new(), Vec::new());
    }
    let chunks: Vec<Range<usize>> = (from..data.len())
        .step_by(INDEX_CHUNK)
        .map(|s| s..(s + INDEX_CHUNK).min(data.len()))
        .collect();

    let parts: Vec<(Vec<u64>, Vec<u8>)> = chunks
        .into_par_iter()
        .map(|chunk| {
            let mut starts = Vec::with_capacity(chunk.len() / 64);
            if chunk.start == from {
                starts.push(from as u64);
            }
            for nl in memchr::memchr_iter(b'\n', &data[chunk.clone()]) {
                let next = chunk.start + nl + 1;
                if next < data.len() {
                    starts.push(next as u64);
                }
            }
            let levels = starts
                .iter()
                .map(|&s| {
                    let s = s as usize;
                    let head = &data[s..(s + LEVEL_SCAN).min(data.len())];
                    let head = memchr::memchr(b'\n', head).map_or(head, |e| &head[..e]);
                    detect_level(head).map_or(0, |(l, _)| l as u8)
                })
                .collect();
            progress.fetch_add(chunk.len() as u64, Ordering::Relaxed);
            (starts, levels)
        })
        .collect();

    let total: usize = parts.iter().map(|(s, _)| s.len()).sum();
    let mut starts = Vec::with_capacity(total);
    let mut levels = Vec::with_capacity(total);
    for (s, l) in parts {
        starts.extend_from_slice(&s);
        levels.extend_from_slice(&l);
    }
    (starts, levels)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn detects_levels() {
        let lvl = |s: &str| detect_level(s.as_bytes()).map(|(l, _)| l);
        assert_eq!(lvl("2024-01-01 ERROR boom"), Some(Level::Error));
        assert_eq!(lvl("[warn] disk"), Some(Level::Warn));
        assert_eq!(lvl(r#"{"level":"info","msg":"x"}"#), Some(Level::Info));
        assert_eq!(lvl("level=debug msg=x"), Some(Level::Debug));
        assert_eq!(lvl("no error here"), None);
        assert_eq!(lvl("ERRORS happen"), None);
    }

    #[test]
    fn indexes_and_appends() {
        let dir = std::env::temp_dir().join(format!("logship-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("a.log");
        std::fs::write(&path, "one\r\nWARN two\nthr").unwrap();

        let mut idx = LogIndex::open(&path, &AtomicU64::new(0)).unwrap();
        assert_eq!(idx.line_count(), 3);
        assert_eq!(idx.line(0), b"one");
        assert_eq!(idx.line(1), b"WARN two");
        assert_eq!(idx.line(2), b"thr");
        assert_eq!(idx.level_counts[Level::Warn as usize], 1);

        let mut f = std::fs::OpenOptions::new().append(true).open(&path).unwrap();
        f.write_all(b"ee ERROR\nfour\n").unwrap();
        let delta = idx.scan_append().unwrap().unwrap();
        assert_eq!(delta.first_line(), 2);
        idx.apply(delta);
        assert_eq!(idx.line_count(), 4);
        assert_eq!(idx.line(2), b"three ERROR");
        assert_eq!(idx.line(3), b"four");
        assert_eq!(idx.level_counts[Level::Error as usize], 1);
        assert!(idx.scan_append().unwrap().is_none());

        std::fs::remove_dir_all(&dir).unwrap();
    }
}
