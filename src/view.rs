//! The log explorer window.
//!
//! Rendering is fully virtualized: only the rows that fit in the window are built each
//! frame, and the scroll position is kept as `(top_row, sub-pixel offset)` instead of a
//! single f32 pixel offset, so files with hundreds of millions of lines scroll with
//! pixel precision.

use std::{
    ops::Range,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

use gpui::{
    App, ClickEvent, ClipboardItem, Context, DispatchPhase, ElementId, ExternalPaths, FocusHandle,
    FontWeight, HighlightStyle, KeyDownEvent, MouseButton, MouseDownEvent, MouseMoveEvent,
    MouseUpEvent, PathPromptOptions, Render, ScrollWheelEvent, SharedString, Stateful, StyledText,
    Task, Window, actions, canvas, div, fill, font, point, prelude::*, px, size,
};
use rayon::prelude::*;
use regex::bytes::Regex;

use crate::{
    index::{ALL_LEVELS, Level, LogIndex, detect_level},
    search,
    theme::Theme,
};

actions!(
    logship,
    [
        Open,
        Quit,
        FocusSearch,
        ToggleTheme,
        ToggleFollow,
        ToggleCase,
        ToggleRegex,
        ToggleFilterMode,
        CopyLine,
        NextMatch,
        PrevMatch,
    ]
);

#[cfg(target_os = "macos")]
const MONO: &str = "Menlo";
#[cfg(target_os = "windows")]
const MONO: &str = "Consolas";
#[cfg(not(any(target_os = "macos", target_os = "windows")))]
const MONO: &str = "DejaVu Sans Mono";

const FONT_SIZE: f32 = 12.5;
const LINE_HEIGHT: f32 = 19.;
const HEADER_H: f32 = 44.;
const STATUS_H: f32 = 26.;
const SCROLLBAR_W: f32 = 14.;
const MIN_THUMB_H: f32 = 28.;
/// Longest line prefix (in chars) that is shaped and displayed.
const MAX_COLS: usize = 4000;
const MAX_HIGHLIGHTS_PER_ROW: usize = 256;
const TAIL_POLL: Duration = Duration::from_millis(250);
const SEARCH_DEBOUNCE: Duration = Duration::from_millis(30);

const MARKER_BUCKETS: usize = 2048;
const MARK_HIT: u8 = 1;
const MARK_WARN: u8 = 2;
const MARK_ERROR: u8 = 4;

const LEVEL_CHIPS: [Level; 6] = [
    Level::Error,
    Level::Warn,
    Level::Info,
    Level::Debug,
    Level::Trace,
    Level::None,
];

struct Loading {
    path: PathBuf,
    progress: Arc<AtomicU64>,
    total: u64,
    _task: Task<()>,
    _ticker: Task<()>,
}

pub struct LogView {
    index: Option<Arc<LogIndex>>,
    loading: Option<Loading>,
    error: Option<String>,
    index_ms: u128,
    dark: bool,

    query: String,
    case_sensitive: bool,
    use_regex: bool,
    /// Filter mode shows only matching lines; highlight mode shows everything and
    /// lets you jump between matches.
    filter_mode: bool,
    level_mask: u8,
    regex: Option<Arc<Regex>>,
    regex_error: Option<String>,

    /// Row -> line mapping when a filter is active; `None` shows every line.
    rows: Option<Arc<Vec<usize>>>,
    /// Sorted line numbers matching the query (and level mask).
    hits: Arc<Vec<usize>>,
    current_hit: Option<usize>,
    filter_generation: Arc<AtomicU64>,
    filter_task: Option<Task<()>>,
    debounce: Option<Task<()>>,
    filter_ms: Option<u128>,

    top_row: usize,
    sub_px: f32,
    h_offset: f32,
    selected: Option<usize>,
    follow: bool,
    char_w: f32,
    last_visible_rows: usize,
    thumb_grab: Option<f32>,

    markers: Arc<Vec<u8>>,
    markers_dirty: bool,
    markers_task: Option<Task<()>>,

    list_focus: FocusHandle,
    search_focus: FocusHandle,
    title: String,
    _watch: Option<Task<()>>,
}

impl LogView {
    pub fn new(cx: &mut Context<Self>) -> Self {
        LogView {
            index: None,
            loading: None,
            error: None,
            index_ms: 0,
            dark: true,
            query: String::new(),
            case_sensitive: false,
            use_regex: false,
            filter_mode: true,
            level_mask: ALL_LEVELS,
            regex: None,
            regex_error: None,
            rows: None,
            hits: Arc::default(),
            current_hit: None,
            filter_generation: Arc::default(),
            filter_task: None,
            debounce: None,
            filter_ms: None,
            top_row: 0,
            sub_px: 0.,
            h_offset: 0.,
            selected: None,
            follow: false,
            char_w: 0.,
            last_visible_rows: 40,
            thumb_grab: None,
            markers: Arc::new(vec![0; MARKER_BUCKETS]),
            markers_dirty: false,
            markers_task: None,
            list_focus: cx.focus_handle(),
            search_focus: cx.focus_handle(),
            title: String::new(),
            _watch: None,
        }
    }

    pub fn list_focus(&self) -> &FocusHandle {
        &self.list_focus
    }

    // ---------------------------------------------------------------- loading

    pub fn open(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        let total = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
        let progress = Arc::new(AtomicU64::new(0));
        let job_progress = progress.clone();
        let job_path = path.clone();
        self._watch = None;
        self.error = None;

        let task = cx.spawn(async move |this, cx| {
            let started = Instant::now();
            let result = cx
                .background_executor()
                .spawn(async move { LogIndex::open(&job_path, &job_progress) })
                .await;
            this.update(cx, |this, cx| {
                this.loading = None;
                match result {
                    Ok(index) => this.set_index(index, started.elapsed().as_millis(), cx),
                    Err(err) => {
                        this.index = None;
                        this.error = Some(err.to_string());
                    }
                }
                cx.notify();
            })
            .ok();
        });
        let ticker = cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor()
                    .timer(Duration::from_millis(33))
                    .await;
                let loading = this
                    .update(cx, |this, cx| {
                        cx.notify();
                        this.loading.is_some()
                    })
                    .unwrap_or(false);
                if !loading {
                    break;
                }
            }
        });
        self.loading = Some(Loading {
            path,
            progress,
            total,
            _task: task,
            _ticker: ticker,
        });
        cx.notify();
    }

    fn set_index(&mut self, index: LogIndex, ms: u128, cx: &mut Context<Self>) {
        let reopened = self.index.as_ref().is_some_and(|i| i.path == index.path);
        self.index = Some(Arc::new(index));
        self.index_ms = ms;
        if !reopened {
            self.top_row = 0;
            self.sub_px = 0.;
            self.h_offset = 0.;
            self.selected = None;
        }
        self.current_hit = None;
        self.rows = None;
        self.hits = Arc::default();
        self.refilter(cx);
        if self.follow {
            self.scroll_to_bottom();
        }
        self.clamp_scroll();
        self.recompute_markers(cx);
        self.start_watch(cx);
    }

    /// Polls the file for appended data (live tail) and keeps derived state fresh.
    fn start_watch(&mut self, cx: &mut Context<Self>) {
        self._watch = Some(cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(TAIL_POLL).await;
                let Ok(Some(index)) = this.read_with(cx, |this, _| this.index.clone()) else {
                    break;
                };
                let len = index.len;
                let scanned = cx
                    .background_executor()
                    .spawn(async move { index.scan_append() })
                    .await;
                let keep_going = this.update(cx, |this, cx| {
                    let Some(current) = &this.index else {
                        return false;
                    };
                    if current.len != len {
                        return true;
                    }
                    match scanned {
                        Ok(None) => {}
                        Ok(Some(delta)) => this.apply_delta(delta, cx),
                        Err(_) => {
                            // Truncated or rotated: reopen if the path exists again.
                            let path = current.path.clone();
                            if std::fs::metadata(&path).is_ok() {
                                this.open(path, cx);
                                return false;
                            }
                        }
                    }
                    if this.markers_dirty {
                        this.recompute_markers(cx);
                    }
                    true
                });
                if !matches!(keep_going, Ok(true)) {
                    break;
                }
            }
        }));
    }

    fn apply_delta(&mut self, delta: crate::index::Delta, cx: &mut Context<Self>) {
        let Some(index) = self.index.as_mut() else {
            return;
        };
        let from = delta.first_line();
        Arc::make_mut(index).apply(delta);
        let count = index.line_count();

        if self.filter_task.is_some() {
            self.refilter(cx);
        } else if self.regex.is_some() || self.rows.is_some() {
            let shared = self.rows_shared_with_hits();
            if shared {
                self.rows = None;
            }
            let keep = self.hits.partition_point(|&l| l < from);
            Arc::make_mut(&mut self.hits).truncate(keep);
            if shared {
                self.rows = Some(self.hits.clone());
            } else if let Some(rows) = &mut self.rows {
                let keep = rows.partition_point(|&l| l < from);
                Arc::make_mut(rows).truncate(keep);
            }
            self.spawn_filter(from..count, true, cx);
        }
        if self.follow {
            self.scroll_to_bottom();
        }
        self.markers_dirty = true;
        cx.notify();
    }

    // ---------------------------------------------------------------- filtering

    fn rows_needed(&self) -> bool {
        self.level_mask != ALL_LEVELS || (self.filter_mode && self.regex.is_some())
    }

    fn rows_shared_with_hits(&self) -> bool {
        matches!(&self.rows, Some(rows) if Arc::ptr_eq(rows, &self.hits))
    }

    fn on_query_changed(&mut self, cx: &mut Context<Self>) {
        self.filter_generation.fetch_add(1, Ordering::SeqCst);
        if self.query.is_empty() {
            self.regex = None;
            self.regex_error = None;
        } else {
            match search::compile(&self.query, self.case_sensitive, self.use_regex) {
                Ok(re) => {
                    self.regex = Some(Arc::new(re));
                    self.regex_error = None;
                }
                Err(err) => {
                    // Keep showing the last valid results while the pattern is incomplete.
                    self.regex_error = Some(err);
                    cx.notify();
                    return;
                }
            }
        }
        self.debounce = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(SEARCH_DEBOUNCE).await;
            this.update(cx, |this, cx| {
                this.debounce = None;
                this.refilter(cx);
            })
            .ok();
        }));
        cx.notify();
    }

    fn refilter(&mut self, cx: &mut Context<Self>) {
        let Some(index) = &self.index else {
            return;
        };
        let count = index.line_count();
        self.spawn_filter(0..count, false, cx);
    }

    fn spawn_filter(&mut self, range: Range<usize>, append: bool, cx: &mut Context<Self>) {
        let generation = self.filter_generation.fetch_add(1, Ordering::SeqCst) + 1;
        let Some(index) = self.index.clone() else {
            return;
        };
        let regex = self.regex.clone();
        let mask = self.level_mask;
        let need_hits = regex.is_some();
        // In filter mode the visible rows are exactly the hits, so compute them once.
        let need_rows = self.rows_needed() && !(self.filter_mode && need_hits);

        if !append && !need_hits && !need_rows {
            self.filter_task = None;
            self.rows = None;
            self.hits = Arc::default();
            self.current_hit = None;
            self.filter_ms = None;
            self.restore_anchor();
            self.recompute_markers(cx);
            cx.notify();
            return;
        }

        let counter = self.filter_generation.clone();
        self.filter_task = Some(cx.spawn(async move |this, cx| {
            let started = Instant::now();
            let result = cx
                .background_executor()
                .spawn(async move {
                    let hits = match &regex {
                        Some(re) => Some(search::filter(
                            &index,
                            range.clone(),
                            Some(re),
                            mask,
                            &counter,
                            generation,
                        )?),
                        None => None,
                    };
                    let rows = if need_rows {
                        Some(search::filter(
                            &index, range, None, mask, &counter, generation,
                        )?)
                    } else {
                        None
                    };
                    Some((rows, hits))
                })
                .await;
            this.update(cx, |this, cx| {
                if this.filter_generation.load(Ordering::SeqCst) != generation {
                    return;
                }
                this.filter_task = None;
                if let Some((rows, hits)) = result {
                    if !append {
                        this.filter_ms = Some(started.elapsed().as_millis());
                    }
                    this.apply_filter(rows, hits, append, cx);
                }
            })
            .ok();
        }));
    }

    fn apply_filter(
        &mut self,
        rows: Option<Vec<usize>>,
        hits: Option<Vec<usize>>,
        append: bool,
        cx: &mut Context<Self>,
    ) {
        let rows_are_hits = self.filter_mode && self.regex.is_some();
        let needs_rows = self.rows_needed();
        if append {
            if rows_are_hits {
                self.rows = None;
            }
            if let Some(hits) = hits {
                Arc::make_mut(&mut self.hits).extend_from_slice(&hits);
            }
            if rows_are_hits {
                self.rows = Some(self.hits.clone());
            } else if let (Some(new), Some(rows)) = (rows, &mut self.rows) {
                Arc::make_mut(rows).extend_from_slice(&new);
            }
            if self.follow {
                self.scroll_to_bottom();
            }
        } else {
            self.hits = Arc::new(hits.unwrap_or_default());
            self.rows = if rows_are_hits {
                Some(self.hits.clone())
            } else if needs_rows {
                rows.map(Arc::new)
            } else {
                None
            };
            self.current_hit = None;
            self.restore_anchor();
        }
        self.recompute_markers(cx);
        cx.notify();
    }

    /// Keeps the selected line in view after the row mapping changed.
    fn restore_anchor(&mut self) {
        if self.follow {
            self.scroll_to_bottom();
            return;
        }
        if let Some(line) = self.selected {
            let row = self.line_to_row(line);
            self.scroll_to_row_centered(row);
        } else {
            self.clamp_scroll();
        }
    }

    fn recompute_markers(&mut self, cx: &mut Context<Self>) {
        if self.markers_task.is_some() {
            self.markers_dirty = true;
            return;
        }
        self.markers_dirty = false;
        let Some(index) = self.index.clone() else {
            return;
        };
        let rows = self.rows.clone();
        let hits = (!self.rows_shared_with_hits()).then(|| self.hits.clone());
        self.markers_task = Some(cx.spawn(async move |this, cx| {
            let markers = cx
                .background_executor()
                .spawn(async move { compute_markers(&index, rows.as_deref().map(Vec::as_slice), hits.as_deref().map(Vec::as_slice)) })
                .await;
            this.update(cx, |this, cx| {
                this.markers = Arc::new(markers);
                this.markers_task = None;
                cx.notify();
            })
            .ok();
        }));
    }

    // ---------------------------------------------------------------- navigation

    fn row_count(&self) -> usize {
        match (&self.rows, &self.index) {
            (Some(rows), _) => rows.len(),
            (None, Some(index)) => index.line_count(),
            (None, None) => 0,
        }
    }

    fn row_to_line(&self, row: usize) -> Option<usize> {
        match &self.rows {
            Some(rows) => rows.get(row).copied(),
            None => (row < self.row_count()).then_some(row),
        }
    }

    /// The row showing `line`, or the next row after it if the line is filtered out.
    fn line_to_row(&self, line: usize) -> usize {
        match &self.rows {
            Some(rows) => rows.partition_point(|&l| l < line),
            None => line,
        }
        .min(self.row_count().saturating_sub(1))
    }

    fn list_height(window: &Window) -> f32 {
        (f32::from(window.viewport_size().height) - HEADER_H - STATUS_H).max(LINE_HEIGHT)
    }

    fn visible_rows(&self, window: &Window) -> usize {
        ((Self::list_height(window) / LINE_HEIGHT).floor() as usize).max(1)
    }

    fn page(&self) -> usize {
        self.last_visible_rows.max(1)
    }

    fn max_top(&self) -> usize {
        self.row_count().saturating_sub(self.last_visible_rows)
    }

    fn scroll_offset(&self) -> f64 {
        self.top_row as f64 * LINE_HEIGHT as f64 + self.sub_px as f64
    }

    fn set_scroll_offset(&mut self, offset: f64) {
        let lh = LINE_HEIGHT as f64;
        let max = self.max_top() as f64 * lh;
        let offset = offset.clamp(0., max);
        self.top_row = (offset / lh).floor() as usize;
        self.sub_px = (offset - self.top_row as f64 * lh) as f32;
        if self.top_row >= self.max_top() {
            self.top_row = self.max_top();
            self.sub_px = 0.;
        }
    }

    fn clamp_scroll(&mut self) {
        self.set_scroll_offset(self.scroll_offset());
    }

    fn scroll_to_bottom(&mut self) {
        self.top_row = self.max_top();
        self.sub_px = 0.;
    }

    fn scroll_to_row_centered(&mut self, row: usize) {
        let half = self.page() / 2;
        self.top_row = row.saturating_sub(half).min(self.max_top());
        self.sub_px = 0.;
    }

    fn ensure_row_visible(&mut self, row: usize) {
        let page = self.page();
        if row < self.top_row || (row == self.top_row && self.sub_px > 0.) {
            self.top_row = row;
            self.sub_px = 0.;
        } else if row + 1 > self.top_row + page {
            self.top_row = (row + 1).saturating_sub(page).min(self.max_top());
            self.sub_px = 0.;
        }
    }

    fn move_selection(&mut self, delta: isize) {
        let count = self.row_count();
        if count == 0 {
            return;
        }
        let current = match self.selected {
            Some(line) => self.line_to_row(line),
            None if delta >= 0 => self.top_row.saturating_sub(1),
            None => (self.top_row + self.page()).min(count),
        };
        let row = (current as isize + delta).clamp(0, count as isize - 1) as usize;
        self.selected = self.row_to_line(row);
        self.ensure_row_visible(row);
        self.follow = self.follow && row + 1 == count;
    }

    fn select_row(&mut self, row: usize) {
        if let Some(line) = self.row_to_line(row) {
            self.selected = Some(line);
            self.ensure_row_visible(row);
        }
    }

    fn jump_to_hit(&mut self, forward: bool) {
        let hits = self.hits.clone();
        if hits.is_empty() {
            return;
        }
        let reference = self
            .selected
            .or_else(|| self.row_to_line(self.top_row))
            .unwrap_or(0);
        let ix = if forward {
            let ix = hits.partition_point(|&l| l <= reference);
            if ix == hits.len() { 0 } else { ix }
        } else {
            match hits.partition_point(|&l| l < reference) {
                0 => hits.len() - 1,
                ix => ix - 1,
            }
        };
        self.current_hit = Some(ix);
        self.selected = Some(hits[ix]);
        self.follow = false;
        let row = self.line_to_row(hits[ix]);
        if row < self.top_row || row >= self.top_row + self.page() {
            self.scroll_to_row_centered(row);
        }
    }

    fn scroll_h(&mut self, dx: f32) {
        self.h_offset = (self.h_offset + dx).clamp(0., MAX_COLS as f32 * self.char_w.max(1.));
    }

    fn copy_selected(&self, cx: &mut App) {
        if let (Some(index), Some(line)) = (&self.index, self.selected)
            && line < index.line_count()
        {
            let text = String::from_utf8_lossy(index.line(line)).into_owned();
            cx.write_to_clipboard(ClipboardItem::new_string(text));
        }
    }

    fn prompt_open(&mut self, cx: &mut Context<Self>) {
        let paths = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some("Open Log".into()),
        });
        cx.spawn(async move |this, cx| {
            if let Ok(Ok(Some(paths))) = paths.await
                && let Some(path) = paths.into_iter().next()
            {
                this.update(cx, |this, cx| this.open(path, cx)).ok();
            }
        })
        .detach();
    }

    // ---------------------------------------------------------------- input

    fn on_list_key(&mut self, ev: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let ks = &ev.keystroke;
        if ks.modifiers.platform || ks.modifiers.control {
            return;
        }
        let shift = ks.modifiers.shift;
        let page = self.page() as isize;
        match (ks.key.as_str(), shift) {
            ("up" | "k", _) => self.move_selection(-1),
            ("down" | "j", _) => self.move_selection(1),
            ("pageup", _) | ("space", true) | ("b", false) => self.move_selection(-page),
            ("pagedown", _) | ("space", false) => self.move_selection(page),
            ("home", _) | ("g", false) => {
                self.follow = false;
                self.select_row(0);
            }
            ("end", _) | ("g", true) => {
                self.select_row(self.row_count().saturating_sub(1));
                self.scroll_to_bottom();
            }
            ("left" | "h", _) => self.scroll_h(-self.char_w * 8.),
            ("right" | "l", _) => self.scroll_h(self.char_w * 8.),
            ("0", false) => self.h_offset = 0.,
            ("n", false) => self.jump_to_hit(true),
            ("n", true) => self.jump_to_hit(false),
            ("f", true) => self.toggle_follow(),
            ("/", _) => window.focus(&self.search_focus),
            ("escape", _) => self.selected = None,
            ("enter", _) => {
                // Leave filter mode and keep the selected line in context.
                if self.filter_mode && self.regex.is_some() {
                    self.filter_mode = false;
                    self.refilter(cx);
                }
            }
            _ => return,
        }
        cx.stop_propagation();
        cx.notify();
    }

    fn on_search_key(&mut self, ev: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let ks = &ev.keystroke;
        let m = ks.modifiers;
        match ks.key.as_str() {
            "escape" => {
                if self.query.is_empty() {
                    window.focus(&self.list_focus);
                } else {
                    self.query.clear();
                    self.on_query_changed(cx);
                }
            }
            "enter" => self.jump_to_hit(!m.shift),
            "up" => self.move_selection(-1),
            "down" => self.move_selection(1),
            "tab" => window.focus(&self.list_focus),
            "backspace" => {
                if m.platform {
                    self.query.clear();
                } else if m.alt {
                    let trimmed = self.query.trim_end_matches(|c: char| !c.is_alphanumeric());
                    let keep = trimmed.trim_end_matches(char::is_alphanumeric).len();
                    self.query.truncate(keep);
                } else {
                    self.query.pop();
                }
                self.on_query_changed(cx);
            }
            "v" if m.platform => {
                if let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) {
                    self.query.push_str(text.lines().next().unwrap_or_default());
                    self.on_query_changed(cx);
                }
            }
            _ if m.platform || m.control => return,
            _ => match &ks.key_char {
                Some(ch) if !ch.chars().any(char::is_control) => {
                    self.query.push_str(ch);
                    self.on_query_changed(cx);
                }
                _ => return,
            },
        }
        cx.stop_propagation();
        cx.notify();
    }

    fn on_scroll(&mut self, ev: &ScrollWheelEvent, _: &mut Window, cx: &mut Context<Self>) {
        let delta = ev.delta.pixel_delta(px(LINE_HEIGHT));
        let (mut dx, dy) = (f32::from(delta.x), f32::from(delta.y));
        if ev.modifiers.shift && dx == 0. {
            dx = dy;
        } else if dy != 0. {
            self.set_scroll_offset(self.scroll_offset() - dy as f64);
            if dy > 0. {
                self.follow = false;
            }
        }
        if dx != 0. {
            self.scroll_h(-dx);
        }
        cx.notify();
    }

    fn toggle_follow(&mut self) {
        self.follow = !self.follow;
        if self.follow {
            self.scroll_to_bottom();
        }
    }

    fn toggle_level(&mut self, level: Level, solo: bool, cx: &mut Context<Self>) {
        self.level_mask = if solo {
            if self.level_mask == level.bit() {
                ALL_LEVELS
            } else {
                level.bit()
            }
        } else {
            self.level_mask ^ level.bit()
        };
        self.refilter(cx);
        cx.notify();
    }

    // ---------------------------------------------------------------- rendering

    fn theme(&self) -> Theme {
        if self.dark { Theme::dark() } else { Theme::light() }
    }

    fn render_row(
        &self,
        row: usize,
        line: usize,
        index: &LogIndex,
        gutter_w: f32,
        t: &Theme,
        cx: &mut Context<Self>,
    ) -> (impl IntoElement + use<>, usize) {
        let text = display_text(index.line(line));
        let cols = text.chars().count();
        let level = index.level(line);
        let selected = self.selected == Some(line);
        let is_current_hit = self.current_hit.and_then(|i| self.hits.get(i)) == Some(&line);

        let mut highlights: Vec<(Range<usize>, HighlightStyle)> = Vec::new();
        if let Some(re) = &self.regex {
            let bg = if is_current_hit {
                t.current_match_bg
            } else {
                t.match_bg
            };
            for m in re.find_iter(text.as_bytes()).take(MAX_HIGHLIGHTS_PER_ROW) {
                if m.is_empty() || !text.is_char_boundary(m.start()) || !text.is_char_boundary(m.end())
                {
                    continue;
                }
                highlights.push((
                    m.range(),
                    HighlightStyle {
                        background_color: Some(bg),
                        ..Default::default()
                    },
                ));
            }
        }
        if level != Level::None
            && let Some((_, range)) = detect_level(text.as_bytes())
            && !highlights
                .iter()
                .any(|(r, _)| r.start < range.end && range.start < r.end)
        {
            let at = highlights.partition_point(|(r, _)| r.start < range.start);
            highlights.insert(
                at,
                (
                    range,
                    HighlightStyle {
                        color: Some(t.level(level)),
                        font_weight: Some(FontWeight::BOLD),
                        ..Default::default()
                    },
                ),
            );
        }

        let text_color = if level == Level::Error {
            t.error_text
        } else {
            t.text
        };
        let stripe = if level == Level::None {
            gpui::transparent_black()
        } else {
            t.level(level)
        };

        let element = div()
            .id(ElementId::Integer(row as u64))
            .h(px(LINE_HEIGHT))
            .w_full()
            .flex()
            .flex_row()
            .items_center()
            .when(selected, |d| d.bg(t.selection))
            .when(!selected, |d| d.hover(|s| s.bg(t.row_hover)))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, ev: &MouseDownEvent, window, cx| {
                    this.selected = Some(line);
                    window.focus(&this.list_focus);
                    if ev.click_count == 2 {
                        this.copy_selected(cx);
                    }
                    cx.notify();
                }),
            )
            .child(div().w(px(3.)).h_full().flex_none().bg(stripe))
            .child(
                div()
                    .w(px(gutter_w))
                    .flex_none()
                    .pr_3()
                    .text_right()
                    .text_color(if selected { t.text_muted } else { t.gutter })
                    .child(SharedString::from(format_int(line + 1))),
            )
            .child(
                div().flex_1().h_full().relative().overflow_hidden().child(
                    div()
                        .absolute()
                        .top_0()
                        .left(px(-self.h_offset))
                        .whitespace_nowrap()
                        .text_color(text_color)
                        .child(StyledText::new(text).with_highlights(highlights)),
                ),
            );
        (element, cols)
    }

    fn render_scrollbar(&self, visible: usize, t: &Theme, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let count = self.row_count();
        let max_top = self.max_top();
        let frac = if max_top == 0 {
            0.
        } else {
            (self.scroll_offset() / (max_top as f64 * LINE_HEIGHT as f64)) as f32
        };
        let markers = self.markers.clone();
        let entity = cx.entity();
        let grab = self.thumb_grab;
        let (track, thumb_color, hit_color, warn_color, error_color) = (
            t.scrollbar_track,
            t.scrollbar_thumb,
            t.accent,
            t.level(Level::Warn),
            t.level(Level::Error),
        );

        canvas(
            |_, _, _| (),
            move |bounds, _, window, _| {
                window.paint_quad(fill(bounds, track));
                let track_h = f32::from(bounds.size.height);
                let x = bounds.origin.x;
                let w = f32::from(bounds.size.width);

                // Error/warning/match overview, one pixel row per merged bucket group.
                let mut by_px = vec![0u8; track_h.max(1.) as usize + 1];
                for (i, &flag) in markers.iter().enumerate() {
                    if flag != 0 {
                        by_px[(i as f32 / MARKER_BUCKETS as f32 * track_h) as usize] |= flag;
                    }
                }
                for (py, &flag) in by_px.iter().enumerate() {
                    if flag == 0 {
                        continue;
                    }
                    let y = bounds.origin.y + px(py as f32);
                    let lvl = if flag & MARK_ERROR != 0 {
                        Some(error_color)
                    } else if flag & MARK_WARN != 0 {
                        Some(warn_color.opacity(0.7))
                    } else {
                        None
                    };
                    if let Some(color) = lvl {
                        window.paint_quad(fill(
                            gpui::Bounds::new(point(x + px(w / 2.), y), size(px(w / 2. - 2.), px(2.))),
                            color,
                        ));
                    }
                    if flag & MARK_HIT != 0 {
                        window.paint_quad(fill(
                            gpui::Bounds::new(point(x + px(2.), y), size(px(w / 2. - 2.), px(2.))),
                            hit_color,
                        ));
                    }
                }

                if count <= visible {
                    return;
                }
                let thumb_h = (track_h * visible as f32 / count as f32).max(MIN_THUMB_H);
                let travel = (track_h - thumb_h).max(1.);
                let thumb_top = travel * frac;
                let thumb = gpui::Bounds::new(
                    point(x + px(3.), bounds.origin.y + px(thumb_top)),
                    size(px(w - 6.), px(thumb_h)),
                );
                let color = if grab.is_some() {
                    thumb_color.opacity(1.6)
                } else {
                    thumb_color
                };
                window.paint_quad(fill(thumb, color).corner_radii(px(4.)));

                let top = bounds.origin.y;
                let set_from_y = move |this: &mut LogView, y: f32, grab: f32| {
                    let f = ((y - f32::from(top) - grab) / travel).clamp(0., 1.) as f64;
                    let max = this.max_top() as f64 * LINE_HEIGHT as f64;
                    this.set_scroll_offset(f * max);
                    this.follow = f >= 1.;
                };

                window.on_mouse_event({
                    let entity = entity.clone();
                    move |ev: &MouseDownEvent, phase, _, cx| {
                        if phase != DispatchPhase::Bubble || !bounds.contains(&ev.position) {
                            return;
                        }
                        let y = f32::from(ev.position.y);
                        let local = y - f32::from(top);
                        let grab = if (thumb_top..thumb_top + thumb_h).contains(&local) {
                            local - thumb_top
                        } else {
                            thumb_h / 2.
                        };
                        entity.update(cx, |this, cx| {
                            this.thumb_grab = Some(grab);
                            set_from_y(this, y, grab);
                            cx.notify();
                        });
                        cx.stop_propagation();
                    }
                });
                window.on_mouse_event({
                    let entity = entity.clone();
                    move |ev: &MouseMoveEvent, _, _, cx| {
                        let Some(grab) = grab else { return };
                        if !ev.dragging() {
                            return;
                        }
                        entity.update(cx, |this, cx| {
                            set_from_y(this, f32::from(ev.position.y), grab);
                            cx.notify();
                        });
                    }
                });
                window.on_mouse_event(move |_: &MouseUpEvent, _, _, cx| {
                    if grab.is_some() {
                        entity.update(cx, |this, cx| {
                            this.thumb_grab = None;
                            cx.notify();
                        });
                    }
                });
            },
        )
        .absolute()
        .top_0()
        .right_0()
        .w(px(SCROLLBAR_W))
        .h_full()
    }

    fn render_list(&mut self, window: &mut Window, t: &Theme, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let visible = self.visible_rows(window);
        self.last_visible_rows = visible;
        self.clamp_scroll();
        let index = self.index.clone().expect("render_list requires an index");
        let digits = format_int(index.line_count().max(1)).len() as f32;
        let gutter_w = digits * self.char_w + 20.;

        let first = self.top_row;
        let last = (first + visible + 2).min(self.row_count());
        let mut widest = 0;
        let mut rows = Vec::with_capacity(last - first);
        for row in first..last {
            let Some(line) = self.row_to_line(row) else { break };
            let (el, cols) = self.render_row(row, line, &index, gutter_w, t, cx);
            widest = widest.max(cols);
            rows.push(el);
        }
        let max_h = (widest as f32 * self.char_w - 200.).max(0.);
        if self.h_offset > max_h {
            self.h_offset = max_h;
        }

        div()
            .id("list")
            .flex_1()
            .relative()
            .overflow_hidden()
            .bg(t.bg)
            .font_family(MONO)
            .text_size(px(FONT_SIZE))
            .line_height(px(LINE_HEIGHT))
            .track_focus(&self.list_focus)
            .on_key_down(cx.listener(Self::on_list_key))
            .on_scroll_wheel(cx.listener(Self::on_scroll))
            .child(
                div()
                    .absolute()
                    .top(px(-self.sub_px))
                    .left_0()
                    .right(px(SCROLLBAR_W))
                    .children(rows),
            )
            .child(self.render_scrollbar(visible, t, cx))
    }

    fn render_header(&self, window: &Window, t: &Theme, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let search_focused = self.search_focus.is_focused(window);
        let hit_info = if self.regex.is_some() {
            let total = self.hits.len();
            match self.current_hit {
                Some(i) => format!("{} / {}", format_int(i + 1), format_int(total)),
                None => format!("{} matches", format_int(total)),
            }
        } else {
            String::new()
        };
        let searching = self.filter_task.is_some() || self.debounce.is_some();

        let search_box = div()
            .id("search")
            .track_focus(&self.search_focus)
            .on_key_down(cx.listener(Self::on_search_key))
            .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                window.focus(&this.search_focus);
                cx.notify();
            }))
            .flex()
            .items_center()
            .gap_2()
            .h(px(28.))
            .w(px(380.))
            .px_2()
            .rounded_md()
            .border_1()
            .border_color(if self.regex_error.is_some() {
                t.error_text
            } else if search_focused {
                t.accent
            } else {
                t.border
            })
            .bg(t.bg)
            .cursor_text()
            .child(div().text_color(t.text_muted).child("⌕"))
            .child(
                div()
                    .flex_1()
                    .flex()
                    .items_center()
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .font_family(MONO)
                    .text_size(px(FONT_SIZE))
                    .when(self.query.is_empty(), |d| {
                        d.text_color(t.text_muted).child("Search  (⌘F)")
                    })
                    .when(!self.query.is_empty(), |d| {
                        d.text_color(t.text).child(SharedString::from(self.query.clone()))
                    })
                    .when(search_focused, |d| {
                        d.child(div().w(px(1.5)).h(px(15.)).bg(t.accent))
                    }),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(t.text_muted)
                    .whitespace_nowrap()
                    .child(if searching && !self.query.is_empty() {
                        "…".to_string()
                    } else {
                        hit_info
                    }),
            )
            .child(
                chip("case", self.case_sensitive, t)
                    .child("Aa")
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                        this.case_sensitive = !this.case_sensitive;
                        this.on_query_changed(cx);
                    })),
            )
            .child(
                chip("regex", self.use_regex, t)
                    .child(".*")
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                        this.use_regex = !this.use_regex;
                        this.on_query_changed(cx);
                    })),
            );

        let mode = div()
            .flex()
            .rounded_md()
            .border_1()
            .border_color(t.border)
            .p(px(2.))
            .gap(px(2.))
            .child(
                chip("mode-filter", self.filter_mode, t)
                    .child("Filter")
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                        this.filter_mode = true;
                        this.refilter(cx);
                        cx.notify();
                    })),
            )
            .child(
                chip("mode-highlight", !self.filter_mode, t)
                    .child("Highlight")
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                        this.filter_mode = false;
                        this.refilter(cx);
                        cx.notify();
                    })),
            );

        let counts = self.index.as_ref().map(|i| i.level_counts).unwrap_or_default();
        let levels = div().flex().gap_1().children(LEVEL_CHIPS.map(|level| {
            let active = self.level_mask & level.bit() != 0;
            let count = counts[level as usize];
            chip(SharedString::from(format!("lvl-{}", level.label())), active, t)
                .child(
                    div()
                        .size(px(7.))
                        .rounded_full()
                        .bg(if active { t.level(level) } else { t.border }),
                )
                .child(SharedString::from(format!(
                    "{} {}",
                    level_short(level),
                    format_compact(count)
                )))
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, ev: &MouseDownEvent, _, cx| {
                        this.toggle_level(level, ev.modifiers.alt || ev.modifiers.platform, cx);
                        cx.stop_propagation();
                    }),
                )
        }));

        div()
            .h(px(HEADER_H))
            .flex_none()
            .flex()
            .items_center()
            .gap_3()
            .px_3()
            .bg(t.surface)
            .border_b_1()
            .border_color(t.border)
            .child(
                chip("open", false, t)
                    .child("Open…")
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.prompt_open(cx))),
            )
            .child(search_box)
            .child(mode)
            .child(levels)
            .child(div().flex_1())
            .child(
                chip("follow", self.follow, t)
                    .child(if self.follow { "● Follow" } else { "○ Follow" })
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                        this.toggle_follow();
                        cx.notify();
                    })),
            )
            .child(
                chip("theme", false, t)
                    .child(if self.dark { "☀" } else { "☾" })
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                        this.dark = !this.dark;
                        cx.notify();
                    })),
            )
    }

    fn render_status(&self, t: &Theme) -> impl IntoElement + use<> {
        let mut left: Vec<String> = Vec::new();
        if let Some(index) = &self.index {
            left.push(index.path.display().to_string());
            left.push(format_bytes(index.len));
            left.push(format!("{} lines", format_int(index.line_count())));
            left.push(format!("indexed in {} ms", self.index_ms));
        }
        let mut right: Vec<String> = Vec::new();
        if self.rows.is_some() {
            let mut s = format!("{} shown", format_int(self.row_count()));
            if let Some(ms) = self.filter_ms {
                s.push_str(&format!(" · {ms} ms"));
            }
            right.push(s);
        }
        if let Some(line) = self.selected {
            right.push(format!("Ln {}", format_int(line + 1)));
        }

        div()
            .h(px(STATUS_H))
            .flex_none()
            .flex()
            .items_center()
            .gap_4()
            .px_3()
            .bg(t.surface)
            .border_t_1()
            .border_color(t.border)
            .text_xs()
            .text_color(t.text_muted)
            .child(div().flex_1().truncate().child(left.join("  ·  ")))
            .when_some(self.regex_error.clone(), |d, err| {
                d.child(div().text_color(t.error_text).truncate().child(err))
            })
            .child(div().whitespace_nowrap().child(right.join("  ·  ")))
    }

    fn render_placeholder(&self, t: &Theme) -> impl IntoElement + use<> {
        let body = if let Some(loading) = &self.loading {
            let done = loading.progress.load(Ordering::Relaxed) as f32;
            let frac = if loading.total == 0 {
                1.
            } else {
                (done / loading.total as f32).clamp(0., 1.)
            };
            div()
                .flex()
                .flex_col()
                .items_center()
                .gap_3()
                .child(
                    div().text_color(t.text).child(format!(
                        "Indexing {}",
                        loading
                            .path
                            .file_name()
                            .map(|n| n.to_string_lossy().into_owned())
                            .unwrap_or_default()
                    )),
                )
                .child(
                    div()
                        .w(px(320.))
                        .h(px(4.))
                        .rounded_full()
                        .bg(t.border)
                        .child(div().h_full().rounded_full().w(px(320. * frac)).bg(t.accent)),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(t.text_muted)
                        .child(format!("{} of {}", format_bytes(done as u64), format_bytes(loading.total))),
                )
        } else {
            div()
                .flex()
                .flex_col()
                .items_center()
                .gap_2()
                .child(div().text_2xl().text_color(t.text).child("logship"))
                .child(
                    div()
                        .text_color(t.text_muted)
                        .child("Open a log file with ⌘O or drop it here"),
                )
                .when_some(self.error.clone(), |d, err| {
                    d.child(div().mt_2().text_color(t.error_text).child(err))
                })
        };
        div()
            .flex_1()
            .flex()
            .items_center()
            .justify_center()
            .bg(t.bg)
            .track_focus(&self.list_focus)
            .child(body)
    }
}

