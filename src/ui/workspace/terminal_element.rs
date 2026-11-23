//! 原生 GPUI 自定义 `TerminalElement` 渲染引擎（参考 Zed 终端渲染管线）
//!
//! 彻底取代逐行 `div()` + `StyledText` 的 DOM 式布局：
//! - 通过 `window.text_system().layout_line("m", ...)` 获取精确 GPU 字形网格尺寸（`cell_width` / `line_height`）
//! - 按行内容哈希（`row_hash`）缓存 `ShapedLine`，未变更行零 CoreText 整形开销
//! - 合并相邻同背景单元格为单个 `PaintQuad`，支持原生 `UnderlineStyle`（含 Neovim 波浪下划线）与 `StrikethroughStyle`
//! - 几何像素级自绘常用盒绘字符（`─│┌┐└┘├┤┬┴┼█` 等），消除 `tmux` / `btop` / `lazygit` 边框行缝
//! - 绘制 `⌘F` 搜索命中高亮、`⌘+悬停` 超链接下划线、光标与 macOS 原生 IME 预编辑（Preedit）
//! - 在 `paint` 阶段调用 `window.handle_input(...)` 挂接 `ElementInputHandler` 打通原生中日韩输入法

use crate::app::HoloSshApp;
use crate::services::terminal_emu::{
    RenderCell, RenderRow, TermSearchMatch, TerminalRenderGrid, UnderlineKind,
};
use crate::state::settings::CursorStyle;
use crate::theme::ThemeTokens;
use alacritty_terminal::vte::ansi::CursorShape;
use gpui::{
    App, BorderStyle, Bounds, Element, ElementId, ElementInputHandler, Entity, FocusHandle, Font,
    FontFeatures, FontStyle, FontWeight, GlobalElementId, Hsla, InspectorElementId, IntoElement,
    LayoutId, PaintQuad, Pixels, Point, Rgba, ShapedLine, SharedString, StrikethroughStyle, Style,
    TextRun, UnderlineStyle, Window, fill, font, outline, point, px, size,
};
use std::cell::RefCell;
use std::collections::HashMap;
use std::panic::Location;
use std::rc::Rc;
use std::sync::{Arc, OnceLock};

/// 每个终端窗格最近一次 `prepaint` 产出的精确 GPU 几何度量（供鼠标命中、自适应 PTY 尺寸与 IME 候选框定位）
#[derive(Debug, Clone, Copy)]
pub struct PaneGridMetrics {
    pub bounds: Bounds<Pixels>,
    pub cell_width: f32,
    pub line_height: f32,
    pub cols: usize,
    pub rows: usize,
    pub cursor_row: usize,
    pub cursor_col: usize,
}

/// 单行按精确网格列分段后的文本渲染缓存（`(相对左偏移 px, ShapedLine)`）
pub type ShapedRowSegments = Vec<(f32, ShapedLine)>;

/// 单窗格按行缓存的 `ShapedRowSegments`（键为字体参数 + 行哈希）
#[derive(Default)]
pub struct CachedPaneShapes {
    pub font_family: String,
    pub font_size_bits: u32,
    pub is_dark: bool,
    pub cell_width_bits: u32,
    pub rows: Vec<(u64, Arc<ShapedRowSegments>)>,
}

pub type SharedPaneMetrics = Rc<RefCell<HashMap<String, PaneGridMetrics>>>;
pub type SharedPaneShapes = Rc<RefCell<HashMap<String, CachedPaneShapes>>>;

/// 传递给 `TerminalElement` 的搜索高亮上下文
#[derive(Clone, Default)]
pub struct TerminalSearchHighlight {
    pub matches: Arc<Vec<TermSearchMatch>>,
    pub active_index: Option<usize>,
}

/// 传递给 `TerminalElement` 的超链接悬停区间（`⌘+悬停`）
#[derive(Clone)]
pub struct HoveredLinkSpan {
    pub viewport_row: usize,
    pub col_start: usize,
    pub col_end: usize,
}

/// 原生 GPUI 终端画布元素
pub struct TerminalElement {
    pane_id: String,
    grid: Arc<TerminalRenderGrid>,
    font_family: SharedString,
    font_size: f32,
    is_dark: bool,
    is_focused: bool,
    cursor_style: CursorStyle,
    theme: ThemeTokens,
    search: TerminalSearchHighlight,
    hovered_link: Option<HoveredLinkSpan>,
    ime_marked_text: Option<String>,
    register_ime_input: bool,
    focus_handle: FocusHandle,
    app_entity: Entity<HoloSshApp>,
    metrics_store: SharedPaneMetrics,
    shape_cache: SharedPaneShapes,
}

