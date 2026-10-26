//! The log explorer window.
//!
//! Rendering is fully virtualized: only the rows that fit in the window are built each
//! frame, and the scroll position is kept as `(top_row, sub-pixel offset)` instead of a
//! single f32 pixel offset, so files with hundreds of millions of lines scroll with
//! pixel precision.

use std::{
    cell::Cell,
    ops::Range,
    path::{Path, PathBuf},
    rc::Rc,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

use gpui::{
    App, Bounds, ClickEvent, ClipboardItem, Context, CursorStyle, DispatchPhase, ElementId,
    EventEmitter, FocusHandle, Font, FontWeight, HighlightStyle, HitboxBehavior, Hsla,
    KeyDownEvent, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, Pixels, Point,
    Render, ScrollWheelEvent, ShapedLine, SharedString, Stateful, StyledText, Task, TextRun,
    Window, actions, canvas, div, fill, font, point, prelude::*, px, size,
};
use rayon::prelude::*;
use regex::bytes::Regex;

use crate::{
    decorations,
    index::{ALL_LEVELS, Level, LogIndex, detect_level},
    search,
    session::RecentFiles,
    settings::Settings,
    theme::Theme,
    workspace::TABBAR_H,
};

actions!(
    logship,
    [
        Open,
        OpenContainer,
        Quit,
        FocusSearch,
        ToggleTheme,
        ToggleFollow,
        ToggleNewestFirst,
        ToggleCase,
        ToggleRegex,
        ToggleFilterMode,
        Copy,
        SelectAll,
        NextMatch,
        PrevMatch,
        NewTab,
        CloseTab,
        NextTab,
        PrevTab,
        OpenSettings,
    ]
);

const HEADER_H: f32 = 44.;
const STATUS_H: f32 = 26.;
const SCROLLBAR_W: f32 = 14.;
/// Width of the level stripe left of the line numbers.
const STRIPE_W: f32 = 3.;
/// Most text a single copy puts on the clipboard.
const MAX_COPY: usize = 64 << 20;
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

/// "⌘" on macOS, "Ctrl+" elsewhere, for shortcut hints.
pub fn mod_key() -> &'static str {
    if cfg!(target_os = "macos") { "⌘" } else { "Ctrl+" }
}

/// A position in the log: a line and a byte offset into its raw contents.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct TextPos {
    line: usize,
    byte: usize,
}

/// What a mouse drag selects: characters, words (double-click) or lines (triple-click).
#[derive(Clone, Copy, PartialEq, Eq)]
enum DragUnit {
    Char,
    Word,
    Line,
}

struct Drag {
    unit: DragUnit,
    /// The word or line first clicked, which stays selected while dragging.
    origin: (TextPos, TextPos),
}

/// A point on a log row: the row's display text and the byte under the pointer.
struct Hit {
    line: usize,
    display: DisplayLine,
    /// Character boundary in `display.text`.
    ix: usize,
    raw_len: usize,
}

impl Hit {
    fn pos_at(&self, ix: usize) -> TextPos {
        TextPos {
            line: self.line,
            byte: self.display.raw_offset(ix, self.raw_len),
        }
    }
}

pub enum LogViewEvent {
    /// A file started loading in this view.
    Opened(PathBuf),
    /// A recent file was picked on the empty-tab screen.
    OpenRequested(PathBuf),
}

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
    /// The file shown or being loaded, even if loading failed.
    path: Option<PathBuf>,

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
    /// Newest lines at the top: display row 0 shows the last row in file order.
    reversed: bool,
    char_w: f32,
    font_family: SharedString,
    font_size: f32,
    line_h: f32,
    gutter_w: f32,
    last_visible_rows: usize,
    thumb_grab: Option<f32>,

    markers: Arc<Vec<u8>>,
    markers_dirty: bool,
    markers_task: Option<Task<()>>,

    /// Text selection as (anchor, head); the head follows the pointer while dragging.
    selection: Option<(TextPos, TextPos)>,
    drag: Option<Drag>,
    drag_pos: Option<Point<Pixels>>,
    autoscroll: Option<Task<()>>,
    /// Where the log rows were drawn in the last frame, for mouse hit-testing.
    list_bounds: Rc<Cell<Option<Bounds<Pixels>>>>,
    /// Brief message in the status bar, e.g. after copying.
    note: Option<String>,
    note_task: Option<Task<()>>,

    list_focus: FocusHandle,
    search_focus: FocusHandle,
    _watch: Option<Task<()>>,
}

impl EventEmitter<LogViewEvent> for LogView {}

impl LogView {
    pub fn new(cx: &mut Context<Self>) -> Self {
        LogView {
            index: None,
            loading: None,
            error: None,
            index_ms: 0,
            path: None,
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
            reversed: false,
            char_w: 0.,
            font_family: SharedString::default(),
            font_size: 0.,
            line_h: 19.,
            gutter_w: 0.,
            last_visible_rows: 40,
            thumb_grab: None,
            markers: Arc::new(vec![0; MARKER_BUCKETS]),
            markers_dirty: false,
            markers_task: None,
            selection: None,
            drag: None,
            drag_pos: None,
            autoscroll: None,
            list_bounds: Rc::default(),
            note: None,
            note_task: None,
            list_focus: cx.focus_handle(),
            search_focus: cx.focus_handle(),
            _watch: None,
        }
    }

    pub fn list_focus(&self) -> &FocusHandle {
        &self.list_focus
    }

    /// The file shown or being loaded in this view.
    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    /// Picks up font changes from the settings.
    fn sync_settings(&mut self, window: &Window, cx: &App) {
        let s = cx.global::<Settings>();
        let newest_first = s.newest_first;
        if self.char_w == 0. || self.font_family.as_ref() != s.font_family || self.font_size != s.font_size {
            self.font_family = s.font_family.clone().into();
            self.font_size = s.font_size;
            self.line_h = s.line_height();
            let text = window.text_system();
            let id = text.resolve_font(&font(self.font_family.clone()));
            self.char_w = text
                .advance(id, px(self.font_size), 'm')
                .map(|s| f32::from(s.width))
                .unwrap_or(self.font_size * 0.6);
            self.clamp_scroll();
        }
        if newest_first != self.reversed {
            self.set_reversed(newest_first);
        }
    }