impl Render for LogView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = self.theme();
        if self.char_w == 0. {
            let text = window.text_system();
            let id = text.resolve_font(&font(MONO));
            self.char_w = text
                .advance(id, px(FONT_SIZE), 'm')
                .map(|s| f32::from(s.width))
                .unwrap_or(FONT_SIZE * 0.6);
        }

        let title = match &self.index {
            Some(index) => format!(
                "{} — logship",
                index
                    .path
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default()
            ),
            None => "logship".into(),
        };
        if title != self.title {
            window.set_window_title(&title);
            self.title = title;
        }

        let header = self.render_header(window, &t, cx);
        let status = self.render_status(&t);
        let body = if self.index.is_some() && self.loading.is_none() {
            self.render_list(window, &t, cx).into_any_element()
        } else {
            self.render_placeholder(&t).into_any_element()
        };

        div()
            .id("root")
            .key_context("LogView")
            .size_full()
            .flex()
            .flex_col()
            .bg(t.bg)
            .text_color(t.text)
            .text_sm()
            .on_action(cx.listener(|this, _: &Open, _, cx| this.prompt_open(cx)))
            .on_action(cx.listener(|this, _: &FocusSearch, window, cx| {
                window.focus(&this.search_focus);
                cx.notify();
            }))
            .on_action(cx.listener(|this, _: &ToggleTheme, _, cx| {
                this.dark = !this.dark;
                cx.notify();
            }))
            .on_action(cx.listener(|this, _: &ToggleFollow, _, cx| {
                this.toggle_follow();
                cx.notify();
            }))
            .on_action(cx.listener(|this, _: &ToggleCase, _, cx| {
                this.case_sensitive = !this.case_sensitive;
                this.on_query_changed(cx);
            }))
            .on_action(cx.listener(|this, _: &ToggleRegex, _, cx| {
                this.use_regex = !this.use_regex;
                this.on_query_changed(cx);
            }))
            .on_action(cx.listener(|this, _: &ToggleFilterMode, _, cx| {
                this.filter_mode = !this.filter_mode;
                this.refilter(cx);
                cx.notify();
            }))
            .on_action(cx.listener(|this, _: &CopyLine, _, cx| this.copy_selected(cx)))
            .on_action(cx.listener(|this, _: &NextMatch, _, cx| {
                this.jump_to_hit(true);
                cx.notify();
            }))
            .on_action(cx.listener(|this, _: &PrevMatch, _, cx| {
                this.jump_to_hit(false);
                cx.notify();
            }))
            .on_drop(cx.listener(|this, paths: &ExternalPaths, _, cx| {
                if let Some(path) = paths.paths().first() {
                    this.open(path.clone(), cx);
                }
            }))
            .drag_over::<ExternalPaths>({
                let accent = t.accent;
                move |style, _, _, _| style.border_2().border_color(accent)
            })
            .child(header)
            .child(body)
            .child(status)
    }
}