impl TerminalElement {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        pane_id: String,
        grid: Arc<TerminalRenderGrid>,
        font_family: SharedString,
        font_size: f32,
        is_dark: bool,
        is_focused: bool,
        cursor_style: CursorStyle,
        theme: ThemeTokens,
        search: TerminalSearchHighlight,
        hovered_link: Option<HoveredLinkSpan>,
        ime_marked_text: Option<String>,
        register_ime_input: bool,
        focus_handle: FocusHandle,
        app_entity: Entity<HoloSshApp>,
        metrics_store: SharedPaneMetrics,
        shape_cache: SharedPaneShapes,
    ) -> Self {
        Self {
            pane_id,
            grid,
            font_family,
            font_size,
            is_dark,
            is_focused,
            cursor_style,
            theme,
            search,
            hovered_link,
            ime_marked_text,
            register_ime_input,
            focus_handle,
            app_entity,
            metrics_store,
            shape_cache,
        }
    }
}

pub struct TerminalPrepaintState {
    cell_width: f32,
    line_height: f32,
    bg_quads: Vec<PaintQuad>,
    shaped_rows: Vec<(usize, Arc<ShapedRowSegments>)>,
    fg_quads: Vec<PaintQuad>,
    ime_preedit: Option<(Point<Pixels>, PaintQuad, ShapedLine, PaintQuad)>,
}

impl IntoElement for TerminalElement {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for TerminalElement {
    type RequestLayoutState = ();
    type PrepaintState = TerminalPrepaintState;

    fn id(&self) -> Option<ElementId> {
        Some(ElementId::Name(SharedString::from(format!(
            "term-canvas-{}",
            self.pane_id
        ))))
    }

