//! Parallel line filtering over a [`LogIndex`].
//!
//! Text queries run the regex over whole multi-megabyte slices of the mapped file and
//! map hits back to lines, which avoids per-line call overhead and lets the regex
//! engine's literal prefilters (memchr / Teddy) skip most of the input.

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

    let mut out = Vec::new();
    let mut pos = 0;
    while pos <= hay.len() {
        let Some(m) = re.find_at(hay, pos) else { break };
        let abs = (base + m.start()) as u64;
        let local = starts.partition_point(|&s| s <= abs) - 1;
        let line = range.start + local;
        if mask == ALL_LEVELS || mask & (1 << levels[line]) != 0 {
            out.push(line);
        }
        match starts.get(local + 1) {
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