fn chip(id: impl Into<ElementId>, active: bool, t: &Theme) -> Stateful<gpui::Div> {
    let hover = t.surface_hover;
    div()
        .id(id)
        .flex()
        .flex_none()
        .items_center()
        .gap_1()
        .px_2()
        .h(px(24.))
        .rounded_md()
        .text_xs()
        .whitespace_nowrap()
        .cursor_pointer()
        .when(active, |d| d.bg(t.surface_hover).text_color(t.text))
        .when(!active, |d| d.text_color(t.text_muted))
        .hover(move |s| s.bg(hover))
}

fn level_short(level: Level) -> &'static str {
    match level {
        Level::Error => "ERR",
        Level::Warn => "WRN",
        Level::Info => "INF",
        Level::Debug => "DBG",
        Level::Trace => "TRC",
        Level::None => "—",
    }
}

/// Converts raw line bytes into displayable text: lossy UTF-8, ANSI escape sequences
/// stripped, tabs expanded, control characters made visible, length capped.
fn display_text(bytes: &[u8]) -> String {
    let bytes = &bytes[..bytes.len().min(MAX_COLS * 4)];
    let s = String::from_utf8_lossy(bytes);
    let mut out = String::with_capacity(s.len());
    let mut cols = 0;
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\x1b' => {
                if chars.peek() == Some(&'[') {
                    chars.next();
                    for c in chars.by_ref() {
                        if ('@'..='~').contains(&c) {
                            break;
                        }
                    }
                }
            }
            '\t' => {
                let n = 4 - cols % 4;
                out.extend(std::iter::repeat_n(' ', n));
                cols += n;
            }
            c if c.is_control() => {
                out.push('·');
                cols += 1;
            }
            c => {
                out.push(c);
                cols += 1;
            }
        }
        if cols >= MAX_COLS {
            out.push('…');
            break;
        }
    }
    out
}