    fn source_location(&self) -> Option<&'static Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        let mut style = Style::default();
        style.size.width = gpui::relative(1.0).into();
        style.size.height = gpui::relative(1.0).into();
        (window.request_layout(style, [], cx), ())
    }

    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        window: &mut Window,
        _cx: &mut App,
    ) -> Self::PrepaintState {
        let font_size_px = px(self.font_size);
        let base_font = build_font(&self.font_family, false, false);

        // 1. 精确测量等宽单格像素宽高（CoreText / GPU 字形度量，消除估算漂移）
        let probe_run = TextRun {
            len: 1,
            font: base_font.clone(),
            color: self.theme.text_terminal,
            background_color: None,
            underline: None,
            strikethrough: None,
        };
        let probe = window
            .text_system()
            .layout_line("m", font_size_px, &[probe_run], None);
        let measured_w: f32 = probe.width.into();
        let cell_width = if measured_w > 3.0 && measured_w < 64.0 {
            measured_w
        } else {
            self.font_size * 0.6
        };
        let line_height = (self.font_size * 1.44).round().max(15.0);

        let bounds_w: f32 = bounds.size.width.into();
        let bounds_h: f32 = bounds.size.height.into();
        let cols = ((bounds_w / cell_width).floor() as usize).clamp(20, 420);
        let rows = ((bounds_h / line_height).floor() as usize).clamp(6, 200);

        // 发布精确网格几何，供鼠标坐标换算、PTY 自适应缩放与 IME 候选框定位使用
        self.metrics_store.borrow_mut().insert(
            self.pane_id.clone(),
            PaneGridMetrics {
                bounds,
                cell_width,
                line_height,
                cols,
                rows,
                cursor_row: self.grid.cursor_row,
                cursor_col: self.grid.cursor_col,
            },
        );

        let origin_x: f32 = bounds.origin.x.into();
        let origin_y: f32 = bounds.origin.y.into();

        let mut bg_quads: Vec<PaintQuad> = Vec::with_capacity(self.grid.rows * 2);
        let mut fg_quads: Vec<PaintQuad> = Vec::with_capacity(64);
        let mut shaped_rows: Vec<(usize, Arc<ShapedRowSegments>)> =
            Vec::with_capacity(self.grid.rows);

        let selection_bg = if self.is_dark {
            Hsla::from(Rgba {
                r: 1.0,
                g: 1.0,
                b: 1.0,
                a: 0.22,
            })
        } else {
            Hsla::from(Rgba {
                r: 0.08,
                g: 0.12,
                b: 0.18,
                a: 0.16,
            })
        };
        let search_bg = if self.is_dark {
            Hsla::from(Rgba {
                r: 0.96,
                g: 0.78,
                b: 0.34,
                a: 0.26,
            })
        } else {
            Hsla::from(Rgba {
                r: 0.85,
                g: 0.56,
                b: 0.06,
                a: 0.22,
            })
        };
        let search_active_bg = if self.is_dark {
            Hsla::from(Rgba {
                r: 0.98,
                g: 0.82,
                b: 0.38,
                a: 0.48,
            })
        } else {
            Hsla::from(Rgba {
                r: 0.88,
                g: 0.58,
                b: 0.08,
                a: 0.38,
            })
        };

        // 准备行级 Shaping 缓存（按字体族/字号/明暗/cell_width 校验有效性）
        let mut cache_map = self.shape_cache.borrow_mut();
        let pane_cache = cache_map.entry(self.pane_id.clone()).or_default();
        let font_size_bits = self.font_size.to_bits();
        let cell_width_bits = cell_width.to_bits();
        if pane_cache.font_family != self.font_family.as_ref()
            || pane_cache.font_size_bits != font_size_bits
            || pane_cache.is_dark != self.is_dark
            || pane_cache.cell_width_bits != cell_width_bits
        {
            pane_cache.font_family = self.font_family.to_string();
            pane_cache.font_size_bits = font_size_bits;
            pane_cache.is_dark = self.is_dark;
            pane_cache.cell_width_bits = cell_width_bits;
            pane_cache.rows.clear();
        }
        if pane_cache.rows.len() != self.grid.grid_rows.len() {
            pane_cache
                .rows
                .resize_with(self.grid.grid_rows.len(), || (0, Arc::new(Vec::new())));
        }

        let max_visible_rows = ((bounds_h / line_height).ceil() as usize).max(1);

        for (row_idx, row) in self
            .grid
            .grid_rows
            .iter()
            .take(max_visible_rows)
            .enumerate()
        {
            let row_y = origin_y + (row_idx as f32) * line_height;

            // 2. 合并绘制本行背景色与原生选区高亮（Run Coalescing）
            collect_row_backgrounds(
                row,
                origin_x,
                row_y,
                cell_width,
                line_height,
                selection_bg,
                &mut bg_quads,
            );

            // 3. 绘制 `⌘F` 搜索命中高亮
            if !self.search.matches.is_empty() {
                collect_row_search_highlights(
                    row.grid_line,
                    self.grid.cols,
                    &self.search,
                    origin_x,
                    row_y,
                    cell_width,
                    line_height,
                    search_bg,
                    search_active_bg,
                    &mut bg_quads,
                );
            }

            // 4. 绘制盒绘几何字符（像素级无缝连接）
            for cell in &row.cells {
                if !cell.hidden && is_geometric_box_char(cell.ch) {
                    let color = effective_fg(cell, &self.theme);
                    emit_box_drawing_quads(
                        cell.ch,
                        origin_x + (cell.col as f32) * cell_width,
                        row_y,
                        cell_width,
                        line_height,
                        color,
                        &mut fg_quads,
                    );
                }
            }

            // 5. 增量按行 Shaping（row_hash 命中直接零拷贝复用上一帧 ShapedLine）
            if row.cells.is_empty() {
                continue;
            }
            let cached_entry = &mut pane_cache.rows[row_idx];
            let shaped = if cached_entry.0 == row.row_hash && cached_entry.0 != 0 {
                Arc::clone(&cached_entry.1)
            } else {
                let s = Arc::new(shape_terminal_row(
                    row,
                    &self.font_family,
                    font_size_px,
                    cell_width,
                    &self.theme,
                    window,
                ));
                *cached_entry = (row.row_hash, Arc::clone(&s));
                s
            };
            if !shaped.is_empty() {
                shaped_rows.push((row_idx, shaped));
            }

            // 6. 绘制 `⌘+悬停` 超链接下划线
            if let Some(hover) = self.hovered_link.as_ref() {
                if hover.viewport_row == row_idx && hover.col_end > hover.col_start {
                    let x0 = origin_x + (hover.col_start as f32) * cell_width;
                    let w = ((hover.col_end - hover.col_start) as f32) * cell_width;
                    fg_quads.push(fill(
                        Bounds::new(
                            point(px(x0), px(row_y + line_height - 2.0)),
                            size(px(w), px(1.0)),
                        ),
                        self.theme.text_primary,
                    ));
                }
            }
        }
        drop(cache_map);

        // 7. 绘制光标（像素级对齐；遵循远端 DECSCUSR 与用户设置，非焦点绘制空心线框）
        if self.grid.cursor_visible && self.grid.cursor_row < max_visible_rows {
            let cx_px = origin_x + (self.grid.cursor_col as f32) * cell_width;
            let cy_px = origin_y + (self.grid.cursor_row as f32) * line_height;
            let cursor_rect = Bounds::new(
                point(px(cx_px), px(cy_px + 1.0)),
                size(px(cell_width.max(7.0)), px((line_height - 2.0).max(12.0))),
            );

            if !self.is_focused {
                let mut q = outline(cursor_rect, self.theme.text_tertiary, BorderStyle::Solid);
                q.corner_radii = px(1.0).into();
                fg_quads.push(q);
            } else {
                let effective_shape = resolve_cursor_shape(self.grid.cursor_shape, self.cursor_style);
                match effective_shape {
                    CursorShape::HollowBlock => {
                        let mut q = outline(cursor_rect, self.theme.text_primary, BorderStyle::Solid);
                        q.corner_radii = px(1.0).into();
                        fg_quads.push(q);
                    }
                    CursorShape::Beam => {
                        let mut q = fill(
                            Bounds::new(
                                point(px(cx_px), px(cy_px + 1.0)),
                                size(px(2.0), px((line_height - 2.0).max(12.0))),
                            ),
                            self.theme.text_primary,
                        );
                        q.corner_radii = px(0.5).into();
                        fg_quads.push(q);
                    }
                    CursorShape::Underline => {
                        let q = fill(
                            Bounds::new(
                                point(px(cx_px), px(cy_px + line_height - 3.0)),
                                size(px(cell_width.max(7.0)), px(2.0)),
                            ),
                            self.theme.text_primary,
                        );
                        fg_quads.push(q);
                    }
                    CursorShape::Block | CursorShape::Hidden => {
                        let mut q = fill(cursor_rect, self.theme.text_primary);
                        q.corner_radii = px(1.0).into();
                        if let Some(cell) = self
                            .grid
                            .grid_rows
                            .get(self.grid.cursor_row)
                            .and_then(|r| r.cells.iter().find(|c| c.col == self.grid.cursor_col))
                            .filter(|c| !c.hidden && c.ch != ' ')
                        {
                            let w_mult = cell.width as f32;
                            q.bounds = Bounds::new(
                                point(px(cx_px), px(cy_px + 1.0)),
                                size(
                                    px((cell_width * w_mult).max(7.0)),
                                    px((line_height - 2.0).max(12.0)),
                                ),
                            );
                        }
                        fg_quads.push(q);
                    }
                }
            }
        }

        // 7.5 绘制右侧 4px 极细磨砂单色滚动条滑块（有历史回滚时显示，翻阅历史时提高不透明度）
        if self.grid.history_size > 0 && bounds_h > 36.0 {
            let total_lines = (self.grid.history_size + self.grid.rows).max(1) as f32;
            let visible_lines = self.grid.rows.max(1) as f32;
            let track_pad = 3.0;
            let track_h = (bounds_h - track_pad * 2.0).max(24.0);
            let thumb_h = ((visible_lines / total_lines) * track_h).clamp(24.0, track_h);
            let scroll_ratio = 1.0
                - (self.grid.display_offset as f32 / self.grid.history_size as f32).clamp(0.0, 1.0);
            let thumb_y = origin_y + track_pad + scroll_ratio * (track_h - thumb_h);
            let thumb_w = 4.0;
            let thumb_x = (origin_x + bounds_w - thumb_w).max(origin_x);
            let alpha = if self.grid.display_offset > 0 {
                if self.is_dark { 0.36 } else { 0.32 }
            } else if self.is_dark {
                0.15
            } else {
                0.14
            };
            let thumb_color = if self.is_dark {
                Hsla::from(Rgba {
                    r: 1.0,
                    g: 1.0,
                    b: 1.0,
                    a: alpha,
                })
            } else {
                Hsla::from(Rgba {
                    r: 0.08,
                    g: 0.10,
                    b: 0.14,
                    a: alpha,
                })
            };
            let mut sb_quad = fill(
                Bounds::new(point(px(thumb_x), px(thumb_y)), size(px(thumb_w), px(thumb_h))),
                thumb_color,
            );
            sb_quad.corner_radii = px(2.0).into();
            fg_quads.push(sb_quad);
        }

        // 8. 原生 macOS IME 预编辑（Marked Text）光标处内联渲染
        let ime_preedit = if self.is_focused {
            if let Some(marked) = self.ime_marked_text.as_deref().filter(|s| !s.is_empty()) {
                let cx_px = origin_x + (self.grid.cursor_col as f32) * cell_width;
                let cy_px = origin_y + (self.grid.cursor_row as f32) * line_height;
                let text: SharedString = marked.to_string().into();
                let run = TextRun {
                    len: text.len(),
                    font: base_font,
                    color: self.theme.text_primary,
                    background_color: None,
                    underline: None,
                    strikethrough: None,
                };
                let shaped = window
                    .text_system()
                    .shape_line(text, font_size_px, &[run], None);
                let w: f32 = shaped.width.into();
                let box_w = w.max(cell_width);
                let mut bg_q = fill(
                    Bounds::new(
                        point(px(cx_px - 2.0), px(cy_px)),
                        size(px(box_w + 4.0), px(line_height)),
                    ),
                    self.theme.sheet_bg,
                );
                bg_q.corner_radii = px(3.0).into();
                let ul_q = fill(
                    Bounds::new(
                        point(px(cx_px), px(cy_px + line_height - 2.0)),
                        size(px(box_w), px(1.5)),
                    ),
                    self.theme.text_primary,
                );
                Some((point(px(cx_px), px(cy_px)), bg_q, shaped, ul_q))
            } else {
                None
            }
        } else {
            None
        };

        TerminalPrepaintState {
            cell_width,
            line_height,
            bg_quads,
            shaped_rows,
            fg_quads,
            ime_preedit,
        }
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        prepaint: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        // 挂接 GPUI 原生输入处理器，使 macOS 中日韩 IME 候选框与预编辑精准绑定当前终端画布
        if self.register_ime_input {
            window.handle_input(
                &self.focus_handle,
                ElementInputHandler::new(bounds, self.app_entity.clone()),
                cx,
            );
        }

        let origin_x: f32 = bounds.origin.x.into();
        let origin_y: f32 = bounds.origin.y.into();

        // 1. 先绘制全部单元格背景、选区高亮与搜索高亮
        for quad in prepaint.bg_quads.drain(..) {
            window.paint_quad(quad);
        }

        // 2. 绘制逐行 GPU 整形文本（纯 ASCII 整行单段，含宽字符时按精确网格列对齐落位）
        let lh_px = px(prepaint.line_height);
        for (row_idx, segments) in &prepaint.shaped_rows {
            let y = origin_y + (*row_idx as f32) * prepaint.line_height;
            for (x_off, seg) in segments.iter() {
                let _ = seg.paint(point(px(origin_x + *x_off), px(y)), lh_px, window, cx);
            }
        }

        // 3. 绘制盒绘几何图形、超链接悬停下划线与光标
        for quad in prepaint.fg_quads.drain(..) {
            window.paint_quad(quad);
        }

        // 若焦点实心方块光标覆盖在非空字符上方，反色重绘该单字以保证 100% 清晰可见
        if self.is_focused
            && self.grid.cursor_visible
            && matches!(
                resolve_cursor_shape(self.grid.cursor_shape, self.cursor_style),
                CursorShape::Block
            )
        {
            if let Some(cell) = self
                .grid
                .grid_rows
                .get(self.grid.cursor_row)
                .and_then(|r| r.cells.iter().find(|c| c.col == self.grid.cursor_col))
                .filter(|c| !c.hidden && c.ch != ' ' && !is_geometric_box_char(c.ch))
            {
                let cx_px = origin_x + (self.grid.cursor_col as f32) * prepaint.cell_width;
                let cy_px = origin_y + (self.grid.cursor_row as f32) * prepaint.line_height;
                let ch_str: SharedString = cell.ch.to_string().into();
                let inv_run = TextRun {
                    len: ch_str.len(),
                    font: build_font(&self.font_family, cell.bold, cell.italic),
                    color: self.theme.window_bg,
                    background_color: None,
                    underline: None,
                    strikethrough: None,
                };
                let is_ascii = cell.ch.is_ascii() && cell.width == 1;
                let shaped_ch = window.text_system().shape_line(
                    ch_str,
                    px(self.font_size),
                    &[inv_run],
                    if is_ascii {
                        Some(px(prepaint.cell_width))
                    } else {
                        None
                    },
                );
                let draw_x = if is_ascii {
                    cx_px
                } else {
                    let slot_w = (cell.width as f32) * prepaint.cell_width;
                    let glyph_w: f32 = shaped_ch.width.into();
                    cx_px + ((slot_w - glyph_w) * 0.5).max(0.0)
                };
                let _ = shaped_ch.paint(point(px(draw_x), px(cy_px)), lh_px, window, cx);
            }
        }

        // 4. 绘制 IME 预编辑（Marked Text）浮层
        if let Some((pt, bg_q, shaped, ul_q)) = prepaint.ime_preedit.take() {
            window.paint_quad(bg_q);
            let _ = shaped.paint(pt, lh_px, window, cx);
            window.paint_quad(ul_q);
        }
    }
}

