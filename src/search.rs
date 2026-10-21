//! Parallel line filtering over a [`LogIndex`].
//!
//! Text queries run the regex over whole multi-megabyte slices of the mapped file and
//! map hits back to lines, which avoids per-line call overhead and lets the regex
//! engine's literal prefilters (memchr / Teddy) skip most of the input. A line
//! matches when the regex matches within that line alone, like grep.

use std::{
    ops::Range,
    sync::atomic::{AtomicU64, Ordering},
};

use rayon::prelude::*;
use regex::bytes::{Regex, RegexBuilder};

use crate::index::{ALL_LEVELS, LogIndex};

const LINES_PER_TASK: usize = 16 * 1024;

pub fn compile(pattern: &str, case_sensitive: bool, is_regex: bool) -> Result<Regex, String> {
    let pattern = if is_regex {
        pattern.to_string()
    } else {
        regex::escape(pattern)
    };
    RegexBuilder::new(&pattern)
        .case_insensitive(!case_sensitive)
        .multi_line(true)
        .size_limit(64 << 20)
        .build()
        .map_err(|e| {
            e.to_string()
                .lines()
                .last()
                .unwrap_or("invalid regex")
                .trim()
                .to_string()
        })
}

/// Returns the sorted line numbers in `lines` whose level is in `level_mask` and which
/// match `regex` (if given). Returns `None` if `generation` moved away from `expected`
/// while running, i.e. the result is stale.
pub fn filter(
    index: &LogIndex,
    lines: Range<usize>,
    regex: Option<&Regex>,
    level_mask: u8,
    generation: &AtomicU64,
    expected: u64,
) -> Option<Vec<usize>> {
    let tasks: Vec<Range<usize>> = lines
        .clone()
        .step_by(LINES_PER_TASK)
        .map(|s| s..(s + LINES_PER_TASK).min(lines.end))
        .collect();

    let parts: Vec<Vec<usize>> = tasks
        .into_par_iter()
        .map(|range| {
            if generation.load(Ordering::Relaxed) != expected {
                return Vec::new();
            }
            match regex {
                Some(re) => filter_regex(index, range, re, level_mask),
                None => filter_levels(index, range, level_mask),
            }
        })
        .collect();

    if generation.load(Ordering::Relaxed) != expected {
        return None;
    }
    let mut out = Vec::with_capacity(parts.iter().map(Vec::len).sum());
    for p in parts {
        out.extend_from_slice(&p);
    }
    Some(out)
}

fn filter_levels(index: &LogIndex, range: Range<usize>, mask: u8) -> Vec<usize> {
    let levels = &index.levels()[range.clone()];
    levels
        .iter()
        .enumerate()
        .filter(|(_, l)| mask & (1 << **l) != 0)
        .map(|(i, _)| range.start + i)
        .collect()
}

