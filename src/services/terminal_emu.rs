//! 基于 `alacritty_terminal` 的现代 VT100 / xterm-256color 终端仿真内核
//!
//! 核心能力：
//! - 完整事件闭环：`Event::Title`、`Event::PtyWrite`（DSR/DA/OSC 10-11 实时回包）、
//!   `Event::ClipboardStore`（OSC 52 远端复制）与 `Event::Bell`
//! - 流式协议观测：OSC 7 / OSC 1337 实时工作目录追踪、DEC `?2026h/?2026l` 同步输出无撕裂合帧
//! - 完整 SGR 样式（粗体/斜体/暗淡/单双波浪下划线/删除线/隐藏/反色）、OSC 8 显式超链接与 URL/IP 识别
//! - 原生 `alacritty_terminal` 字符/词/整行/矩形块选区与 `RegexSearch` 历史回滚搜索
//! - 按行内容哈希（`row_hash`）驱动 GPUI `TerminalElement` 增量文本 Shaping 缓存

use alacritty_terminal::Term;
use alacritty_terminal::event::{Event, EventListener};
use alacritty_terminal::grid::{Dimensions, Scroll};
use alacritty_terminal::index::{Column, Direction, Line, Point, Side};
use alacritty_terminal::selection::{Selection, SelectionType};
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::term::search::{RegexIter, RegexSearch};
use alacritty_terminal::term::{Config, TermMode};
use alacritty_terminal::vte::ansi::{self, Color, CursorShape, NamedColor};
use gpui::{Hsla, Rgba};
use std::collections::HashSet;
use std::hash::{Hash, Hasher};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// 内部事件代理（捕获标题变更、PTY 查询回包、OSC 52 剪贴板与响铃）
#[derive(Clone, Default)]
struct HoloEventProxy {
    title: Arc<Mutex<Option<String>>>,
    pty_writes: Arc<Mutex<Vec<String>>>,
    clipboard_stores: Arc<Mutex<Vec<String>>>,
    bell_count: Arc<AtomicU64>,
}

impl EventListener for HoloEventProxy {
    fn send_event(&self, event: Event) {
        match event {
            Event::Title(title) => {
                if let Ok(mut guard) = self.title.lock() {
                    *guard = Some(title);
                }
            }
            Event::ResetTitle => {
                if let Ok(mut guard) = self.title.lock() {
                    *guard = None;
                }
            }
            Event::PtyWrite(text) => {
                if let Ok(mut guard) = self.pty_writes.lock() {
                    guard.push(text);
                }
            }
            Event::ClipboardStore(_, text) => {
                if let Ok(mut guard) = self.clipboard_stores.lock() {
                    guard.push(text);
                }
            }
            Event::Bell => {
                self.bell_count.fetch_add(1, Ordering::Relaxed);
            }
            _ => {}
        }
    }
}

/// 终端行列尺寸描述
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TermDimensions {
    pub cols: usize,
    pub rows: usize,
}

impl Dimensions for TermDimensions {
    fn total_lines(&self) -> usize {
        self.rows
    }

    fn screen_lines(&self) -> usize {
        self.rows
    }

    fn columns(&self) -> usize {
        self.cols
    }
}

/// 单行中的下划线样式
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum UnderlineKind {
    None,
    Single,
    Double,
    Curly,
    Dotted,
    Dashed,
}

/// 单个可渲染终端网格单元格（直供 GPUI `TerminalElement` 绘制）
#[derive(Debug, Clone)]
pub struct RenderCell {
    pub col: usize,
    pub ch: char,
    pub width: u8,
    pub fg: Option<Hsla>,
    pub bg: Option<Hsla>,
    pub underline_color: Option<Hsla>,
    pub bold: bool,
    pub italic: bool,
    pub dim: bool,
    pub underline: UnderlineKind,
    pub strikeout: bool,
    pub hidden: bool,
    pub selected: bool,
    pub hyperlink: Option<Arc<str>>,
}

/// 单行中检测到的可点击链接（OSC 8 显式超链接或 URL/IP 正则匹配）
#[derive(Debug, Clone)]
pub struct DetectedLink {
    pub col_start: usize,
    pub col_end: usize,
    pub uri: String,
}

/// 单行完整渲染数据（含 `row_hash` 供 `TerminalElement` 增量 Shaping 缓存）
#[derive(Debug, Clone)]
pub struct RenderRow {
    pub grid_line: i32,
    pub row_hash: u64,
    pub cells: Vec<RenderCell>,
    pub links: Vec<DetectedLink>,
}

/// 一帧完整渲染网格快照（直供自定义 `gpui::Element` 高速 GPU 绘制）
#[derive(Debug, Clone)]
pub struct TerminalRenderGrid {
    pub cols: usize,
    pub rows: usize,
    pub history_size: usize,
    pub display_offset: usize,
    pub grid_rows: Vec<RenderRow>,
    pub cursor_row: usize,
    pub cursor_col: usize,
    pub cursor_visible: bool,
    pub cursor_shape: CursorShape,
}

/// 终端内正则/关键字搜索命中范围（以 `alacritty_terminal` 网格行号表示，支持历史回滚）
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TermSearchMatch {
    pub start_line: i32,
    pub start_col: usize,
    pub end_line: i32,
    pub end_col: usize,
}