fn resolve_cursor_shape(remote: CursorShape, user_pref: CursorStyle) -> CursorShape {
    match remote {
        CursorShape::Beam => CursorShape::Beam,
        CursorShape::Underline => CursorShape::Underline,
        CursorShape::HollowBlock => CursorShape::HollowBlock,
        CursorShape::Hidden => CursorShape::Hidden,
        CursorShape::Block => match user_pref {
            CursorStyle::Block => CursorShape::Block,
            CursorStyle::Beam => CursorShape::Beam,
            CursorStyle::Underline => CursorShape::Underline,
        },
    }
}

fn terminal_no_ligatures_features() -> FontFeatures {
    static FEATURES: OnceLock<FontFeatures> = OnceLock::new();
    FEATURES
        .get_or_init(|| {
            FontFeatures(Arc::new(vec![
                ("calt".into(), 0),
                ("liga".into(), 0),
                ("dlig".into(), 0),
            ]))
        })
        .clone()
}

fn build_font(family: &SharedString, bold: bool, italic: bool) -> Font {
    let mut f = font(family.clone());
    // 禁用编程连字（calt/liga/dlig），避免 JetBrains Mono 等字体将 `***` 变形为三角星号或使终端字符错位
    f.features = terminal_no_ligatures_features();
    if bold {
        f.weight = FontWeight::BOLD;
    }
    if italic {
        f.style = FontStyle::Italic;
    }
    f
}