fn compute_markers(index: &LogIndex, rows: Option<&[usize]>, hits: Option<&[usize]>) -> Vec<u8> {
    let count = rows.map_or(index.line_count(), <[usize]>::len);
    let mut out = vec![0u8; MARKER_BUCKETS];
    if count == 0 {
        return out;
    }
    let levels = index.levels();
    let classify = |l: u8, f: &mut u8| {
        if l == Level::Error as u8 {
            *f |= MARK_ERROR;
        } else if l == Level::Warn as u8 {
            *f |= MARK_WARN;
        }
    };
    out.par_iter_mut().enumerate().for_each(|(b, flag)| {
        let start = b * count / MARKER_BUCKETS;
        let end = ((b + 1) * count / MARKER_BUCKETS).max(start + 1).min(count);
        if start >= count {
            return;
        }
        let mut f = 0;
        match rows {
            Some(rows) => {
                for &line in &rows[start..end] {
                    classify(levels[line], &mut f);
                    if f & MARK_ERROR != 0 {
                        break;
                    }
                }
            }
            None => {
                for &l in &levels[start..end] {
                    classify(l, &mut f);
                    if f & MARK_ERROR != 0 {
                        break;
                    }
                }
            }
        }
        *flag = f;
    });
    if let Some(hits) = hits {
        for &line in hits {
            let row = rows.map_or(line, |r| r.partition_point(|&l| l < line));
            out[(row * MARKER_BUCKETS / count).min(MARKER_BUCKETS - 1)] |= MARK_HIT;
        }
    }
    out
}

