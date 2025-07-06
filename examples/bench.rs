//! Headless benchmark: `cargo run --release --example bench -- <file> [query]`
#![allow(dead_code)]
#[path = "../src/index.rs"]
mod index;
#[path = "../src/search.rs"]
mod search;

use std::{sync::atomic::AtomicU64, time::Instant};

fn main() {
    let mut args = std::env::args().skip(1);
    let path = args.next().expect("usage: bench <file> [query]");
    let query = args.next().unwrap_or_else(|| "connection reset".into());

    let t = Instant::now();
    let idx = index::LogIndex::open(path.as_ref(), &AtomicU64::new(0)).unwrap();
    println!(
        "indexed {} lines / {} MB in {:?}",
        idx.line_count(),
        idx.len >> 20,
        t.elapsed()
    );

    let generation = AtomicU64::new(0);
    for (label, case, regex) in [("literal", false, false), ("literal+case", true, false), ("regex", false, true)] {
        let re = search::compile(&query, case, regex).unwrap();
        let t = Instant::now();
        let hits = search::filter(&idx, 0..idx.line_count(), Some(&re), index::ALL_LEVELS, &generation, 0).unwrap();
        println!("{label:14} {:>10} hits in {:?}", hits.len(), t.elapsed());
    }
    let t = Instant::now();
    let errors = search::filter(&idx, 0..idx.line_count(), None, index::Level::Error.bit(), &generation, 0).unwrap();
    println!("level=ERROR    {:>10} rows in {:?}", errors.len(), t.elapsed());
}