fn effective_fg(cell: &RenderCell, theme: &ThemeTokens) -> Hsla {
    let mut c = cell.fg.unwrap_or(theme.text_terminal);
    if cell.dim {
        c.a *= 0.62;
    }
    if cell.hidden {
        c.a = 0.0;
    }
    c
}

/// 合并单行中相邻同背景色（含选区）单元格，极大减少 GPU Quad 数量
fn collect_row_backgrounds(
    row: &RenderRow,
    origin_x: f32,
    row_y: f32,
    cell_width: f32,
    line_height: f32,
    selection_bg: Hsla,
    out: &mut Vec<PaintQuad>,
) {
    let mut run_start: Option<(usize, usize, Hsla)> = None;

    for cell in &row.cells {
        let cell_bg = if cell.selected {
            Some(selection_bg)
        } else {
            cell.bg
        };
        let col_end = cell.col + cell.width as usize;

        match (run_start, cell_bg) {
            (Some((s_col, e_col, cur_bg)), Some(bg)) if e_col == cell.col && cur_bg == bg => {
                run_start = Some((s_col, col_end, cur_bg));
            }
            (Some((s_col, e_col, cur_bg)), next) => {
                out.push(fill(
                    Bounds::new(
                        point(px(origin_x + (s_col as f32) * cell_width), px(row_y)),
                        size(px(((e_col - s_col) as f32) * cell_width), px(line_height)),
                    ),
                    cur_bg,
                ));
                run_start = next.map(|bg| (cell.col, col_end, bg));
            }
            (None, Some(bg)) => {
                run_start = Some((cell.col, col_end, bg));
            }
            (None, None) => {}
        }
    }

    if let Some((s_col, e_col, cur_bg)) = run_start {
        out.push(fill(
            Bounds::new(
                point(px(origin_x + (s_col as f32) * cell_width), px(row_y)),
                size(px(((e_col - s_col) as f32) * cell_width), px(line_height)),
            ),
            cur_bg,
        ));
    }
}