fn filter_regex(index: &LogIndex, range: Range<usize>, re: &Regex, mask: u8) -> Vec<usize> {
    let starts = &index.starts()[range.clone()];
    let levels = index.levels();
    let base = starts[0] as usize;
    let end = index.raw_range(range.end - 1).end;
    let hay = &index.data()[base..end];

    let line_at = |offset: usize| starts.partition_point(|&s| s as usize <= base + offset) - 1;

    // The search over the slice only finds the earliest end of a match from
    // `pos`, so no line before the one that end is in can match on its own; those
    // lines are confirmed one by one. Asking for the earliest end rather than the
    // full match keeps a pattern that can run across lines (`(?s)a.*`, `[^#]*x`)
    // from scanning to the end of the slice again for every line it matches.
    let mut out = Vec::new();
    let mut pos = 0;
    while pos <= hay.len() {
        let Some(end) = re.shortest_match_at(hay, pos) else { break };
        let (first, last) = (line_at(end.saturating_sub(1).max(pos)), line_at(end));
        for local in first..=last {
            let line = range.start + local;
            if mask != ALL_LEVELS && mask & (1 << levels[line]) == 0 {
                continue;
            }
            let raw = &index.data()[index.raw_range(line)];
            if re.is_match(raw.strip_suffix(b"\n").unwrap_or(raw)) {
                out.push(line);
            }
        }
        match starts.get(last + 1) {
            Some(&next) => pos = next as usize - base,
            None => break,
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index::Level;
    use std::io::Write;

    struct Rng(u64);

    impl Rng {
        fn below(&mut self, n: usize) -> usize {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            (self.0 % n.max(1) as u64) as usize
        }
    }

    fn fuzz_iters(default: usize) -> usize {
        std::env::var("LOGSHIP_FUZZ_ITERS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(default)
    }

    /// The index and the search agree with a naive line-by-line reference on
    /// random files: odd line endings, invalid UTF-8, empty lines, appends that
    /// continue an unterminated last line.
    #[test]
    fn index_append_and_search_match_a_naive_reference() {
        const WORDS: &[&str] = &[
            "ERROR", "warn", "level=info", "[debug]", "Trace", "disk", "full", "id=7", "é", "日本",
            "\u{fffd}", " ", "\t", "\r", "\r\n", "\n", "\n", "\n", "\x1b[1m",
        ];
        let dir = std::env::temp_dir().join(format!("logship-fuzz-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("f.log");
        let mut rng = Rng(7);
        for _ in 0..fuzz_iters(150) {
            let make = |rng: &mut Rng| {
                let mut v = Vec::new();
                for _ in 0..rng.below(120) {
                    v.extend_from_slice(WORDS[rng.below(WORDS.len())].as_bytes());
                    if rng.below(30) == 0 {
                        v.push(0xff);
                    }
                }
                v
            };
            let mut data = make(&mut rng);
            std::fs::write(&path, &data).unwrap();
            let mut idx = LogIndex::open(&path, &AtomicU64::new(0)).unwrap();
            for _ in 0..rng.below(3) {
                // Appended, not rewritten: Windows can't truncate a mapped file.
                let more = make(&mut rng);
                data.extend_from_slice(&more);
                let mut file = std::fs::OpenOptions::new().append(true).open(&path).unwrap();
                file.write_all(&more).unwrap();
                if let Some(delta) = idx.scan_append().unwrap() {
                    idx.apply(delta);
                }
            }
            let fresh = LogIndex::open(&path, &AtomicU64::new(0)).unwrap();
            assert_eq!(idx.starts(), fresh.starts());
            assert_eq!(idx.levels(), fresh.levels());
            assert_eq!(idx.level_counts, fresh.level_counts);

            // Lines rebuild the file exactly.
            let mut rebuilt = Vec::new();
            for l in 0..idx.line_count() {
                rebuilt.extend_from_slice(&idx.data()[idx.raw_range(l)]);
            }
            assert_eq!(rebuilt, data);
            let lines: Vec<&[u8]> = (0..idx.line_count())
                .map(|l| {
                    let raw = &idx.data()[idx.raw_range(l)];
                    raw.strip_suffix(b"\n").unwrap_or(raw)
                })
                .collect();

            // Searches find exactly the lines that match on their own, also for
            // patterns that could run across lines.
            let generation = AtomicU64::new(1);
            let queries = [
                ("ERROR", false),
                ("error", false),
                ("disk full", false),
                ("id=7", false),
                ("日本", false),
                ("é", false),
                ("\t", false),
                (r"(?s)disk.*full", true),
                (r"[^#]*full", true),
                (r"warn\s+disk", true),
                (r"full\ndisk", true),
                (r"^$", true),
                (r"x*", true),
                (r"\bid=7\b", true),
                (r"^\s*warn|full$", true),
            ];
            for (query, regex) in queries {
                for case in [true, false] {
                    let re = compile(query, case, regex).unwrap();
                    let got = filter(&idx, 0..idx.line_count(), Some(&re), ALL_LEVELS, &generation, 1).unwrap();
                    let want: Vec<usize> = (0..lines.len()).filter(|&l| re.is_match(lines[l])).collect();
                    assert_eq!(got, want, "query {query:?} case {case}");
                }
            }
            // Level filters match the per-line levels.
            for mask in [1u8 << 5, (1 << 4) | (1 << 3), ALL_LEVELS] {
                let got = filter(&idx, 0..idx.line_count(), None, mask, &generation, 1).unwrap();
                let want: Vec<usize> = (0..idx.line_count())
                    .filter(|&l| mask & (1 << idx.levels()[l]) != 0)
                    .collect();
                assert_eq!(got, want);
            }
        }
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn filters_by_regex_and_level() {
        let dir = std::env::temp_dir().join(format!("logship-search-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("b.log");
        let mut text = String::new();
        for i in 0..50_000 {
            let lvl = if i % 10 == 0 { "ERROR" } else { "INFO" };
            text.push_str(&format!("{lvl} request {i} done\n"));
        }
        std::fs::write(&path, &text).unwrap();
        let idx = LogIndex::open(&path, &AtomicU64::new(0)).unwrap();
        let generation = AtomicU64::new(1);

        let re = compile("request 4999", true, false).unwrap();
        let hits = filter(&idx, 0..idx.line_count(), Some(&re), ALL_LEVELS, &generation, 1);
        assert_eq!(hits.unwrap(), vec![4999, 49990, 49991, 49992, 49993, 49994, 49995, 49996, 49997, 49998, 49999]);

        let errors = filter(&idx, 0..idx.line_count(), None, Level::Error.bit(), &generation, 1).unwrap();
        assert_eq!(errors.len(), 5_000);

        let re = compile(r"^error request \d*00 done$", false, true).unwrap();
        let hits = filter(&idx, 0..idx.line_count(), Some(&re), ALL_LEVELS, &generation, 1).unwrap();
        assert_eq!(hits.len(), 499);

        assert!(filter(&idx, 0..idx.line_count(), None, ALL_LEVELS, &generation, 2).is_none());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