/// 轻量流式协议观测器：提取 OSC 7 / OSC 1337 实时路径与 DEC `?2026h/?2026l` 同步输出状态
#[derive(Default)]
struct OscStreamScanner {
    state: ScanState,
    buf: Vec<u8>,
    sync_update_deadline: Option<Instant>,
    detected_cwd: Option<String>,
    cwd_dirty: bool,
}

#[derive(Default, Clone, Copy, PartialEq, Eq)]
enum ScanState {
    #[default]
    Normal,
    Esc,
    Osc,
    OscEsc,
    Csi,
}

impl OscStreamScanner {
    fn feed(&mut self, bytes: &[u8]) {
        for &b in bytes {
            match self.state {
                ScanState::Normal => {
                    if b == 0x1b {
                        self.state = ScanState::Esc;
                    }
                }
                ScanState::Esc => match b {
                    b']' => {
                        self.buf.clear();
                        self.state = ScanState::Osc;
                    }
                    b'[' => {
                        self.buf.clear();
                        self.state = ScanState::Csi;
                    }
                    0x1b => {
                        self.state = ScanState::Esc;
                    }
                    _ => {
                        self.state = ScanState::Normal;
                    }
                },
                ScanState::Osc => {
                    if b == 0x07 {
                        self.handle_osc();
                        self.buf.clear();
                        self.state = ScanState::Normal;
                    } else if b == 0x1b {
                        self.state = ScanState::OscEsc;
                    } else if self.buf.len() < 2048 {
                        self.buf.push(b);
                    } else {
                        self.buf.clear();
                        self.state = ScanState::Normal;
                    }
                }
                ScanState::OscEsc => {
                    if b == b'\\' {
                        self.handle_osc();
                        self.buf.clear();
                        self.state = ScanState::Normal;
                    } else {
                        self.buf.clear();
                        self.state = ScanState::Normal;
                    }
                }
                ScanState::Csi => {
                    if (0x40..=0x7e).contains(&b) {
                        self.buf.push(b);
                        self.handle_csi();
                        self.buf.clear();
                        self.state = ScanState::Normal;
                    } else if self.buf.len() < 32 {
                        self.buf.push(b);
                    } else {
                        self.buf.clear();
                        self.state = ScanState::Normal;
                    }
                }
            }
        }
    }

    fn handle_csi(&mut self) {
        if self.buf.as_slice() == b"?2026h" {
            self.sync_update_deadline = Some(Instant::now() + Duration::from_millis(100));
        } else if self.buf.as_slice() == b"?2026l" {
            self.sync_update_deadline = None;
        }
    }

    fn handle_osc(&mut self) {
        let Ok(payload) = std::str::from_utf8(&self.buf) else {
            return;
        };
        // OSC 7;file://host/path/to/dir
        if let Some(rest) = payload.strip_prefix("7;") {
            if let Some(path) = parse_osc7_file_uri(rest.trim()) {
                if self.detected_cwd.as_deref() != Some(path.as_str()) {
                    self.detected_cwd = Some(path);
                    self.cwd_dirty = true;
                }
            }
            return;
        }
        // OSC 1337;CurrentDir=/path/to/dir (iTerm2 / Tabby / Ghostty 兼容)
        if let Some(rest) = payload.strip_prefix("1337;") {
            for part in rest.split(';') {
                if let Some(dir) = part.strip_prefix("CurrentDir=") {
                    let clean = dir.trim();
                    if !clean.is_empty() && self.detected_cwd.as_deref() != Some(clean) {
                        self.detected_cwd = Some(clean.to_string());
                        self.cwd_dirty = true;
                    }
                }
            }
        }
    }

    fn take_cwd_update(&mut self) -> Option<String> {
        if self.cwd_dirty {
            self.cwd_dirty = false;
            self.detected_cwd.clone()
        } else {
            None
        }
    }

    fn clear_sync(&mut self) {
        self.sync_update_deadline = None;
    }
}

/// 单窗格独立终端仿真器实例（支持增量哈希缓存与同步输出无撕裂渲染）
pub struct TerminalEmulator {
    term: Term<HoloEventProxy>,
    parser: ansi::Processor,
    proxy: HoloEventProxy,
    scanner: OscStreamScanner,
    dims: TermDimensions,
    generation: u64,
    cached_grid: std::cell::RefCell<Option<(bool, u64, Arc<TerminalRenderGrid>)>>,
    last_committed_grid: std::cell::RefCell<Option<(bool, Arc<TerminalRenderGrid>)>>,
}

impl TerminalEmulator {
    #[cfg(test)]
    pub fn new(cols: usize, rows: usize) -> Self {
        Self::new_with_history(cols, rows, 4000)
    }

    pub fn new_with_history(cols: usize, rows: usize, scrollback_lines: usize) -> Self {
        let dims = TermDimensions {
            cols: cols.max(20),
            rows: rows.max(6),
        };
        let proxy = HoloEventProxy::default();
        let config = Config {
            scrolling_history: scrollback_lines.clamp(500, 50_000),
            ..Config::default()
        };
        let term = Term::new(config, &dims, proxy.clone());
        Self {
            term,
            parser: ansi::Processor::new(),
            proxy,
            scanner: OscStreamScanner::default(),
            dims,
            generation: 1,
            cached_grid: std::cell::RefCell::new(None),
            last_committed_grid: std::cell::RefCell::new(None),
        }
    }