#[allow(clippy::too_many_arguments)]
fn collect_row_search_highlights(
    grid_line: i32,
    total_cols: usize,
    search: &TerminalSearchHighlight,
    origin_x: f32,
    row_y: f32,
    cell_width: f32,
    line_height: f32,
    normal_bg: Hsla,
    active_bg: Hsla,
    out: &mut Vec<PaintQuad>,
) {
    for (idx, m) in search.matches.iter().enumerate() {
        if grid_line < m.start_line || grid_line > m.end_line {
            continue;
        }
        let col_start = if grid_line == m.start_line {
            m.start_col
        } else {
            0
        };
        let col_end = if grid_line == m.end_line {
            (m.end_col + 1).min(total_cols)
        } else {
            total_cols
        };
        if col_end <= col_start {
            continue;
        }
        let is_active = search.active_index == Some(idx);
        let mut q = fill(
            Bounds::new(
                point(px(origin_x + (col_start as f32) * cell_width), px(row_y + 0.5)),
                size(
                    px(((col_end - col_start) as f32) * cell_width),
                    px((line_height - 1.0).max(10.0)),
                ),
            ),
            if is_active { active_bg } else { normal_bg },
        );
        q.corner_radii = px(2.0).into();
        out.push(q);
    }
}

fn cell_decorations(
    cell: &RenderCell,
    fg: Hsla,
) -> (Option<UnderlineStyle>, Option<StrikethroughStyle>) {
    let ul = match cell.underline {
        UnderlineKind::None => None,
        UnderlineKind::Curly => Some(UnderlineStyle {
            thickness: px(1.2),
            color: Some(cell.underline_color.unwrap_or(fg)),
            wavy: true,
        }),
        UnderlineKind::Double => Some(UnderlineStyle {
            thickness: px(1.5),
            color: Some(cell.underline_color.unwrap_or(fg)),
            wavy: false,
        }),
        UnderlineKind::Single | UnderlineKind::Dotted | UnderlineKind::Dashed => {
            Some(UnderlineStyle {
                thickness: px(1.0),
                color: Some(cell.underline_color.unwrap_or(fg)),
                wavy: false,
            })
        }
    };
    let st = cell.strikeout.then(|| StrikethroughStyle {
        thickness: px(1.0),
        color: Some(fg),
    });
    (ul, st)
}

fn push_merged_run(
    runs: &mut Vec<TextRun>,
    font_family: &SharedString,
    len: usize,
    fg: Hsla,
    bold: bool,
    italic: bool,
    underline: Option<UnderlineStyle>,
    strikethrough: Option<StrikethroughStyle>,
) {
    if len == 0 {
        return;
    }
    let font_spec = build_font(font_family, bold, italic);
    if let Some(last) = runs.last_mut() {
        if last.color == fg
            && last.font == font_spec
            && last.underline == underline
            && last.strikethrough == strikethrough
        {
            last.len += len;
            return;
        }
    }
    runs.push(TextRun {
        len,
        font: font_spec,
        color: fg,
        background_color: None,
        underline,
        strikethrough,
    });
}