fn format_int(n: usize) -> String {
    let s = n.to_string();
    let mut out = String::with_capacity(s.len() + s.len() / 3);
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

fn format_compact(n: usize) -> String {
    match n {
        0..1_000 => n.to_string(),
        1_000..1_000_000 => format!("{:.1}k", n as f64 / 1e3),
        1_000_000..1_000_000_000 => format!("{:.1}M", n as f64 / 1e6),
        _ => format!("{:.1}G", n as f64 / 1e9),
    }
}

fn format_bytes(n: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut v = n as f64;
    let mut unit = 0;
    while v >= 1024. && unit < UNITS.len() - 1 {
        v /= 1024.;
        unit += 1;
    }
    if unit == 0 {
        format!("{n} B")
    } else {
        format!("{v:.1} {}", UNITS[unit])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_text_cleans_lines() {
        assert_eq!(display_text(b"\x1b[31mred\x1b[0m\tx"), "red x");
        assert_eq!(display_text(b"a\x00b"), "a·b");
        assert_eq!(display_text(&[0xff, b'a']), "\u{fffd}a");
    }

    #[test]
    fn formats_numbers() {
        assert_eq!(format_int(1234567), "1,234,567");
        assert_eq!(format_int(12), "12");
        assert_eq!(format_compact(1500), "1.5k");
        assert_eq!(format_bytes(1536), "1.5 KB");
    }
}