    #[inline]
    fn bump_generation(&mut self) {
        self.generation = self.generation.wrapping_add(1);
        *self.cached_grid.borrow_mut() = None;
    }

    /// 读取当前终端状态机全部模式标志（`APP_CURSOR`、`BRACKETED_PASTE`、`MOUSE_MODE`、`ALT_SCREEN` 等）
    pub fn mode(&self) -> TermMode {
        *self.term.mode()
    }

    /// 断线或重连前重置远端可能残留的私有模式（鼠标追踪、括弧粘贴、备用屏、隐藏光标，规避 Tabby 残留模式卡死）
    pub fn reset_terminal_modes(&mut self) {
        self.scanner.clear_sync();
        self.feed_bytes(
            b"\x1b[?1000l\x1b[?1002l\x1b[?1003l\x1b[?1006l\x1b[?1004l\x1b[?2004l\x1b[?2026l\x1b[?1049l\x1b[?25h",
        );
    }

    /// 直接读取最近 OSC 标题（O(1)）
    pub fn detected_title(&self) -> Option<String> {
        self.proxy.title.lock().ok().and_then(|g| g.clone())
    }

    /// 消费最近一次 OSC 7 / OSC 1337 汇报的精确工作目录
    pub fn take_cwd_update(&mut self) -> Option<String> {
        self.scanner.take_cwd_update()
    }

    /// 排空终端仿真器需要写回远端 PTY 的应答序列（DSR `\x1b[6n`、DA1/DA2、OSC 10/11 等）
    pub fn drain_pty_writes(&self) -> Vec<String> {
        self.proxy
            .pty_writes
            .lock()
            .map(|mut g| std::mem::take(&mut *g))
            .unwrap_or_default()
    }

    /// 排空远端通过 OSC 52 请求写入本机剪贴板的文本
    pub fn drain_clipboard_stores(&self) -> Vec<String> {
        self.proxy
            .clipboard_stores
            .lock()
            .map(|mut g| std::mem::take(&mut *g))
            .unwrap_or_default()
    }

    /// 读取并清零响铃计数
    pub fn take_bell_count(&self) -> u64 {
        self.proxy.bell_count.swap(0, Ordering::Relaxed)
    }

    /// 喂入来自 SSH PTY 或本地提示的原始字节流
    pub fn feed_bytes(&mut self, bytes: &[u8]) {
        if bytes.is_empty() {
            return;
        }
        self.scanner.feed(bytes);
        self.parser.advance(&mut self.term, bytes);
        self.bump_generation();
    }

    /// 写入本地状态提示行（自动补 CRLF）
    pub fn write_notice(&mut self, text: &str) {
        let formatted = format!("\x1b[90m{text}\x1b[0m\r\n");
        self.feed_bytes(formatted.as_bytes());
    }

    /// 彻底清屏并清空历史回滚缓冲与光标状态（用于 SSH 连接成功进入真实远端会话前清理本地连接提示）
    pub fn clear_screen_and_history(&mut self) {
        self.term.selection = None;
        self.scanner.sync_update_deadline = None;
        // VT100 RIS (\x1bc) + 显式擦除屏幕与回滚缓冲 (\x1b[2J\x1b[3J\x1b[H)
        self.feed_bytes(b"\x1bc\x1b[2J\x1b[3J\x1b[H");
    }

    /// 调整终端行列尺寸
    pub fn resize(&mut self, cols: usize, rows: usize) {
        let next = TermDimensions {
            cols: cols.max(20),
            rows: rows.max(6),
        };
        if next.cols != self.dims.cols || next.rows != self.dims.rows {
            self.dims = next;
            self.term.resize(next);
            self.bump_generation();
        }
    }

    pub fn cols(&self) -> u32 {
        self.dims.cols as u32
    }

    pub fn rows(&self) -> u32 {
        self.dims.rows as u32
    }

    /// 滚轮浏览历史回滚缓冲（delta > 0 向上翻历史，delta < 0 向下回最新；天然保持 Scroll Pinning）
    pub fn scroll_display(&mut self, delta_lines: i32) -> bool {
        let before = self.term.grid().display_offset();
        self.term.scroll_display(Scroll::Delta(delta_lines));
        let after = self.term.grid().display_offset();
        if before != after {
            self.bump_generation();
            true
        } else {
            false
        }
    }

    /// 滚回终端最新输出底部（键入命令或粘贴时调用）
    pub fn scroll_to_bottom(&mut self) {
        if self.term.grid().display_offset() != 0 {
            self.term.scroll_display(Scroll::Bottom);
            self.bump_generation();
        }
    }

    /// 将指定网格行号居中滚动到当前视口（供 `⌘F` 搜索跳转命中项）
    pub fn scroll_to_grid_line(&mut self, target_line: i32) {
        let history = self.term.grid().history_size() as i32;
        let rows = self.dims.rows as i32;
        let desired_offset = (rows / 2 - target_line).clamp(0, history) as usize;
        let cur_offset = self.term.grid().display_offset();
        if desired_offset != cur_offset {
            let delta = desired_offset as i32 - cur_offset as i32;
            self.term.scroll_display(Scroll::Delta(delta));
            self.bump_generation();
        }
    }