/// 将一行终端网格整形为精确网格对齐的 `ShapedRowSegments`：
/// - 纯 ASCII 行走单次 `shape_line(..., Some(cell_width))` 快速路径；
/// - 含中文/宽字符/Emoji 行将连续 ASCII 子段与每个宽字符分别锚定至 `col * cell_width`，
///   确保光标、选区、搜索高亮、表格列与中文字形 100% 像素对齐。
fn shape_terminal_row(
    row: &RenderRow,
    font_family: &SharedString,
    font_size: Pixels,
    cell_width: f32,
    theme: &ThemeTokens,
    window: &Window,
) -> ShapedRowSegments {
    let last_visible = row.cells.iter().rposition(|c| {
        (!c.hidden && c.ch != ' ' && !is_geometric_box_char(c.ch))
            || c.underline != UnderlineKind::None
            || c.strikeout
    });
    let Some(last_idx) = last_visible else {
        return Vec::new();
    };
    let visible_slice = &row.cells[..=last_idx];
    let all_ascii = visible_slice.iter().all(|c| {
        let ch = if c.hidden || is_geometric_box_char(c.ch) {
            ' '
        } else {
            c.ch
        };
        ch.is_ascii() && c.width == 1
    });

    // 1. 纯 ASCII 快速路径：整行一次 GPU Shaping，强制每字符精确占 1 × cell_width
    if all_ascii {
        let mut text = String::with_capacity(last_idx + 16);
        let mut runs: Vec<TextRun> = Vec::with_capacity(8);
        let mut cursor_col = 0usize;

        for cell in visible_slice {
            if cell.col > cursor_col {
                let gap = cell.col - cursor_col;
                for _ in 0..gap {
                    text.push(' ');
                }
                push_merged_run(
                    &mut runs,
                    font_family,
                    gap,
                    theme.text_terminal,
                    false,
                    false,
                    None,
                    None,
                );
            }
            let draw_ch = if cell.hidden || is_geometric_box_char(cell.ch) {
                ' '
            } else {
                cell.ch
            };
            let fg = effective_fg(cell, theme);
            let (ul, st) = cell_decorations(cell, fg);
            let before = text.len();
            text.push(draw_ch);
            push_merged_run(
                &mut runs,
                font_family,
                text.len() - before,
                fg,
                cell.bold,
                cell.italic,
                ul,
                st,
            );
            cursor_col = cell.col + 1;
        }
        if text.is_empty() {
            return Vec::new();
        }
        let shaped = window.text_system().shape_line(
            SharedString::from(text),
            font_size,
            &runs,
            Some(px(cell_width)),
        );
        return vec![(0.0, shaped)];
    }

    // 2. 混排路径（含中日韩宽字符 / Emoji / 特殊符号）：按精确列坐标锚定每一段
    let mut segments: ShapedRowSegments = Vec::with_capacity(16);
    let mut ascii_start_col: Option<usize> = None;
    let mut ascii_cur_col = 0usize;
    let mut ascii_text = String::new();
    let mut ascii_runs: Vec<TextRun> = Vec::new();

    let flush_ascii = |segments: &mut ShapedRowSegments,
                       start_col: &mut Option<usize>,
                       text: &mut String,
                       runs: &mut Vec<TextRun>| {
        if let Some(sc) = start_col.take() {
            if !text.is_empty() {
                let s = window.text_system().shape_line(
                    SharedString::from(std::mem::take(text)),
                    font_size,
                    runs,
                    Some(px(cell_width)),
                );
                segments.push(((sc as f32) * cell_width, s));
            }
            runs.clear();
        }
    };

    for cell in visible_slice {
        let draw_ch = if cell.hidden || is_geometric_box_char(cell.ch) {
            ' '
        } else {
            cell.ch
        };
        let fg = effective_fg(cell, theme);
        let (ul, st) = cell_decorations(cell, fg);

        if draw_ch.is_ascii() && cell.width == 1 {
            if ascii_start_col.is_none() {
                ascii_start_col = Some(cell.col);
            } else if cell.col > ascii_cur_col {
                let gap = cell.col - ascii_cur_col;
                for _ in 0..gap {
                    ascii_text.push(' ');
                }
                push_merged_run(
                    &mut ascii_runs,
                    font_family,
                    gap,
                    theme.text_terminal,
                    false,
                    false,
                    None,
                    None,
                );
            }
            let before = ascii_text.len();
            ascii_text.push(draw_ch);
            push_merged_run(
                &mut ascii_runs,
                font_family,
                ascii_text.len() - before,
                fg,
                cell.bold,
                cell.italic,
                ul,
                st,
            );
            ascii_cur_col = cell.col + 1;
        } else {
            flush_ascii(
                &mut segments,
                &mut ascii_start_col,
                &mut ascii_text,
                &mut ascii_runs,
            );
            let ch_str: SharedString = draw_ch.to_string().into();
            let run = TextRun {
                len: ch_str.len(),
                font: build_font(font_family, cell.bold, cell.italic),
                color: fg,
                background_color: None,
                underline: ul,
                strikethrough: st,
            };
            let shaped = window
                .text_system()
                .shape_line(ch_str, font_size, &[run], None);
            let slot_w = (cell.width as f32) * cell_width;
            let glyph_w: f32 = shaped.width.into();
            let x_pos = (cell.col as f32) * cell_width + ((slot_w - glyph_w) * 0.5).max(0.0);
            segments.push((x_pos, shaped));
        }
    }

    flush_ascii(
        &mut segments,
        &mut ascii_start_col,
        &mut ascii_text,
        &mut ascii_runs,
    );

    segments
}