    /// Flips the row order, keeping the selected line (or the one in the middle of
    /// the view) in view, or the newest lines when following.
    fn set_reversed(&mut self, reversed: bool) {
        let middle = (self.top_row + self.page() / 2).min(self.row_count().saturating_sub(1));
        let anchor = self.selected.or_else(|| self.row_to_line(middle));
        self.reversed = reversed;
        if self.follow {
            self.scroll_to_latest();
        } else if let Some(line) = anchor {
            self.scroll_to_row_centered(self.line_to_row(line));
        }
    }

    // ---------------------------------------------------------------- loading

    pub fn open(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        let total = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
        let progress = Arc::new(AtomicU64::new(0));
        let job_progress = progress.clone();
        let job_path = path.clone();
        self._watch = None;
        self.error = None;
        self.path = Some(path.clone());
        cx.emit(LogViewEvent::Opened(path.clone()));

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
        let lines = index.line_count();
        self.index = Some(Arc::new(index));
        self.index_ms = ms;
        self.selection = None;
        self.drag = None;
        if !reopened {
            self.top_row = 0;
            self.sub_px = 0.;
            self.h_offset = 0.;
            self.selected = None;
        }
        // A reopened file can be shorter than before (rotated or truncated).
        self.selected = self.selected.filter(|&line| line < lines);
        self.current_hit = None;
        self.rows = None;
        self.hits = Arc::default();
        self.refilter(cx);
        if self.follow {
            self.scroll_to_latest();
        }
        self.clamp_scroll();
        self.recompute_markers(cx);
        self.start_watch(cx);
    }