    /// 当前视口相对底部的历史回滚行数（0 表示处于最新底部）
    pub fn display_offset(&self) -> usize {
        self.term.grid().display_offset()
    }

    /// 将视口坐标 `(viewport_row, col)` 转换为 `alacritty_terminal` 网格 `Point`
    fn viewport_to_point(&self, col: usize, viewport_row: usize) -> Point {
        let display_offset = self.term.grid().display_offset() as i32;
        let max_col = self.dims.cols.saturating_sub(1);
        let max_row = self.dims.rows.saturating_sub(1);
        let clamped_row = viewport_row.min(max_row) as i32;
        let grid_line = clamped_row - display_offset;
        Point::new(Line(grid_line), Column(col.min(max_col)))
    }

    /// 启动原生选区（支持单字拖动 Simple、双击选词 Semantic、三击选行 Lines、`Option` 块选 Block）
    pub fn start_selection(&mut self, col: usize, viewport_row: usize, ty: SelectionType, side: Side) {
        let point = self.viewport_to_point(col, viewport_row);
        self.term.selection = Some(Selection::new(ty, point, side));
        self.bump_generation();
    }

    /// 更新当前选区终点
    pub fn update_selection(&mut self, col: usize, viewport_row: usize, side: Side) -> bool {
        let point = self.viewport_to_point(col, viewport_row);
        if let Some(sel) = self.term.selection.as_mut() {
            sel.update(point, side);
            self.bump_generation();
            true
        } else {
            false
        }
    }

    /// 清除选区
    pub fn clear_selection(&mut self) {
        if self.term.selection.take().is_some() {
            self.bump_generation();
        }
    }

    /// 当前是否有有效非空选区
    pub fn has_selection(&self) -> bool {
        self.term
            .selection
            .as_ref()
            .and_then(|s| s.to_range(&self.term))
            .is_some()
    }

    /// 提取当前选区纯文本（原生支持折行拼接、语义选词与矩形块选）
    pub fn selection_to_string(&self) -> Option<String> {
        self.term.selection_to_string().filter(|s| !s.is_empty())
    }

    /// 在整份终端网格与全部历史回滚中执行搜索（原生 `alacritty_terminal::RegexSearch`）
    pub fn search_all(
        &self,
        query: &str,
        case_sensitive: bool,
        is_regex: bool,
    ) -> Vec<TermSearchMatch> {
        if query.is_empty() {
            return Vec::new();
        }
        let escaped = if is_regex {
            query.to_string()
        } else {
            escape_regex_literal(query)
        };
        let pattern = if case_sensitive {
            format!("(?-i){escaped}")
        } else {
            format!("(?i){escaped}")
        };
        let Ok(mut dfas) = RegexSearch::new(&pattern) else {
            return Vec::new();
        };

        let top_line = Line(-(self.term.grid().history_size() as i32));
        let bottom_line = Line(self.dims.rows.saturating_sub(1) as i32);
        let start = Point::new(top_line, Column(0));
        let end = Point::new(bottom_line, Column(self.dims.cols.saturating_sub(1)));

        let mut matches = Vec::new();
        let mut seen = HashSet::new();
        let iter = RegexIter::new(start, end, Direction::Right, &self.term, &mut dfas);
        for m in iter.take(1500) {
            let s = *m.start();
            let e = *m.end();
            let key = (s.line.0, s.column.0, e.line.0, e.column.0);
            if !seen.insert(key) {
                break;
            }
            matches.push(TermSearchMatch {
                start_line: s.line.0,
                start_col: s.column.0,
                end_line: e.line.0,
                end_col: e.column.0,
            });
        }
        matches
    }

    /// 查询指定视口单元格是否存在可点击链接（优先 OSC 8 显式链接，其次 URL/IP 匹配）
    pub fn hyperlink_at(&self, col: usize, viewport_row: usize, is_dark: bool) -> Option<String> {
        let grid = self.render_grid(is_dark);
        let row = grid.grid_rows.get(viewport_row)?;
        for link in &row.links {
            if col >= link.col_start && col < link.col_end {
                return Some(link.uri.clone());
            }
        }
        None
    }