/// 判断是否为适合像素级几何自绘的常用盒绘 / 块状字符（`tmux` / `btop` / `lazygit` 边框无缝衔接）
fn is_geometric_box_char(ch: char) -> bool {
    matches!(
        ch,
        '─' | '━'
            | '│'
            | '┃'
            | '┌'
            | '┏'
            | '┐'
            | '┓'
            | '└'
            | '┗'
            | '┘'
            | '┛'
            | '├'
            | '┣'
            | '┤'
            | '┫'
            | '┬'
            | '┳'
            | '┴'
            | '┻'
            | '┼'
            | '╋'
            | '╭'
            | '╮'
            | '╰'
            | '╯'
            | '█'
            | '▀'
            | '▄'
            | '▌'
            | '▐'
    )
}

fn emit_box_drawing_quads(
    ch: char,
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    color: Hsla,
    out: &mut Vec<PaintQuad>,
) {
    // 块状填充元素
    match ch {
        '█' => {
            out.push(fill(Bounds::new(point(px(x), px(y)), size(px(w), px(h))), color));
            return;
        }
        '▀' => {
            out.push(fill(
                Bounds::new(point(px(x), px(y)), size(px(w), px(h * 0.5))),
                color,
            ));
            return;
        }
        '▄' => {
            out.push(fill(
                Bounds::new(point(px(x), px(y + h * 0.5)), size(px(w), px(h * 0.5))),
                color,
            ));
            return;
        }
        '▌' => {
            out.push(fill(
                Bounds::new(point(px(x), px(y)), size(px(w * 0.5), px(h))),
                color,
            ));
            return;
        }
        '▐' => {
            out.push(fill(
                Bounds::new(point(px(x + w * 0.5), px(y)), size(px(w * 0.5), px(h))),
                color,
            ));
            return;
        }
        _ => {}
    }

    let heavy = matches!(
        ch,
        '━' | '┃' | '┏' | '┓' | '┗' | '┛' | '┣' | '┫' | '┳' | '┻' | '╋'
    );
    let t = if heavy { 2.0_f32 } else { 1.0_f32 };
    let cx = (x + w * 0.5 - t * 0.5).round();
    let cy = (y + h * 0.5 - t * 0.5).round();

    let left = matches!(
        ch,
        '─' | '━' | '┐' | '┓' | '╯' | '┘' | '┛' | '╮' | '┤' | '┫' | '┬' | '┳' | '┴' | '┻' | '┼' | '╋'
    );
    let right = matches!(
        ch,
        '─' | '━' | '┌' | '┏' | '╭' | '└' | '┗' | '╰' | '├' | '┣' | '┬' | '┳' | '┴' | '┻' | '┼' | '╋'
    );
    let up = matches!(
        ch,
        '│' | '┃' | '└' | '┗' | '╰' | '┘' | '┛' | '╯' | '├' | '┣' | '┤' | '┫' | '┴' | '┻' | '┼' | '╋'
    );
    let down = matches!(
        ch,
        '│' | '┃' | '┌' | '┏' | '╭' | '┐' | '┓' | '╮' | '├' | '┣' | '┤' | '┫' | '┬' | '┳' | '┼' | '╋'
    );

    if left {
        out.push(fill(
            Bounds::new(point(px(x), px(cy)), size(px((cx + t - x).max(t)), px(t))),
            color,
        ));
    }
    if right {
        out.push(fill(
            Bounds::new(point(px(cx), px(cy)), size(px((x + w - cx).max(t)), px(t))),
            color,
        ));
    }
    if up {
        out.push(fill(
            Bounds::new(point(px(cx), px(y)), size(px(t), px((cy + t - y).max(t)))),
            color,
        ));
    }
    if down {
        out.push(fill(
            Bounds::new(point(px(cx), px(cy)), size(px(t), px((y + h - cy).max(t)))),
            color,
        ));
    }
}