    /// When showing newest first and not following, appended lines push the rows
    /// down; shift the view by the same amount so the lines being read stay put.
    fn keep_view_on_append(&mut self, rows_before: usize) {
        if self.reversed && !self.follow {
            let added = self.row_count() as isize - rows_before as isize;
            self.top_row = (self.top_row as isize + added).max(0) as usize;
            self.clamp_scroll();
        }
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
        let rows_before = match &self.rows {
            Some(rows) => rows.len(),
            None => index.line_count(),
        };
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
        self.keep_view_on_append(rows_before);
        if self.follow {
            self.scroll_to_latest();
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
            let rows_before = self.row_count();
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
            self.keep_view_on_append(rows_before);
            if self.follow {
                self.scroll_to_latest();
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
            self.scroll_to_latest();
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

    /// The line shown on display row `row`.
    fn row_to_line(&self, row: usize) -> Option<usize> {
        let row = flip_row(row, self.row_count(), self.reversed)?;
        match &self.rows {
            Some(rows) => rows.get(row).copied(),
            None => Some(row),
        }
    }

    /// The display row showing `line`, or the row of the next line in the file if
    /// `line` is filtered out.
    fn line_to_row(&self, line: usize) -> usize {
        let count = self.row_count();
        let row = match &self.rows {
            Some(rows) => rows.partition_point(|&l| l < line),
            None => line,
        }
        .min(count.saturating_sub(1));
        flip_row(row, count, self.reversed).unwrap_or(0)
    }

    fn list_height(&self, window: &Window) -> f32 {
        let chrome = HEADER_H + STATUS_H + TABBAR_H + decorations::chrome_height(window);
        (f32::from(window.viewport_size().height) - chrome).max(self.line_h)
    }

    fn visible_rows(&self, window: &Window) -> usize {
        ((self.list_height(window) / self.line_h).floor() as usize).max(1)
    }

    fn page(&self) -> usize {
        self.last_visible_rows.max(1)
    }

    fn max_top(&self) -> usize {
        self.row_count().saturating_sub(self.last_visible_rows)
    }

    fn scroll_offset(&self) -> f64 {
        self.top_row as f64 * self.line_h as f64 + self.sub_px as f64
    }

    fn set_scroll_offset(&mut self, offset: f64) {
        let lh = self.line_h as f64;
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

    /// Scrolls to the newest lines: the bottom, or the top when showing newest first.
    fn scroll_to_latest(&mut self) {
        if self.reversed {
            self.top_row = 0;
            self.sub_px = 0.;
        } else {
            self.scroll_to_bottom();
        }
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
        let at_latest = if self.reversed { row == 0 } else { row + 1 == count };
        self.follow = self.follow && at_latest;
    }

    fn select_row(&mut self, row: usize) {
        if let Some(line) = self.row_to_line(row) {
            self.selected = Some(line);
            self.ensure_row_visible(row);
        }
    }

    fn jump_to_hit(&mut self, forward: bool) {
        // "Next" always moves down the screen.
        let forward = forward != self.reversed;
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

    // ---------------------------------------------------------------- text selection

    /// The selection as (start, end), if it covers any text.
    /// The selection as (first, last) in screen order, if it covers any text.
    fn selection_range(&self) -> Option<(TextPos, TextPos)> {
        let (a, b) = self.selection?;
        let (first, last) = if before(b, a, self.reversed) { (b, a) } else { (a, b) };
        (first != last).then_some((first, last))
    }

    /// The row character under `pos` (window coordinates). Positions above or below
    /// the rows on screen hit the first or last of them.
    fn hit(&self, pos: Point<Pixels>, window: &Window) -> Option<Hit> {
        let bounds = self.list_bounds.get()?;
        let index = self.index.as_ref()?;
        let count = self.row_count();
        if count == 0 {
            return None;
        }
        let y = f32::from(pos.y - bounds.top()).clamp(0., (f32::from(bounds.size.height) - 1.).max(0.));
        let row = (self.top_row + ((y + self.sub_px) / self.line_h) as usize).min(count - 1);
        let line = self.row_to_line(row)?;
        let bytes = index.line(line);
        let display = DisplayLine::new(bytes);
        let x = f32::from(pos.x - bounds.left()) - STRIPE_W - self.gutter_w + self.h_offset;
        let ix = if x <= 0. || display.text.is_empty() {
            0
        } else {
            let shaped = self.shape(&display.text, index.level(line), window);
            closest_boundary(&shaped, px(x))
        };
        Some(Hit {
            line,
            display,
            ix,
            raw_len: bytes.len(),
        })
    }

    /// Shapes a row's text the way `render_row` draws it (the level token is bold).
    fn shape(&self, text: &str, level: Level, window: &Window) -> ShapedLine {
        let normal = font(self.font_family.clone());
        let run = |len: usize, font: Font| TextRun {
            len,
            font,
            color: gpui::black(),
            background_color: None,
            underline: None,
            strikethrough: None,
        };
        let mut runs = Vec::with_capacity(3);
        match detect_level(text.as_bytes()).filter(|_| level != Level::None) {
            Some((_, token)) => {
                let bold = Font {
                    weight: FontWeight::BOLD,
                    ..normal.clone()
                };
                runs.push(run(token.start, normal.clone()));
                runs.push(run(token.len(), bold));
                runs.push(run(text.len() - token.end, normal));
            }
            None => runs.push(run(text.len(), normal)),
        }
        runs.retain(|r| r.len > 0);
        window
            .text_system()
            .shape_line(text.to_string().into(), px(self.font_size), &runs, None)
    }

    fn word_at(hit: &Hit) -> (TextPos, TextPos) {
        let r = word_range(&hit.display.text, hit.ix);
        (hit.pos_at(r.start), hit.pos_at(r.end))
    }

    fn line_at(&self, line: usize) -> (TextPos, TextPos) {
        let len = self.index.as_ref().map_or(0, |i| i.line(line).len());
        (TextPos { line, byte: 0 }, TextPos { line, byte: len })
    }

    fn on_text_mouse_down(&mut self, ev: &MouseDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        window.focus(&self.list_focus);
        let Some(hit) = self.hit(ev.position, window) else {
            return;
        };
        self.selected = Some(hit.line);
        let pos = hit.pos_at(hit.ix);
        let (unit, origin) = match ev.click_count {
            0 | 1 => (DragUnit::Char, (pos, pos)),
            2 => (DragUnit::Word, Self::word_at(&hit)),
            _ => (DragUnit::Line, self.line_at(hit.line)),
        };
        self.selection = match self.selection {
            Some((anchor, _)) if unit == DragUnit::Char && ev.modifiers.shift => Some((anchor, pos)),
            _ => Some(origin),
        };
        self.drag = Some(Drag { unit, origin });
        self.drag_pos = Some(ev.position);
        cx.notify();
    }

    fn on_text_drag(&mut self, pos: Point<Pixels>, window: &mut Window, cx: &mut Context<Self>) {
        self.drag_pos = Some(pos);
        self.extend_selection(pos, window);
        self.update_autoscroll(window, cx);
        cx.notify();
    }

    fn extend_selection(&mut self, pos: Point<Pixels>, window: &Window) {
        let Some(drag) = &self.drag else { return };
        let (unit, (o_start, o_end)) = (drag.unit, drag.origin);
        let Some(hit) = self.hit(pos, window) else { return };
        let (start, end) = match unit {
            DragUnit::Char => {
                let pos = hit.pos_at(hit.ix);
                (pos, pos)
            }
            DragUnit::Word => Self::word_at(&hit),
            DragUnit::Line => self.line_at(hit.line),
        };
        let rev = self.reversed;
        self.selection = Some(match unit {
            DragUnit::Char => (self.selection.map_or(start, |(anchor, _)| anchor), start),
            _ if before(start, o_start, rev) => (o_end, start),
            _ if before(end, o_end, rev) => (o_start, o_end),
            _ => (o_start, end),
        });
    }

    fn end_drag(&mut self, cx: &mut Context<Self>) {
        if self.drag.take().is_none() {
            return;
        }
        self.autoscroll = None;
        self.drag_pos = None;
        // Linux convention: selecting text makes it pastable with the middle button.
        #[cfg(any(target_os = "linux", target_os = "freebsd"))]
        if let Some((text, _, _)) = self.selected_text() {
            cx.write_to_primary(ClipboardItem::new_string(text));
        }
        cx.notify();
    }

    /// Keeps scrolling while a selection is dragged above or below the rows.
    fn update_autoscroll(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let outside = matches!(
            (self.drag_pos, self.list_bounds.get()),
            (Some(p), Some(b)) if p.y < b.top() || p.y >= b.bottom()
        );
        if !outside {
            self.autoscroll = None;
        } else if self.autoscroll.is_none() {
            self.autoscroll = Some(cx.spawn_in(window, async move |this, cx| {
                loop {
                    cx.background_executor().timer(Duration::from_millis(40)).await;
                    let more = this
                        .update_in(cx, |this, window, cx| this.autoscroll_tick(window, cx))
                        .unwrap_or(false);
                    if !more {
                        break;
                    }
                }
            }));
        }
    }

    fn autoscroll_tick(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let (Some(pos), Some(bounds), true) = (self.drag_pos, self.list_bounds.get(), self.drag.is_some()) else {
            self.autoscroll = None;
            return false;
        };
        let dist = if pos.y < bounds.top() {
            f32::from(pos.y - bounds.top())
        } else if pos.y >= bounds.bottom() {
            f32::from(pos.y - bounds.bottom())
        } else {
            self.autoscroll = None;
            return false;
        };
        // Faster the further the pointer is from the rows.
        let rows = (dist / self.line_h).clamp(-10., 10.);
        let step = if rows < 0. { rows.floor() } else { rows.ceil().max(1.) };
        self.set_scroll_offset(self.scroll_offset() + step as f64 * self.line_h as f64);
        self.follow = false;
        self.extend_selection(pos, window);
        cx.notify();
        true
    }

    fn select_all(&mut self) {
        let count = self.row_count();
        let (Some(index), Some(first), Some(last)) =
            (&self.index, self.row_to_line(0), self.row_to_line(count.saturating_sub(1)))
        else {
            return;
        };
        self.selection = Some((
            TextPos { line: first, byte: 0 },
            TextPos { line: last, byte: index.line(last).len() },
        ));
    }

    /// The selected text as it is in the file, without terminal escape sequences
    /// or other control characters (tabs are kept), in the order shown. Lines
    /// hidden by a filter are left out. Returns the text, the number of lines and
    /// whether it was cut at `MAX_COPY`.
    fn selected_text(&self) -> Option<(String, usize, bool)> {
        let (start, end) = self.selection_range()?;
        let index = self.index.as_ref()?;
        let rev = self.reversed;
        let mut out = String::new();
        let mut lines = 0;
        let mut row = self.line_to_row(start.line);
        while let Some(line) = self.row_to_line(row) {
            row += 1;
            if line_before(line, start.line, rev) {
                continue;
            }
            if line_before(end.line, line, rev) {
                break;
            }
            let bytes = index.line(line);
            let from = if line == start.line { start.byte.min(bytes.len()) } else { 0 };
            let to = if line == end.line { end.byte.min(bytes.len()) } else { bytes.len() };
            if lines > 0 {
                if out.len() >= MAX_COPY {
                    return Some((out, lines, true));
                }
                out.push('\n');
            }
            lines += 1;
            if !push_copy_text(&mut out, &bytes[from..to.max(from)], MAX_COPY) {
                return Some((out, lines, true));
            }
        }
        Some((out, lines, false))
    }

    /// Copies the selected text, or the whole selected line if no text is selected.
    fn copy(&mut self, cx: &mut Context<Self>) {
        let copied = self.selected_text().or_else(|| {
            let index = self.index.as_ref()?;
            let line = self.selected.filter(|&line| line < index.line_count())?;
            let mut text = String::new();
            let complete = push_copy_text(&mut text, index.line(line), MAX_COPY);
            Some((text, 1, !complete))
        });
        let Some((text, lines, truncated)) = copied else { return };
        let note = if truncated && lines == 1 {
            format!("Copied the first {} of the line", format_bytes(text.len() as u64))
        } else if truncated {
            format!("Copied the first {} lines ({})", format_int(lines), format_bytes(text.len() as u64))
        } else if lines > 1 {
            format!("Copied {} lines", format_int(lines))
        } else {
            format!("Copied {} characters", format_int(text.chars().count()))
        };
        cx.write_to_clipboard(ClipboardItem::new_string(text));
        self.show_note(note, cx);
    }

    fn show_note(&mut self, note: String, cx: &mut Context<Self>) {
        self.note = Some(note);
        self.note_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(Duration::from_millis(2500)).await;
            this.update(cx, |this, cx| {
                this.note = None;
                this.note_task = None;
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
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
                self.follow = self.follow && self.reversed;
                self.select_row(0);
            }
            ("end", _) | ("g", true) => {
                self.follow = self.follow && !self.reversed;
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
            ("escape", _) => {
                self.selected = None;
                self.selection = None;
            }
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
        let delta = ev.delta.pixel_delta(px(self.line_h));
        let (mut dx, dy) = (f32::from(delta.x), f32::from(delta.y));
        if ev.modifiers.shift && dx == 0. {
            dx = dy;
        } else if dy != 0. {
            self.set_scroll_offset(self.scroll_offset() - dy as f64);
            // Scrolling away from the newest lines stops following.
            if (dy > 0.) != self.reversed {
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
            self.scroll_to_latest();
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

    fn render_row(
        &self,
        row: usize,
        line: usize,
        index: &LogIndex,
        gutter_w: f32,
        t: &Theme,
    ) -> (impl IntoElement + use<>, usize) {
        let display = DisplayLine::new(index.line(line));
        let text = display.text.clone();
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

        if let Some((start, end)) = self.selection_range()
            && (start.line.min(end.line)..=start.line.max(end.line)).contains(&line)
        {
            let from = if line == start.line { display.display_offset(start.byte) } else { 0 };
            let to = if line == end.line { display.display_offset(end.byte) } else { text.len() };
            if from < to {
                highlights = overlay_background(highlights, from..to, t.text_selection);
            }
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
            .h(px(self.line_h))
            .w_full()
            .flex()
            .flex_row()
            .items_center()
            .when(selected, |d| d.bg(t.selection))
            .when(!selected, |d| d.hover(|s| s.bg(t.row_hover)))
            .child(div().w(px(STRIPE_W)).h_full().flex_none().bg(stripe))
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
            (self.scroll_offset() / (max_top as f64 * self.line_h as f64)) as f32
        };
        let markers = self.markers.clone();
        let reversed = self.reversed;
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
                        // Markers are in file order; newest first shows them upside down.
                        let i = if reversed { MARKER_BUCKETS - 1 - i } else { i };
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
                    let max = this.max_top() as f64 * this.line_h as f64;
                    this.set_scroll_offset(f * max);
                    this.follow = if this.reversed { f <= 0. } else { f >= 1. };
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
        self.gutter_w = gutter_w;

        let first = self.top_row;
        let last = (first + visible + 2).min(self.row_count());
        let mut widest = 0;
        let mut rows = Vec::with_capacity(last - first);
        for row in first..last {
            let Some(line) = self.row_to_line(row) else { break };
            let (el, cols) = self.render_row(row, line, &index, gutter_w, t);
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
            .font_family(self.font_family.clone())
            .text_size(px(self.font_size))
            .line_height(px(self.line_h))
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
            .child(self.render_selection_layer(cx))
    }

    /// An invisible layer over the rows that turns mouse presses and drags into
    /// text selections. Drags keep extending the selection outside the rows.
    fn render_selection_layer(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let entity = cx.entity();
        let bounds_cell = self.list_bounds.clone();
        canvas(
            move |bounds, window, _| {
                bounds_cell.set(Some(bounds));
                let text_area = Bounds::new(
                    bounds.origin,
                    size(bounds.size.width - px(SCROLLBAR_W), bounds.size.height),
                );
                window.insert_hitbox(text_area, HitboxBehavior::Normal)
            },
            move |_, hitbox, window, _| {
                window.set_cursor_style(CursorStyle::IBeam, &hitbox);
                window.on_mouse_event({
                    let entity = entity.clone();
                    move |ev: &MouseDownEvent, phase, window, cx| {
                        if phase == DispatchPhase::Bubble
                            && ev.button == MouseButton::Left
                            && hitbox.is_hovered(window)
                        {
                            entity.update(cx, |this, cx| this.on_text_mouse_down(ev, window, cx));
                        }
                    }
                });
                window.on_mouse_event({
                    let entity = entity.clone();
                    move |ev: &MouseMoveEvent, phase, window, cx| {
                        if phase != DispatchPhase::Bubble {
                            return;
                        }
                        entity.update(cx, |this, cx| {
                            if this.drag.is_none() {
                            } else if ev.pressed_button == Some(MouseButton::Left) {
                                this.on_text_drag(ev.position, window, cx);
                            } else {
                                // The button was released outside the window.
                                this.end_drag(cx);
                            }
                        });
                    }
                });
                window.on_mouse_event(move |ev: &MouseUpEvent, phase, _, cx| {
                    if phase == DispatchPhase::Bubble && ev.button == MouseButton::Left {
                        entity.update(cx, |this, cx| this.end_drag(cx));
                    }
                });
            },
        )
        .absolute()
        .top_0()
        .left_0()
        .size_full()
    }

    fn render_header(&self, window: &Window, t: &Theme, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let dark = cx.global::<Settings>().dark;
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
                    .font_family(self.font_family.clone())
                    .text_size(px(self.font_size))
                    .when(self.query.is_empty(), |d| {
                        d.text_color(t.text_muted).child(format!("Search  ({}F)", mod_key()))
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
                    .on_click(|_: &ClickEvent, window, cx| window.dispatch_action(Box::new(Open), cx)),
            )
            .child(
                chip("open-container", false, t)
                    .child("Containers…")
                    .on_click(|_: &ClickEvent, window, cx| {
                        window.dispatch_action(Box::new(OpenContainer), cx)
                    }),
            )
            .child(search_box)
            .child(mode)
            .child(levels)
            .child(div().flex_1())
            .child(
                chip("newest-first", self.reversed, t)
                    .child(if self.reversed { "● Newest first" } else { "○ Newest first" })
                    .on_click(|_: &ClickEvent, window, cx| {
                        window.dispatch_action(Box::new(ToggleNewestFirst), cx)
                    }),
            )
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
                    .child(if dark { "☀" } else { "☾" })
                    .on_click(|_: &ClickEvent, window, cx| window.dispatch_action(Box::new(ToggleTheme), cx)),
            )
            .child(
                chip("settings", false, t)
                    .child("⚙")
                    .on_click(|_: &ClickEvent, window, cx| window.dispatch_action(Box::new(OpenSettings), cx)),
            )
    }

    fn render_status(&self, t: &Theme) -> impl IntoElement + use<> {
        let mut left: Vec<String> = Vec::new();
        if let Some(index) = &self.index {
            left.push(printable(&index.path.to_string_lossy()));
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
        if let Some(note) = &self.note {
            right.insert(0, note.clone());
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

    fn render_placeholder(&self, t: &Theme, cx: &mut Context<Self>) -> impl IntoElement + use<> {
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
                            .map(|n| printable(&n.to_string_lossy()))
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
                        .child(format!("Open a log file with {}O or drop it here", mod_key())),
                )
                .child(div().text_color(t.text_muted).child(format!(
                    "or open the logs of a Docker or Podman container with {}",
                    if cfg!(target_os = "macos") { "⇧⌘O" } else { "Ctrl+Shift+O" }
                )))
                .when_some(self.error.clone(), |d, err| {
                    d.child(div().mt_2().text_color(t.error_text).child(err))
                })
                .child(self.render_recent(t, cx))
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

    /// Recently opened files, shown on an empty tab.
    fn render_recent(&self, t: &Theme, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let recent: Vec<PathBuf> = cx.try_global::<RecentFiles>().map(|r| r.0.clone()).unwrap_or_default();
        let hover = t.surface_hover;
        div()
            .mt_6()
            .w(px(460.))
            .flex()
            .flex_col()
            .gap_1()
            .when(!recent.is_empty(), |d| {
                d.child(div().px_2().text_xs().text_color(t.text_muted).child("Recent files"))
            })
            .children(recent.into_iter().take(10).enumerate().map(|(i, path)| {
                let name = printable(&path.file_name().unwrap_or(path.as_os_str()).to_string_lossy());
                let dir = printable(&path.parent().unwrap_or(Path::new("")).to_string_lossy());
                div()
                    .id(("recent", i))
                    .flex()
                    .items_center()
                    .gap_3()
                    .px_2()
                    .py_1()
                    .rounded_md()
                    .cursor_pointer()
                    .hover(move |s| s.bg(hover))
                    .child(div().flex_none().text_color(t.text).child(name))
                    .child(div().flex_1().truncate().text_xs().text_color(t.text_muted).child(dir))
                    .on_click(cx.listener(move |_, _: &ClickEvent, _, cx| {
                        cx.emit(LogViewEvent::OpenRequested(path.clone()));
                    }))
            }))
    }
}

impl Render for LogView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.sync_settings(window, cx);
        let t = cx.global::<Settings>().theme();

        let header = self.render_header(window, &t, cx);
        let status = self.render_status(&t);
        let body = if self.index.is_some() && self.loading.is_none() {
            self.render_list(window, &t, cx).into_any_element()
        } else {
            self.render_placeholder(&t, cx).into_any_element()
        };

        div()
            .id("log-view")
            .key_context("LogView")
            .flex_1()
            .min_h_0()
            .w_full()
            .flex()
            .flex_col()
            .bg(t.bg)
            .text_color(t.text)
            .text_sm()
            .on_action(cx.listener(|this, _: &FocusSearch, window, cx| {
                window.focus(&this.search_focus);
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
            .on_action(cx.listener(|this, _: &Copy, _, cx| this.copy(cx)))
            .on_action(cx.listener(|this, _: &SelectAll, _, cx| {
                this.select_all();
                cx.notify();
            }))
            .on_action(cx.listener(|this, _: &NextMatch, _, cx| {
                this.jump_to_hit(true);
                cx.notify();
            }))
            .on_action(cx.listener(|this, _: &PrevMatch, _, cx| {
                this.jump_to_hit(false);
                cx.notify();
            }))
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

/// Maps a display row to a row in file order (the mapping is its own inverse).
fn flip_row(row: usize, count: usize, reversed: bool) -> Option<usize> {
    (row < count).then(|| if reversed { count - 1 - row } else { row })
}

/// Whether `a` is shown above `b`.
fn line_before(a: usize, b: usize, reversed: bool) -> bool {
    if reversed { a > b } else { a < b }
}

/// Whether text position `a` comes before `b` on screen.
fn before(a: TextPos, b: TextPos, reversed: bool) -> bool {
    if a.line == b.line {
        a.byte < b.byte
    } else {
        line_before(a.line, b.line, reversed)
    }
}

/// Calls `f` with each character of a raw line and the byte offset it starts at.
/// Invalid UTF-8 becomes U+FFFD and terminal escape sequences (`ESC [ ... final`,
/// or a lone ESC) are skipped. Stops early when `f` returns false.
fn for_each_visible_char(bytes: &[u8], mut f: impl FnMut(char, usize) -> bool) {
    #[derive(PartialEq)]
    enum Esc {
        None,
        Start,
        Csi,
    }
    let mut esc = Esc::None;
    let mut offset = 0;
    for chunk in bytes.utf8_chunks() {
        let base = offset;
        let valid = chunk.valid();
        offset += valid.len() + chunk.invalid().len();
        let invalid = (!chunk.invalid().is_empty()).then_some(('\u{fffd}', base + valid.len()));
        for (c, raw) in valid.char_indices().map(|(i, c)| (c, base + i)).chain(invalid) {
            match esc {
                Esc::Csi => {
                    if ('@'..='~').contains(&c) {
                        esc = Esc::None;
                    }
                    continue;
                }
                Esc::Start if c == '[' => {
                    esc = Esc::Csi;
                    continue;
                }
                _ => esc = Esc::None,
            }
            if c == '\x1b' {
                esc = Esc::Start;
                continue;
            }
            if !f(c, raw) {
                return;
            }
        }
    }
}

/// A raw line as shown on screen: escape sequences stripped, tabs expanded,
/// control characters shown as `·`, length capped at `MAX_COLS`. `map` records
/// the raw byte each displayed character came from, so text selected on screen
/// can be copied from the original bytes.
struct DisplayLine {
    text: String,
    /// (display byte offset, raw byte offset) of every displayed character.
    map: Vec<(usize, usize)>,
}

impl DisplayLine {
    fn new(bytes: &[u8]) -> DisplayLine {
        let shown = &bytes[..bytes.len().min(MAX_COLS * 4)];
        let mut text = String::with_capacity(shown.len());
        let mut map = Vec::with_capacity(shown.len());
        let mut cols = 0;
        let mut truncated = shown.len() < bytes.len();
        for_each_visible_char(shown, |c, raw| {
            if cols >= MAX_COLS {
                truncated = true;
                return false;
            }
            let (shown, width) = match c {
                '\t' => (' ', 4 - cols % 4),
                c if c.is_control() => ('·', 1),
                c => (c, 1),
            };
            for _ in 0..width {
                map.push((text.len(), raw));
                text.push(shown);
            }
            cols += width;
            true
        });
        if truncated {
            // Selecting the ellipsis selects the rest of the line.
            map.push((text.len(), bytes.len()));
            text.push('…');
        }
        DisplayLine { text, map }
    }

    /// Raw byte offset for a character boundary in `text`; the end of the text is
    /// the end of the raw line.
    fn raw_offset(&self, ix: usize, raw_len: usize) -> usize {
        let i = self.map.partition_point(|&(d, _)| d < ix);
        match self.map.get(i) {
            Some(&(_, raw)) => raw,
            None => raw_len,
        }
    }

    /// Display offset of the first character that starts at or after raw byte `raw`.
    fn display_offset(&self, raw: usize) -> usize {
        let i = self.map.partition_point(|&(_, r)| r < raw);
        self.map.get(i).map_or(self.text.len(), |&(d, _)| d)
    }
}

/// Appends a raw line slice as copied text: what is shown, but with tabs kept and
/// nothing cut short. Escape sequences and other control characters are dropped,
/// so pasting into a terminal can't run anything hidden in the log. Stops before
/// `out` grows past `limit` bytes and returns false if it had to.
fn push_copy_text(out: &mut String, bytes: &[u8], limit: usize) -> bool {
    let mut complete = true;
    for_each_visible_char(bytes, |c, _| {
        if c == '\t' || !c.is_control() {
            if out.len() + c.len_utf8() > limit {
                complete = false;
                return false;
            }
            out.push(c);
        }
        true
    });
    complete
}

/// The character boundary in a shaped line closest to `x`.
fn closest_boundary(line: &ShapedLine, x: Pixels) -> usize {
    let glyphs: Vec<(usize, Pixels)> = line
        .runs
        .iter()
        .flat_map(|run| run.glyphs.iter().map(|g| (g.index, g.position.x)))
        .collect();
    for (i, &(ix, left)) in glyphs.iter().enumerate() {
        let next = glyphs.get(i + 1);
        let right = next.map_or(line.width, |g| g.1);
        if x < right {
            return if x < left + (right - left) / 2. {
                ix
            } else {
                next.map_or(line.len, |g| g.0)
            };
        }
    }
    line.len
}

/// Byte range of the word around boundary `ix`, or of the single character there
/// if it isn't part of a word.
fn word_range(text: &str, ix: usize) -> Range<usize> {
    let is_word = |c: char| c.is_alphanumeric() || c == '_';
    let ix = ix.min(text.len());
    let Some(c) = text[ix..].chars().next() else {
        return ix..ix;
    };
    if !is_word(c) {
        return ix..ix + c.len_utf8();
    }
    let start = text[..ix]
        .char_indices()
        .rev()
        .take_while(|&(_, c)| is_word(c))
        .last()
        .map_or(ix, |(i, _)| i);
    let end = text[ix..]
        .char_indices()
        .find(|&(_, c)| !is_word(c))
        .map_or(text.len(), |(i, _)| ix + i);
    start..end
}

/// Gives `range` the background `bg`, on top of `highlights` (sorted, not
/// overlapping), splitting highlights where the range starts or ends.
fn overlay_background(
    highlights: Vec<(Range<usize>, HighlightStyle)>,
    range: Range<usize>,
    bg: Hsla,
) -> Vec<(Range<usize>, HighlightStyle)> {
    let with_bg = |style: HighlightStyle| HighlightStyle {
        background_color: Some(bg),
        ..style
    };
    let mut out = Vec::with_capacity(highlights.len() + 3);
    // Start of the part of `range` not yet emitted.
    let mut pos = range.start;
    for (r, style) in highlights {
        if r.end <= range.start {
            out.push((r, style));
            continue;
        }
        if r.start >= range.end {
            if pos < range.end {
                out.push((pos..range.end, with_bg(HighlightStyle::default())));
                pos = range.end;
            }
            out.push((r, style));
            continue;
        }
        if r.start < range.start {
            out.push((r.start..range.start, style));
        }
        let (from, to) = (r.start.max(range.start), r.end.min(range.end));
        if pos < from {
            out.push((pos..from, with_bg(HighlightStyle::default())));
        }
        out.push((from..to, with_bg(style)));
        pos = to;
        if r.end > range.end {
            out.push((range.end..r.end, style));
        }
    }
    if pos < range.end {
        out.push((pos..range.end, with_bg(HighlightStyle::default())));
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

/// A file name or path for display: control and bidi formatting characters are
/// shown as `·`, so a crafted name can't break the layout or reorder what is
/// shown around it (in the window title too, where the system applies bidi).
pub fn printable(text: &str) -> String {
    let hidden = |c: char| {
        c.is_control()
            || matches!(c, '\u{061c}' | '\u{200e}' | '\u{200f}')
            || ('\u{202a}'..='\u{202e}').contains(&c)
            || ('\u{2066}'..='\u{2069}').contains(&c)
    };
    text.chars().map(|c| if hidden(c) { '·' } else { c }).collect()
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

    fn display(bytes: &[u8]) -> String {
        DisplayLine::new(bytes).text
    }

    fn copied(bytes: &[u8]) -> String {
        let mut out = String::new();
        assert!(push_copy_text(&mut out, bytes, usize::MAX));
        out
    }

    #[test]
    fn printable_names_hide_controls_and_bidi_overrides() {
        assert_eq!(printable("we\u{202e}gol.exe\nx\x1b[31m.log"), "we·gol.exe·x·[31m.log");
        assert_eq!(printable("\u{2066}a\u{2069} 日本 é.log"), "·a· 日本 é.log");
    }

    #[test]
    fn display_text_cleans_lines() {
        assert_eq!(display(b"\x1b[31mred\x1b[0m\tx"), "red x");
        assert_eq!(display(b"a\x00b"), "a·b");
        assert_eq!(display(&[0xff, b'a']), "\u{fffd}a");
        assert_eq!(display(b"a\x1bb"), "ab");
        let long = vec![b'x'; MAX_COLS + 10];
        let text = display(&long);
        assert_eq!(text.chars().count(), MAX_COLS + 1);
        assert!(text.ends_with('…'));
        assert_eq!(display(&vec![b'x'; MAX_COLS]).chars().count(), MAX_COLS);
    }

    #[test]
    fn display_maps_back_to_raw_bytes() {
        // "\x1b[1m" (4 bytes) hidden, the tab after "ab" shown as 2 spaces, "é" is 2 bytes.
        let raw = "\x1b[1mab\tcé!".as_bytes();
        let d = DisplayLine::new(raw);
        assert_eq!(d.text, "ab  cé!");
        assert_eq!(d.raw_offset(0, raw.len()), 4); // 'a'
        assert_eq!(d.raw_offset(2, raw.len()), 6); // first space -> the tab
        assert_eq!(d.raw_offset(3, raw.len()), 6); // second space -> the tab
        assert_eq!(d.raw_offset(4, raw.len()), 7); // 'c'
        assert_eq!(d.raw_offset(5, raw.len()), 8); // 'é'
        assert_eq!(d.raw_offset(7, raw.len()), 10); // '!'
        assert_eq!(d.raw_offset(d.text.len(), raw.len()), raw.len());
        assert_eq!(d.display_offset(0), 0);
        assert_eq!(d.display_offset(6), 2);
        assert_eq!(d.display_offset(7), 4);
        assert_eq!(d.display_offset(raw.len()), d.text.len());

        // The ellipsis of a cut line stands for the rest of it.
        let long = vec![b'x'; MAX_COLS + 10];
        let d = DisplayLine::new(&long);
        let ellipsis = d.text.len() - '…'.len_utf8();
        assert_eq!(d.raw_offset(ellipsis, long.len()), long.len());
    }

    #[test]
    fn copy_keeps_text_and_tabs_but_drops_control_characters() {
        assert_eq!(copied(b"\x1b[31mERROR\x1b[0m\tdisk full"), "ERROR\tdisk full");
        // A carriage return or a bracketed-paste end marker could run a command
        // when pasted into a terminal.
        assert_eq!(copied(b"ls\x1b[201~\rrm -rf ~\r"), "lsrm -rf ~");
        assert_eq!(copied(b"a\x00b\x7fc"), "abc");
        assert_eq!(copied(&[b'o', 0xff, b'k']), "o\u{fffd}k");
        let long = vec![b'y'; MAX_COLS * 5];
        assert_eq!(copied(&long).len(), long.len());
    }

    #[test]
    fn newest_first_flips_rows_and_screen_order() {
        assert_eq!(flip_row(0, 5, false), Some(0));
        assert_eq!(flip_row(0, 5, true), Some(4));
        assert_eq!(flip_row(4, 5, true), Some(0));
        assert_eq!(flip_row(5, 5, true), None);
        assert_eq!(flip_row(0, 0, true), None);

        let p = |line, byte| TextPos { line, byte };
        // Oldest first: earlier lines come first.
        assert!(before(p(1, 9), p(2, 0), false));
        // Newest first: later lines come first, bytes within a line keep their order.
        assert!(before(p(2, 0), p(1, 9), true));
        assert!(before(p(2, 3), p(2, 7), true));
        assert!(!before(p(2, 7), p(2, 3), true));
    }

    /// Small deterministic RNG for the property tests below.
    struct Rng(u64);

    impl Rng {
        fn next(&mut self) -> u64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            self.0
        }
        fn below(&mut self, n: usize) -> usize {
            (self.next() % n.max(1) as u64) as usize
        }
    }

    /// Iterations for the property tests; raise with LOGSHIP_FUZZ_ITERS for a long run.
    fn fuzz_iters(default: usize) -> usize {
        std::env::var("LOGSHIP_FUZZ_ITERS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(default)
    }

    /// A raw log line built from pieces that stress the display and copy code:
    /// escape sequences, control characters, invalid and truncated UTF-8, wide,
    /// combining, zero-width and bidi characters, tabs, level words.
    fn hostile_line(rng: &mut Rng) -> Vec<u8> {
        const PIECES: &[&[u8]] = &[
            b"a", b"Z", b"0", b" ", b"\t", b"\x1b", b"\x1b[", b"[31m", b"\x1b[0m", b"\x1b]0;t\x07",
            b";", b"~", b"\r", b"\x00", b"\x7f", b"\x07", b"\xff", b"\xfe", b"\xc0", b"\x80",
            b"\xe6\x97", "é".as_bytes(), "日本".as_bytes(), "😀".as_bytes(), "\u{202e}".as_bytes(),
            "\u{200b}".as_bytes(), "e\u{301}".as_bytes(), b"\xc2\x9b", b"ERROR", b"level=warn",
            b"[info]", b"id_42", b"..", b"=",
        ];
        let len = match rng.below(10) {
            0 => rng.below(6000),
            1 => 0,
            _ => rng.below(80),
        };
        let mut out = Vec::new();
        for _ in 0..len {
            out.extend_from_slice(PIECES[rng.below(PIECES.len())]);
        }
        out
    }

    #[test]
    fn display_and_copy_hold_their_invariants_on_hostile_lines() {
        let mut rng = Rng(0x9e37_79b9_7f4a_7c15);
        for _ in 0..fuzz_iters(3000) {
            let raw = hostile_line(&mut rng);
            let d = DisplayLine::new(&raw);
            // Shown text: no control characters, capped length, monotonic map.
            assert!(!d.text.chars().any(|c| c.is_control()), "{:?}", d.text);
            assert!(d.text.chars().count() <= MAX_COLS + 1);
            let mut prev = None;
            for &(disp, r) in &d.map {
                assert!(d.text.is_char_boundary(disp));
                assert!(r <= raw.len());
                if let Some((pd, pr)) = prev {
                    assert!(disp > pd && r >= pr);
                }
                prev = Some((disp, r));
            }
            // Every boundary maps to a raw offset and back without leaving bounds.
            for ix in (0..=d.text.len()).filter(|&i| d.text.is_char_boundary(i)) {
                let r = d.raw_offset(ix, raw.len());
                assert!(r <= raw.len());
                let back = d.display_offset(r);
                assert!(back <= d.text.len() && d.text.is_char_boundary(back));
                let w = word_range(&d.text, ix);
                assert!(w.start <= w.end && w.end <= d.text.len());
                assert!(d.text.is_char_boundary(w.start) && d.text.is_char_boundary(w.end));
            }
            // Copied text never carries control characters other than tabs.
            let c = copied(&raw);
            assert!(c.chars().all(|ch| ch == '\t' || !ch.is_control()), "{c:?}");
            // Copying any slice of the line is safe, wherever it is cut.
            let (a, b) = (rng.below(raw.len() + 1), rng.below(raw.len() + 1));
            let _ = copied(&raw[a.min(b)..a.max(b)]);
        }
    }

    #[test]
    fn overlay_background_keeps_highlights_sorted_and_disjoint() {
        let mut rng = Rng(42);
        for _ in 0..fuzz_iters(3000) {
            let len = rng.below(200) + 1;
            let mut highlights = Vec::new();
            let mut pos = 0;
            while pos < len && highlights.len() < 8 {
                let start = pos + rng.below(5);
                let end = (start + 1 + rng.below(10)).min(len);
                if start >= end {
                    break;
                }
                highlights.push((start..end, HighlightStyle::default()));
                pos = end;
            }
            let (a, b) = (rng.below(len + 1), rng.below(len + 1));
            let range = a.min(b)..a.max(b);
            if range.is_empty() {
                continue;
            }
            let out = overlay_background(highlights.clone(), range.clone(), gpui::red());
            let mut last_end = 0;
            for (r, style) in &out {
                assert!(r.start < r.end && r.start >= last_end && r.end <= len, "{out:?}");
                last_end = r.end;
                let inside = r.start >= range.start && r.end <= range.end;
                assert_eq!(style.background_color.is_some(), inside, "{out:?}");
            }
            // The selected range is fully covered.
            let covered: usize = out
                .iter()
                .filter(|(_, s)| s.background_color.is_some())
                .map(|(r, _)| r.len())
                .sum();
            assert_eq!(covered, range.len());
        }
    }

    #[test]
    fn finds_words() {
        let text = "id=42 user_name:bob";
        assert_eq!(&text[word_range(text, 0)], "id");
        assert_eq!(&text[word_range(text, 1)], "id");
        assert_eq!(&text[word_range(text, 2)], "=");
        assert_eq!(&text[word_range(text, 8)], "user_name");
        assert_eq!(&text[word_range(text, 17)], "bob");
        assert_eq!(word_range(text, text.len()), text.len()..text.len());
    }

    #[test]
    fn selection_overlays_existing_highlights() {
        let bold = HighlightStyle {
            font_weight: Some(FontWeight::BOLD),
            ..Default::default()
        };
        let bg = gpui::red();
        let out = overlay_background(vec![(2..4, bold), (8..10, bold)], 3..9, bg);
        let ranges: Vec<_> = out.iter().map(|(r, _)| r.clone()).collect();
        assert_eq!(ranges, vec![2..3, 3..4, 4..8, 8..9, 9..10]);
        let has_bg: Vec<_> = out.iter().map(|(_, s)| s.background_color.is_some()).collect();
        assert_eq!(has_bg, vec![false, true, true, true, false]);
        assert_eq!(out[1].1.font_weight, Some(FontWeight::BOLD));

        let out = overlay_background(vec![], 0..5, bg);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].0, 0..5);
    }

    #[test]
    fn formats_numbers() {
        assert_eq!(format_int(1234567), "1,234,567");
        assert_eq!(format_int(12), "12");
        assert_eq!(format_compact(1500), "1.5k");
        assert_eq!(format_bytes(1536), "1.5 KB");
    }
}