    /// 生成供 GPUI `TerminalElement` 直接渲染的高性能网格快照（带 `?2026` 同步合帧与代次缓存）
    pub fn render_grid(&self, is_dark: bool) -> Arc<TerminalRenderGrid> {
        // DEC ?2026h 同步输出期间若未超时，优先复用上一帧完整提交画面，彻底杜绝撕裂
        if let Some(deadline) = self.scanner.sync_update_deadline {
            if Instant::now() < deadline {
                if let Some((cached_dark, committed)) = self.last_committed_grid.borrow().as_ref() {
                    if *cached_dark == is_dark {
                        return Arc::clone(committed);
                    }
                }
            }
        }

        if let Some((cached_dark, cached_gen, snap)) = self.cached_grid.borrow().as_ref() {
            if *cached_dark == is_dark && *cached_gen == self.generation {
                return Arc::clone(snap);
            }
        }

        let content = self.term.renderable_content();
        let display_offset = content.display_offset;
        let history_size = self.term.grid().history_size();
        let rows_count = self.dims.rows;
        let cols_count = self.dims.cols;
        let selection_range = content.selection;

        let mut rows_cells: Vec<Vec<Option<RenderCell>>> =
            (0..rows_count).map(|_| vec![None; cols_count]).collect();

        for indexed in content.display_iter {
            let shifted_row = indexed.point.line.0 + display_offset as i32;
            if shifted_row < 0 || (shifted_row as usize) >= rows_count {
                continue;
            }
            let row = shifted_row as usize;
            let col = indexed.point.column.0;
            if col >= cols_count {
                continue;
            }
            let cell = indexed.cell;
            if cell.flags.contains(Flags::WIDE_CHAR_SPACER)
                || cell.flags.contains(Flags::LEADING_WIDE_CHAR_SPACER)
            {
                continue;
            }

            let mut fg = ansi_color_to_hsla(cell.fg, is_dark);
            let mut bg = ansi_bg_color_to_hsla(cell.bg, is_dark);
            if cell.flags.contains(Flags::INVERSE) {
                let default_fg = if is_dark {
                    rgb(235, 238, 244)
                } else {
                    rgb(24, 28, 34)
                };
                let default_bg = if is_dark {
                    rgb(24, 27, 32)
                } else {
                    rgb(246, 247, 249)
                };
                let orig_fg = fg.unwrap_or(default_fg);
                let orig_bg = bg.unwrap_or(default_bg);
                fg = Some(orig_bg);
                bg = Some(orig_fg);
            } else if !is_dark && bg.is_none() {
                // 浅色磨砂底色下，对 256 色 / RGB 过亮前景色自动压暗保底对比度，避免黄/青/浅灰难以辨认
                if matches!(cell.fg, Color::Spec(_) | Color::Indexed(16..=255)) {
                    fg = fg.map(ensure_light_mode_fg_contrast);
                }
            }

            let bold = cell.flags.contains(Flags::BOLD);
            let italic = cell.flags.contains(Flags::ITALIC);
            let dim = cell.flags.contains(Flags::DIM);
            let strikeout = cell.flags.contains(Flags::STRIKEOUT);
            let hidden = cell.flags.contains(Flags::HIDDEN);
            let underline = if cell.flags.contains(Flags::UNDERCURL) {
                UnderlineKind::Curly
            } else if cell.flags.contains(Flags::DOUBLE_UNDERLINE) {
                UnderlineKind::Double
            } else if cell.flags.contains(Flags::DOTTED_UNDERLINE) {
                UnderlineKind::Dotted
            } else if cell.flags.contains(Flags::DASHED_UNDERLINE) {
                UnderlineKind::Dashed
            } else if cell.flags.contains(Flags::UNDERLINE) {
                UnderlineKind::Single
            } else {
                UnderlineKind::None
            };
            let underline_color = cell
                .underline_color()
                .and_then(|c| ansi_color_to_hsla(c, is_dark));
            let hyperlink = cell
                .hyperlink()
                .map(|h| Arc::<str>::from(h.uri()));
            let width = if cell.flags.contains(Flags::WIDE_CHAR) {
                2
            } else {
                1
            };
            let selected = selection_range
                .map(|sel| {
                    sel.contains(indexed.point)
                        || (width == 2
                            && sel.contains(Point::new(
                                indexed.point.line,
                                Column(col.saturating_add(1)),
                            )))
                })
                .unwrap_or(false);

            let ch = if cell.c == '\0' { ' ' } else { cell.c };
            rows_cells[row][col] = Some(RenderCell {
                col,
                ch,
                width,
                fg,
                bg,
                underline_color,
                bold,
                italic,
                dim,
                underline,
                strikeout,
                hidden,
                selected,
                hyperlink,
            });
        }

        let cursor_shifted = content.cursor.point.line.0 + display_offset as i32;
        let cursor_in_view = cursor_shifted >= 0 && (cursor_shifted as usize) < rows_count;
        let cursor_row = cursor_shifted.max(0) as usize;
        let cursor_col = content.cursor.point.column.0;
        let cursor_shape = content.cursor.shape;
        let cursor_visible = cursor_in_view && cursor_shape != CursorShape::Hidden;

        let mut grid_rows = Vec::with_capacity(rows_count);
        for (r_idx, row_slots) in rows_cells.into_iter().enumerate() {
            let grid_line = r_idx as i32 - display_offset as i32;
            let mut cells = Vec::with_capacity(cols_count);
            for slot in row_slots {
                if let Some(c) = slot {
                    cells.push(c);
                }
            }
            let links = detect_row_links(&cells);
            let row_hash = compute_row_hash(&cells);
            grid_rows.push(RenderRow {
                grid_line,
                row_hash,
                cells,
                links,
            });
        }

        let snap = Arc::new(TerminalRenderGrid {
            cols: cols_count,
            rows: rows_count,
            history_size,
            display_offset,
            grid_rows,
            cursor_row,
            cursor_col,
            cursor_visible,
            cursor_shape,
        });
        *self.cached_grid.borrow_mut() = Some((is_dark, self.generation, Arc::clone(&snap)));
        *self.last_committed_grid.borrow_mut() = Some((is_dark, Arc::clone(&snap)));
        snap
    }
}

/// FNV-1a 风格快速行哈希（用于 `TerminalElement` 跳过未变更行的 CoreText Shaping）
fn compute_row_hash(cells: &[RenderCell]) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    cells.len().hash(&mut h);
    for c in cells {
        c.col.hash(&mut h);
        c.ch.hash(&mut h);
        c.width.hash(&mut h);
        c.bold.hash(&mut h);
        c.italic.hash(&mut h);
        c.dim.hash(&mut h);
        c.underline.hash(&mut h);
        c.strikeout.hash(&mut h);
        c.hidden.hash(&mut h);
        c.selected.hash(&mut h);
        hash_opt_hsla(c.fg, &mut h);
        hash_opt_hsla(c.bg, &mut h);
    }
    h.finish()
}

fn hash_opt_hsla(color: Option<Hsla>, h: &mut impl Hasher) {
    match color {
        Some(c) => {
            1u8.hash(h);
            c.h.to_bits().hash(h);
            c.s.to_bits().hash(h);
            c.l.to_bits().hash(h);
            c.a.to_bits().hash(h);
        }
        None => 0u8.hash(h),
    }
}

/// 识别单行中的 OSC 8 显式超链接与 `http(s)://` / `ssh://` / `ftp://` / `www.` 链接
fn detect_row_links(cells: &[RenderCell]) -> Vec<DetectedLink> {
    let mut links = Vec::new();

    // 1. 优先采集 OSC 8 显式超链接连续区间
    let mut i = 0;
    while i < cells.len() {
        if let Some(uri) = cells[i].hyperlink.as_ref() {
            let start_col = cells[i].col;
            let mut end_col = start_col + cells[i].width as usize;
            let mut j = i + 1;
            while j < cells.len() {
                if cells[j].hyperlink.as_deref() == Some(uri.as_ref()) {
                    end_col = cells[j].col + cells[j].width as usize;
                    j += 1;
                } else {
                    break;
                }
            }
            links.push(DetectedLink {
                col_start: start_col,
                col_end: end_col,
                uri: uri.to_string(),
            });
            i = j;
        } else {
            i += 1;
        }
    }

    // 2. 轻量扫描常见 URL（零外部 regex 依赖开销，O(cols) 单趟完成）
    let mut plain = String::with_capacity(cells.len());
    let mut char_cols: Vec<(usize, usize)> = Vec::with_capacity(cells.len());
    for c in cells {
        plain.push(c.ch);
        char_cols.push((c.col, c.col + c.width as usize));
    }
    let chars: Vec<char> = plain.chars().collect();
    let n = chars.len();
    let mut idx = 0;
    while idx < n {
        let rest = &plain[plain.char_indices().nth(idx).map(|(b, _)| b).unwrap_or(plain.len())..];
        let matched_prefix = if rest.starts_with("https://")
            || rest.starts_with("http://")
            || rest.starts_with("ftp://")
            || rest.starts_with("ssh://")
        {
            Some(false)
        } else if rest.starts_with("www.")
            && (idx == 0 || !chars[idx - 1].is_ascii_alphanumeric())
        {
            Some(true)
        } else {
            None
        };

        if let Some(is_www) = matched_prefix {
            let mut end = idx;
            while end < n && is_url_char(chars[end]) {
                end += 1;
            }
            // 剥离句末标点（如句号、逗号、右括号、分号、引号）
            while end > idx
                && matches!(
                    chars[end - 1],
                    '.' | ',' | ';' | ':' | ')' | ']' | '}' | '\'' | '"' | '>' | '!' | '?'
                )
            {
                end -= 1;
            }
            if end > idx + 6 {
                let col_start = char_cols[idx].0;
                let col_end = char_cols[end - 1].1;
                let overlaps = links
                    .iter()
                    .any(|l| col_start < l.col_end && col_end > l.col_start);
                if !overlaps {
                    let raw: String = chars[idx..end].iter().collect();
                    let uri = if is_www {
                        format!("https://{raw}")
                    } else {
                        raw
                    };
                    links.push(DetectedLink {
                        col_start,
                        col_end,
                        uri,
                    });
                }
                idx = end;
                continue;
            }
        }
        idx += 1;
    }

    links
}

fn is_url_char(ch: char) -> bool {
    ch.is_ascii_graphic()
        && !matches!(ch, '<' | '>' | '"' | '\'' | '`' | '{' | '}' | '|' | '\\' | '^')
}

/// 解析 OSC 7 `file://hostname/encoded/path` 为本地/远端路径字符串
pub fn parse_osc7_file_uri(uri: &str) -> Option<String> {
    let without_scheme = uri.strip_prefix("file://")?;
    let slash_idx = without_scheme.find('/')?;
    let encoded_path = &without_scheme[slash_idx..];
    let decoded = percent_decode_utf8(encoded_path);
    if decoded.is_empty() {
        None
    } else {
        Some(decoded)
    }
}

fn percent_decode_utf8(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let (Some(hi), Some(lo)) = (hex_val(bytes[i + 1]), hex_val(bytes[i + 2])) {
                out.push((hi << 4) | lo);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn hex_val(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

fn escape_regex_literal(input: &str) -> String {
    let mut out = String::with_capacity(input.len() + 8);
    for ch in input.chars() {
        if matches!(
            ch,
            '\\' | '.' | '+' | '*' | '?' | '(' | ')' | '|' | '[' | ']' | '{' | '}' | '^' | '$'
        ) {
            out.push('\\');
        }
        out.push(ch);
    }
    out
}

fn ansi_bg_color_to_hsla(color: Color, is_dark: bool) -> Option<Hsla> {
    match color {
        Color::Named(NamedColor::Background) => None,
        _ => ansi_color_to_hsla(color, is_dark),
    }
}

fn rgb(r: u8, g: u8, b: u8) -> Hsla {
    Rgba {
        r: r as f32 / 255.0,
        g: g as f32 / 255.0,
        b: b as f32 / 255.0,
        a: 1.0,
    }
    .into()
}

fn srgb_to_linear(c: f32) -> f32 {
    if c <= 0.04045 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

fn linear_to_srgb(c: f32) -> f32 {
    let clamped = c.clamp(0.0, 1.0);
    if clamped <= 0.0031308 {
        clamped * 12.92
    } else {
        1.055 * clamped.powf(1.0 / 2.4) - 0.055
    }
}

/// 浅色磨砂背景下，确保 256 色与 Truecolor 前景色的 WCAG 相对亮度不超过阈值（保持色相不变）
fn ensure_light_mode_fg_contrast(color: Hsla) -> Hsla {
    const MAX_LINEAR_LUMINANCE: f32 = 0.20;
    let rgba: Rgba = color.into();
    let lr = srgb_to_linear(rgba.r);
    let lg = srgb_to_linear(rgba.g);
    let lb = srgb_to_linear(rgba.b);
    let lum = 0.2126 * lr + 0.7152 * lg + 0.0722 * lb;
    if lum <= MAX_LINEAR_LUMINANCE || lum <= 0.0 {
        return color;
    }
    let scale = MAX_LINEAR_LUMINANCE / lum;
    Rgba {
        r: linear_to_srgb(lr * scale),
        g: linear_to_srgb(lg * scale),
        b: linear_to_srgb(lb * scale),
        a: rgba.a,
    }
    .into()
}

fn ansi_color_to_hsla(color: Color, is_dark: bool) -> Option<Hsla> {
    match color {
        Color::Named(named) => match named {
            NamedColor::Foreground | NamedColor::BrightForeground | NamedColor::DimForeground => {
                None
            }
            NamedColor::Black => Some(if is_dark {
                rgb(90, 96, 108)
            } else {
                rgb(28, 32, 42)
            }),
            NamedColor::Red | NamedColor::BrightRed => Some(if is_dark {
                rgb(255, 107, 107)
            } else {
                rgb(204, 42, 42)
            }),
            NamedColor::Green | NamedColor::BrightGreen => Some(if is_dark {
                rgb(112, 230, 158)
            } else {
                rgb(22, 143, 72)
            }),
            NamedColor::Yellow | NamedColor::BrightYellow => Some(if is_dark {
                rgb(245, 198, 86)
            } else {
                rgb(180, 118, 12)
            }),
            NamedColor::Blue | NamedColor::BrightBlue => Some(if is_dark {
                rgb(110, 178, 255)
            } else {
                rgb(28, 102, 214)
            }),
            NamedColor::Magenta | NamedColor::BrightMagenta => Some(if is_dark {
                rgb(196, 148, 255)
            } else {
                rgb(142, 68, 212)
            }),
            NamedColor::Cyan | NamedColor::BrightCyan => Some(if is_dark {
                rgb(104, 216, 236)
            } else {
                rgb(16, 134, 162)
            }),
            NamedColor::White | NamedColor::BrightWhite => Some(if is_dark {
                rgb(238, 242, 248)
            } else {
                rgb(64, 72, 86)
            }),
            NamedColor::BrightBlack => Some(if is_dark {
                rgb(148, 156, 170)
            } else {
                rgb(105, 114, 130)
            }),
            _ => None,
        },
        Color::Spec(c) => Some(rgb(c.r, c.g, c.b)),
        Color::Indexed(idx) => {
            if idx < 16 {
                let named = match idx {
                    0 => NamedColor::Black,
                    1 => NamedColor::Red,
                    2 => NamedColor::Green,
                    3 => NamedColor::Yellow,
                    4 => NamedColor::Blue,
                    5 => NamedColor::Magenta,
                    6 => NamedColor::Cyan,
                    7 => NamedColor::White,
                    8 => NamedColor::BrightBlack,
                    9 => NamedColor::BrightRed,
                    10 => NamedColor::BrightGreen,
                    11 => NamedColor::BrightYellow,
                    12 => NamedColor::BrightBlue,
                    13 => NamedColor::BrightMagenta,
                    14 => NamedColor::BrightCyan,
                    _ => NamedColor::BrightWhite,
                };
                ansi_color_to_hsla(Color::Named(named), is_dark)
            } else if idx >= 232 {
                let gray = 8 + (idx - 232) * 10;
                Some(rgb(gray, gray, gray))
            } else {
                let n = idx - 16;
                let r = (n / 36) * 51;
                let g = ((n % 36) / 6) * 51;
                let b = (n % 6) * 51;
                Some(rgb(r, g, b))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_ansi_sgr_selection_and_cursor() {
        let mut emu = TerminalEmulator::new(80, 24);
        emu.feed_bytes(b"\x1b[32;3;4mroot@host\x1b[0m:~# ls\r\ndir_a  dir_b\r\n");
        let grid = emu.render_grid(true);
        assert_eq!(grid.cursor_row, 2);
        assert_eq!(grid.cursor_col, 0);
        let first_cell = &grid.grid_rows[0].cells[0];
        assert_eq!(first_cell.ch, 'r');
        assert!(first_cell.italic);
        assert_eq!(first_cell.underline, UnderlineKind::Single);

        // 原生选区测试（字符选区 + 语义双击选词）
        emu.start_selection(0, 0, SelectionType::Simple, Side::Left);
        emu.update_selection(8, 0, Side::Right);
        assert_eq!(emu.selection_to_string().as_deref(), Some("root@host"));

        emu.start_selection(1, 1, SelectionType::Semantic, Side::Left);
        assert_eq!(emu.selection_to_string().as_deref(), Some("dir_a"));
    }

    #[test]
    fn captures_pty_write_osc52_osc7_sync_and_links() {
        let mut emu = TerminalEmulator::new(80, 24);
        // DSR 光标位置查询必须触发 Event::PtyWrite 回包
        emu.feed_bytes(b"\x1b[6n");
        let writes = emu.drain_pty_writes();
        assert_eq!(writes, vec!["\x1b[1;1R"]);

        // OSC 52 剪贴板写入
        emu.feed_bytes(b"\x1b]52;c;SGVsbG8=\x07");
        assert_eq!(emu.drain_clipboard_stores(), vec!["Hello"]);

        // OSC 7 工作目录解析（含中文 URL 编码）
        emu.feed_bytes(b"\x1b]7;file://server.local/var/log/%E6%B5%8B%E8%AF%95%20dir\x07");
        assert_eq!(
            emu.take_cwd_update().as_deref(),
            Some("/var/log/测试 dir")
        );

        // 终端内正则与普通搜索
        emu.feed_bytes(b"Visit https://holossh.dev/docs now\r\n");
        let matches = emu.search_all("holossh", false, false);
        assert_eq!(matches.len(), 1);
        assert_eq!(
            emu.hyperlink_at(10, 0, true).as_deref(),
            Some("https://holossh.dev/docs")
        );

        // 断线重置私有模式（关闭鼠标追踪与括弧粘贴）
        emu.feed_bytes(b"\x1b[?1000h\x1b[?1006h\x1b[?2004h");
        assert!(emu.mode().contains(TermMode::SGR_MOUSE));
        assert!(emu.mode().contains(TermMode::BRACKETED_PASTE));
        emu.reset_terminal_modes();
        assert!(!emu.mode().contains(TermMode::SGR_MOUSE));
        assert!(!emu.mode().contains(TermMode::BRACKETED_PASTE));
    }

    #[test]
    fn supports_inverse_video_and_scrollback() {
        let mut emu = TerminalEmulator::new(40, 4);
        for i in 1..=8 {
            emu.feed_bytes(format!("line-{i}\r\n").as_bytes());
        }
        emu.scroll_display(4);
        assert!(emu.display_offset() > 0);
        emu.scroll_to_bottom();
        assert_eq!(emu.display_offset(), 0);

        let mut inv = TerminalEmulator::new(40, 4);
        inv.feed_bytes(b"\x1b[7mSTATUS\x1b[0m");
        let g = inv.render_grid(true);
        assert!(g.grid_rows[0].cells.iter().any(|c| c.ch == 'S' && c.bg.is_some()));
    }

    #[test]
    fn enforces_light_mode_minimum_contrast_and_reports_history() {
        let mut emu = TerminalEmulator::new(40, 6);
        for i in 1..=16 {
            emu.feed_bytes(format!("line-{i}\r\n").as_bytes());
        }
        // Truecolor 纯黄 (#ffff00) 在浅色模式下必须被压暗，深色模式下保持原样
        emu.feed_bytes(b"\x1b[38;2;255;255;0mYELLOW\x1b[0m");
        let dark_grid = emu.render_grid(true);
        let light_grid = emu.render_grid(false);
        assert!(light_grid.history_size >= 6);
        assert_eq!(light_grid.display_offset, 0);

        let dark_y = dark_grid
            .grid_rows
            .iter()
            .flat_map(|r| r.cells.iter())
            .find(|c| c.ch == 'Y')
            .and_then(|c| c.fg)
            .unwrap();
        let light_y = light_grid
            .grid_rows
            .iter()
            .flat_map(|r| r.cells.iter())
            .find(|c| c.ch == 'Y')
            .and_then(|c| c.fg)
            .unwrap();
        let dark_rgba: Rgba = dark_y.into();
        let light_rgba: Rgba = light_y.into();
        assert!(dark_rgba.r > 0.9 && dark_rgba.g > 0.9);
        let light_lum = 0.2126 * srgb_to_linear(light_rgba.r)
            + 0.7152 * srgb_to_linear(light_rgba.g)
            + 0.0722 * srgb_to_linear(light_rgba.b);
        assert!(light_lum <= 0.22, "expected darkened luminance, got {light_lum}");

        // 连接成功后清屏必须彻底清空屏幕与历史回滚并将光标复位到 (0, 0)
        emu.clear_screen_and_history();
        let cleared = emu.render_grid(true);
        assert_eq!(cleared.history_size, 0);
        assert_eq!(cleared.cursor_row, 0);
        assert_eq!(cleared.cursor_col, 0);
        assert!(cleared.grid_rows.iter().all(|r| r.cells.iter().all(|c| c.ch == ' ')));
    }
}
