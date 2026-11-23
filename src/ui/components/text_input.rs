//! 通用单行可编辑文本核心与 GPUI 像素级渲染元素
//!
//! 为全应用所有单行输入框（侧栏搜索、分组新建/重命名、终端搜索、SFTP 新建文件夹、
//! 新建/编辑主机弹窗、密钥库与云同步表单等）统一提供原生 macOS 级编辑体验：
//! - 鼠标单击精准落位光标（CoreText 实际字形坐标命中，兼容中/日/韩/Emoji/掩码圆点）
//! - 鼠标拖拽选区、双击选词、三击全选、长文本自动水平滚动跟随光标
//! - 完整键盘移动与选区（`←`/`→`、`⌥←`/`⌥→`、`⌘←`/`⌘→`、`Home`/`End`、`⇧` 扩选、`⌘A`）
//! - 完整删除与剪贴板（`Backspace`/`Delete`、`⌥Backspace`、`⌘Backspace`、`Ctrl+K/U`、`⌘C/⌘X/⌘V`）
//! - 与原生 macOS IME（`NSTextInputClient`）精确联动：预编辑内联下划线展示与候选框紧贴光标

use crate::app::ActiveInput;
use crate::theme::ThemeTokens;
use crate::ui::components::primitives::{
    active_ime_marked_text, set_active_ime_caret_bounds,
};
use gpui::{
    App, Bounds, ContentMask, Element, ElementId, Font, FontFeatures, FontStyle, FontWeight,
    GlobalElementId, Hsla, InspectorElementId, IntoElement, Keystroke, LayoutId, PaintQuad, Pixels,
    Point, Rgba, ShapedLine, SharedString, Style, TextRun, UnderlineStyle, Window, fill, point, px,
    size,
};
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::panic::Location;
use std::time::Instant;

/// 通用文本输入框的光标与选区状态（统一使用 Unicode 标量字符索引 `0..=char_len`，
/// 确保密码掩码 `•` 与明文 1:1 对应，永不发生 UTF-8 字节截断 panic）
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct TextCursorState {
    /// 选区锚点（字符索引）
    pub anchor: usize,
    /// 当前活动光标位置（字符索引）
    pub cursor: usize,
    /// 鼠标左键是否正在拖拽选区
    pub dragging: bool,
}

/// 单次键盘编辑按键的处理结果
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TextKeyOutcome {
    /// 非文本编辑键（交由外层处理，例如 Enter / Esc / Tab）
    Unhandled,
    /// 仅移动了光标或修改了选区（无需落盘，但需刷新渲染）
    CursorMoved,
    /// 文本内容已变更（需刷新渲染并触发搜索/保存副作用）
    Modified,
}

impl TextCursorState {
    #[inline]
    pub fn clamp(&mut self, text: &str) {
        let len = text.chars().count();
        self.anchor = self.anchor.min(len);
        self.cursor = self.cursor.min(len);
    }

    #[inline]
    pub fn move_to_end(&mut self, text: &str) {
        let len = text.chars().count();
        self.anchor = len;
        self.cursor = len;
        self.dragging = false;
    }

    #[inline]
    pub fn set_caret(&mut self, char_idx: usize, text: &str) {
        let idx = char_idx.min(text.chars().count());
        self.anchor = idx;
        self.cursor = idx;
    }

    #[inline]
    pub fn extend_to(&mut self, char_idx: usize, text: &str) {
        let len = text.chars().count();
        self.anchor = self.anchor.min(len);
        self.cursor = char_idx.min(len);
    }

    #[inline]
    pub fn select_all(&mut self, text: &str) {
        let len = text.chars().count();
        self.anchor = 0;
        self.cursor = len;
    }

    #[inline]
    pub fn selection_chars(&self, text: &str) -> (usize, usize) {
        let len = text.chars().count();
        let a = self.anchor.min(len);
        let c = self.cursor.min(len);
        if a <= c { (a, c) } else { (c, a) }
    }

    #[inline]
    pub fn has_selection(&self, text: &str) -> bool {
        let (s, e) = self.selection_chars(text);
        s < e
    }

    pub fn selected_text<'a>(&self, text: &'a str) -> &'a str {
        let (s, e) = self.selection_chars(text);
        if s >= e {
            return "";
        }
        let sb = char_to_byte(text, s);
        let eb = char_to_byte(text, e);
        &text[sb..eb]
    }

    /// 双击选词：按字母数字 / CJK / 空白 / 标点自然切词
    pub fn select_word_at(&mut self, text: &str, char_idx: usize) {
        let chars: Vec<char> = text.chars().collect();
        if chars.is_empty() {
            self.anchor = 0;
            self.cursor = 0;
            return;
        }
        let idx = char_idx.min(chars.len().saturating_sub(1));
        let kind = char_word_class(chars[idx]);
        let mut start = idx;
        while start > 0 && char_word_class(chars[start - 1]) == kind {
            start -= 1;
        }
        let mut end = idx + 1;
        while end < chars.len() && char_word_class(chars[end]) == kind {
            end += 1;
        }
        self.anchor = start;
        self.cursor = end;
    }

    /// 在当前光标处插入文本（若存在选区则先替换选区）
    pub fn insert_text(&mut self, buf: &mut String, text: &str, digits_only: bool) -> bool {
        let cleaned: String = text
            .chars()
            .filter(|c| !c.is_control() && (!digits_only || c.is_ascii_digit()))
            .collect();
        if cleaned.is_empty() {
            return false;
        }
        let (start, end) = self.selection_chars(buf);
        let sb = char_to_byte(buf, start);
        let eb = char_to_byte(buf, end);
        buf.replace_range(sb..eb, &cleaned);
        let next = start + cleaned.chars().count();
        self.anchor = next;
        self.cursor = next;
        reset_caret_blink_epoch();
        true
    }

    /// 剪切当前选区，返回被剪下的文本；无选区时返回 `None`
    pub fn cut_selection(&mut self, buf: &mut String) -> Option<String> {
        let (start, end) = self.selection_chars(buf);
        if start >= end {
            return None;
        }
        let sb = char_to_byte(buf, start);
        let eb = char_to_byte(buf, end);
        let cut = buf[sb..eb].to_string();
        buf.replace_range(sb..eb, "");
        self.anchor = start;
        self.cursor = start;
        reset_caret_blink_epoch();
        Some(cut)
    }

    /// 删除当前选区
    pub fn delete_selection(&mut self, buf: &mut String) -> bool {
        let (start, end) = self.selection_chars(buf);
        if start >= end {
            return false;
        }
        let sb = char_to_byte(buf, start);
        let eb = char_to_byte(buf, end);
        buf.replace_range(sb..eb, "");
        self.anchor = start;
        self.cursor = start;
        reset_caret_blink_epoch();
        true
    }

    /// 处理全部原生单行文本编辑/导航/选区按键
    pub fn apply_key(&mut self, buf: &mut String, ks: &Keystroke) -> TextKeyOutcome {
        self.clamp(buf);
        let len = buf.chars().count();
        let key = ks.key.as_str();
        let cmd = ks.modifiers.platform;
        let ctrl = ks.modifiers.control;
        let alt = ks.modifiers.alt;
        let shift = ks.modifiers.shift;

        // 1. 全选（⌘A）
        if cmd && !ctrl && !alt && !shift && key.eq_ignore_ascii_case("a") {
            self.select_all(buf);
            reset_caret_blink_epoch();
            return TextKeyOutcome::CursorMoved;
        }

        // 2. 光标左移 / 扩选（← / ⌥← / ⌘← / ⇧← / Home / Ctrl+A）
        if key == "left" || key == "home" || (ctrl && !cmd && !alt && key.eq_ignore_ascii_case("a"))
        {
            let target = if key == "home" || cmd || ctrl {
                0
            } else if alt {
                prev_word_boundary(buf, self.cursor)
            } else if !shift && self.has_selection(buf) {
                self.selection_chars(buf).0
            } else {
                self.cursor.saturating_sub(1)
            };
            self.cursor = target;
            if !shift {
                self.anchor = target;
            }
            reset_caret_blink_epoch();
            return TextKeyOutcome::CursorMoved;
        }

        // 3. 光标右移 / 扩选（→ / ⌥→ / ⌘→ / ⇧→ / End / Ctrl+E）
        if key == "right"
            || key == "end"
            || (ctrl && !cmd && !alt && key.eq_ignore_ascii_case("e"))
        {
            let target = if key == "end" || cmd || ctrl {
                len
            } else if alt {
                next_word_boundary(buf, self.cursor)
            } else if !shift && self.has_selection(buf) {
                self.selection_chars(buf).1
            } else {
                (self.cursor + 1).min(len)
            };
            self.cursor = target;
            if !shift {
                self.anchor = target;
            }
            reset_caret_blink_epoch();
            return TextKeyOutcome::CursorMoved;
        }

        // 4. 上/下方向键在单行输入框内映射为行首/行尾（macOS 原生 NSTextField 行为）
        if (key == "up" || key == "down") && !cmd && !ctrl && !alt {
            let target = if key == "up" { 0 } else { len };
            self.cursor = target;
            if !shift {
                self.anchor = target;
            }
            reset_caret_blink_epoch();
            return TextKeyOutcome::CursorMoved;
        }

        // 5. Ctrl+U 清空整行
        if ctrl && !cmd && !alt && key.eq_ignore_ascii_case("u") {
            if buf.is_empty() {
                return TextKeyOutcome::CursorMoved;
            }
            buf.clear();
            self.anchor = 0;
            self.cursor = 0;
            reset_caret_blink_epoch();
            return TextKeyOutcome::Modified;
        }

        // 6. Ctrl+K 删除光标至行尾
        if ctrl && !cmd && !alt && key.eq_ignore_ascii_case("k") {
            if self.delete_selection(buf) {
                return TextKeyOutcome::Modified;
            }
            if self.cursor < len {
                let sb = char_to_byte(buf, self.cursor);
                buf.truncate(sb);
                self.anchor = self.cursor;
                reset_caret_blink_epoch();
                return TextKeyOutcome::Modified;
            }
            return TextKeyOutcome::CursorMoved;
        }

        // 7. Backspace（含选区删除、⌘Backspace 删至行首、⌥Backspace 按词删除）
        if key == "backspace" {
            if self.delete_selection(buf) {
                return TextKeyOutcome::Modified;
            }
            if self.cursor == 0 {
                return TextKeyOutcome::CursorMoved;
            }
            let del_start = if cmd {
                0
            } else if alt {
                prev_word_boundary(buf, self.cursor)
            } else {
                self.cursor - 1
            };
            let sb = char_to_byte(buf, del_start);
            let eb = char_to_byte(buf, self.cursor);
            buf.replace_range(sb..eb, "");
            self.anchor = del_start;
            self.cursor = del_start;
            reset_caret_blink_epoch();
            return TextKeyOutcome::Modified;
        }

        // 8. Forward Delete（Delete / Fn+Backspace / Ctrl+D，含 ⌥Delete 按词向后删）
        if key == "delete" || (ctrl && !cmd && !alt && key.eq_ignore_ascii_case("d")) {
            if self.delete_selection(buf) {
                return TextKeyOutcome::Modified;
            }
            if self.cursor >= len {
                return TextKeyOutcome::CursorMoved;
            }
            let del_end = if alt {
                next_word_boundary(buf, self.cursor)
            } else {
                self.cursor + 1
            };
            let sb = char_to_byte(buf, self.cursor);
            let eb = char_to_byte(buf, del_end);
            buf.replace_range(sb..eb, "");
            self.anchor = self.cursor;
            reset_caret_blink_epoch();
            return TextKeyOutcome::Modified;
        }

        TextKeyOutcome::Unhandled
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WordClass {
    Whitespace,
    Word,
    Punct,
}

#[inline]
fn char_word_class(c: char) -> WordClass {
    if c.is_whitespace() {
        WordClass::Whitespace
    } else if c.is_alphanumeric() || c == '_' {
        WordClass::Word
    } else {
        WordClass::Punct
    }
}

fn prev_word_boundary(text: &str, cursor: usize) -> usize {
    let chars: Vec<char> = text.chars().collect();
    let mut i = cursor.min(chars.len());
    while i > 0 && chars[i - 1].is_whitespace() {
        i -= 1;
    }
    if i == 0 {
        return 0;
    }
    let kind = char_word_class(chars[i - 1]);
    while i > 0 && char_word_class(chars[i - 1]) == kind {
        i -= 1;
    }
    i
}

fn next_word_boundary(text: &str, cursor: usize) -> usize {
    let chars: Vec<char> = text.chars().collect();
    let len = chars.len();
    let mut i = cursor.min(len);
    while i < len && chars[i].is_whitespace() {
        i += 1;
    }
    if i >= len {
        return len;
    }
    let kind = char_word_class(chars[i]);
    while i < len && char_word_class(chars[i]) == kind {
        i += 1;
    }
    i
}

#[inline]
pub fn char_to_byte(s: &str, char_idx: usize) -> usize {
    if char_idx == 0 {
        return 0;
    }
    s.char_indices()
        .nth(char_idx)
        .map(|(b, _)| b)
        .unwrap_or(s.len())
}

#[inline]
pub fn byte_to_char(s: &str, byte_idx: usize) -> usize {
    let clamped = byte_idx.min(s.len());
    s.char_indices()
        .take_while(|(b, _)| *b < clamped)
        .count()
}

// ────────── 渲染态与精确像素命中缓存（thread-local，与 GPUI 主线程 1:1 绑定）──────────

struct CachedInputLayout {
    bounds: Bounds<Pixels>,
    scroll_x: f32,
    /// 不含 IME 预编辑文本的纯显示文本整形结果，用于鼠标坐标反查精确字符索引
    shaped_base: ShapedLine,
    base_text: String,
    is_placeholder: bool,
}

thread_local! {
    static RENDER_ACTIVE_INPUT: Cell<ActiveInput> = const { Cell::new(ActiveInput::Terminal) };
    static RENDER_TEXT_CURSOR: Cell<TextCursorState> = const { Cell::new(TextCursorState { anchor: 0, cursor: 0, dragging: false }) };
    static CARET_BLINK_EPOCH: RefCell<Instant> = RefCell::new(Instant::now());
    static INPUT_LAYOUT_CACHE: RefCell<HashMap<ActiveInput, CachedInputLayout>> = RefCell::new(HashMap::new());
}

/// 每帧渲染前由 `HoloSshApp::render` 同步当前活跃输入框与光标选区状态
pub fn sync_active_text_input_render_state(active: ActiveInput, cursor: TextCursorState) {
    RENDER_ACTIVE_INPUT.with(|s| s.set(active));
    RENDER_TEXT_CURSOR.with(|s| s.set(cursor));
}

/// 每次按键或点击时重置光标闪烁基准，使光标立刻实心可见（原生 macOS 手感）
pub fn reset_caret_blink_epoch() {
    CARET_BLINK_EPOCH.with(|slot| *slot.borrow_mut() = Instant::now());
}

/// 根据鼠标窗口坐标，精准反查指定输入框内最接近的字符索引（CoreText 真实字形坐标）
pub fn hit_test_text_input(target: ActiveInput, pos: Point<Pixels>) -> Option<usize> {
    INPUT_LAYOUT_CACHE.with(|cache| {
        let map = cache.borrow();
        let layout = map.get(&target)?;
        if layout.is_placeholder || layout.base_text.is_empty() {
            return Some(0);
        }
        let origin_x: f32 = layout.bounds.origin.x.into();
        let click_x: f32 = pos.x.into();
        let local_x = (click_x - origin_x + layout.scroll_x).max(0.0);
        let byte_idx = layout.shaped_base.closest_index_for_x(px(local_x));
        Some(byte_to_char(&layout.base_text, byte_idx))
    })
}

/// 构造统一的单行可编辑文本 GPUI 元素（支持任意位置光标、选区高亮、水平自动滚动与 IME 跟随）
#[allow(clippy::too_many_arguments)]
pub fn editable_text_line(
    target: ActiveInput,
    value: &str,
    placeholder: &str,
    masked: bool,
    focused: bool,
    font_size: Pixels,
    caret_height_px: f32,
    text_color: Hsla,
    placeholder_color: Hsla,
    font_family: Option<&'static str>,
    theme: &ThemeTokens,
) -> EditableTextElement {
    let is_placeholder = value.is_empty() && !placeholder.is_empty();
    let display_text = if value.is_empty() {
        if focused {
            String::new()
        } else {
            placeholder.to_string()
        }
    } else if masked {
        "•".repeat(value.chars().count())
    } else {
        value.to_string()
    };

    let render_placeholder = value.is_empty() && !focused;
    let is_dark = theme.is_dark;
    let selection_bg = if is_dark {
        Hsla::from(Rgba {
            r: 1.0,
            g: 1.0,
            b: 1.0,
            a: 0.24,
        })
    } else {
        Hsla::from(Rgba {
            r: 0.08,
            g: 0.14,
            b: 0.24,
            a: 0.18,
        })
    };

    EditableTextElement {
        target,
        display_text,
        placeholder_when_focused: if focused && is_placeholder {
            Some(placeholder.to_string())
        } else {
            None
        },
        is_placeholder: render_placeholder,
        focused,
        font_size,
        caret_height_px,
        text_color: if render_placeholder {
            placeholder_color
        } else {
            text_color
        },
        placeholder_color,
        caret_color: theme.text_primary,
        selection_bg,
        font_family,
    }
}

pub struct EditableTextElement {
    target: ActiveInput,
    display_text: String,
    /// 聚焦且文本为空时可选渲染的淡色占位提示（位于光标右侧）
    placeholder_when_focused: Option<String>,
    is_placeholder: bool,
    focused: bool,
    font_size: Pixels,
    caret_height_px: f32,
    text_color: Hsla,
    placeholder_color: Hsla,
    caret_color: Hsla,
    selection_bg: Hsla,
    font_family: Option<&'static str>,
}

pub struct EditableTextPrepaint {
    shaped_display: ShapedLine,
    shaped_placeholder: Option<ShapedLine>,
    scroll_x: f32,
    line_height: f32,
    selection_quad: Option<PaintQuad>,
    ime_underline_quad: Option<PaintQuad>,
    caret_quad: Option<PaintQuad>,
    request_blink_frame: bool,
}

impl IntoElement for EditableTextElement {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl EditableTextElement {
    fn build_font(&self) -> Font {
        let family = self.font_family.unwrap_or(".SystemUIFont");
        Font {
            family: family.into(),
            features: FontFeatures::default(),
            fallbacks: None,
            weight: FontWeight::NORMAL,
            style: FontStyle::Normal,
        }
    }
}

impl Element for EditableTextElement {
    type RequestLayoutState = ();
    type PrepaintState = EditableTextPrepaint;

    fn id(&self) -> Option<ElementId> {
        None
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
        let fs: f32 = self.font_size.into();
        let line_h = (fs * 1.38).round().max(self.caret_height_px + 2.0);
        let mut style = Style::default();
        style.flex_grow = 1.0;
        style.flex_shrink = 1.0;
        style.min_size.width = px(0.0).into();
        style.size.width = gpui::relative(1.0).into();
        style.size.height = px(line_h).into();
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
        let fs: f32 = self.font_size.into();
        let line_height: f32 = bounds.size.height.into();
        let bounds_w: f32 = bounds.size.width.into();
        let origin_x: f32 = bounds.origin.x.into();
        let origin_y: f32 = bounds.origin.y.into();
        let font = self.build_font();

        // 1. 整形基础显示文本（供鼠标精确点击命中与选区坐标计算）
        let base_run = TextRun {
            len: self.display_text.len(),
            font: font.clone(),
            color: self.text_color,
            background_color: None,
            underline: None,
            strikethrough: None,
        };
        let base_runs = [base_run];
        let shaped_base = window.text_system().shape_line(
            SharedString::from(self.display_text.clone()),
            px(fs),
            if self.display_text.is_empty() {
                &[]
            } else {
                &base_runs
            },
            None,
        );

        let is_active_focus =
            self.focused && RENDER_ACTIVE_INPUT.with(|s| s.get() == self.target);
        let ime_marked = if is_active_focus {
            active_ime_marked_text()
        } else {
            None
        };

        let mut cursor_state = if is_active_focus {
            RENDER_TEXT_CURSOR.with(|s| s.get())
        } else {
            TextCursorState::default()
        };
        cursor_state.clamp(&self.display_text);

        // 2. 若正在进行 IME 预编辑，将预编辑文本临时拼入光标处并加下划线
        let (shaped_display, ime_span_bytes, effective_caret_byte) = if let Some(ref preedit) =
            ime_marked
        {
            let (sel_s, sel_e) = cursor_state.selection_chars(&self.display_text);
            let sb = char_to_byte(&self.display_text, sel_s);
            let eb = char_to_byte(&self.display_text, sel_e);
            let mut composed = String::with_capacity(self.display_text.len() + preedit.len());
            composed.push_str(&self.display_text[..sb]);
            let ime_start = composed.len();
            composed.push_str(preedit);
            let ime_end = composed.len();
            composed.push_str(&self.display_text[eb..]);

            let mut runs = Vec::new();
            if ime_start > 0 {
                runs.push(TextRun {
                    len: ime_start,
                    font: font.clone(),
                    color: self.text_color,
                    background_color: None,
                    underline: None,
                    strikethrough: None,
                });
            }
            runs.push(TextRun {
                len: ime_end - ime_start,
                font: font.clone(),
                color: self.text_color,
                background_color: None,
                underline: Some(UnderlineStyle {
                    color: Some(self.caret_color),
                    thickness: px(1.0),
                    wavy: false,
                }),
                strikethrough: None,
            });
            if ime_end < composed.len() {
                runs.push(TextRun {
                    len: composed.len() - ime_end,
                    font: font.clone(),
                    color: self.text_color,
                    background_color: None,
                    underline: None,
                    strikethrough: None,
                });
            }
            let shaped = window.text_system().shape_line(
                SharedString::from(composed),
                px(fs),
                &runs,
                None,
            );
            (shaped, Some((ime_start, ime_end)), ime_end)
        } else {
            let caret_byte = char_to_byte(&self.display_text, cursor_state.cursor);
            let display_run = TextRun {
                len: self.display_text.len(),
                font: font.clone(),
                color: self.text_color,
                background_color: None,
                underline: None,
                strikethrough: None,
            };
            let display_runs = [display_run];
            let shaped = window.text_system().shape_line(
                SharedString::from(self.display_text.clone()),
                px(fs),
                if self.display_text.is_empty() {
                    &[]
                } else {
                    &display_runs
                },
                None,
            );
            (shaped, None, caret_byte)
        };

        let line_w: f32 = shaped_display.width.into();
        let caret_raw_x: f32 = shaped_display.x_for_index(effective_caret_byte).into();

        // 3. 计算水平自动滚动偏移，确保长文本输入/移动光标时永远可见
        let prev_scroll = INPUT_LAYOUT_CACHE.with(|cache| {
            cache
                .borrow()
                .get(&self.target)
                .map(|c| c.scroll_x)
                .unwrap_or(0.0)
        });
        let scroll_x = if is_active_focus && bounds_w > 8.0 {
            let mut sx = prev_scroll;
            let pad_left = 2.0;
            let pad_right = 4.0;
            if caret_raw_x < sx + pad_left {
                sx = (caret_raw_x - pad_left).max(0.0);
            } else if caret_raw_x > sx + bounds_w - pad_right {
                sx = (caret_raw_x - bounds_w + pad_right).max(0.0);
            }
            let max_scroll = (line_w - bounds_w + pad_right + 2.0).max(0.0);
            sx.clamp(0.0, max_scroll)
        } else {
            0.0
        };

        // 写入命中缓存（含非聚焦态，保证首次点击任意文字中间即可一次精准落位）
        INPUT_LAYOUT_CACHE.with(|cache| {
            cache.borrow_mut().insert(
                self.target,
                CachedInputLayout {
                    bounds,
                    scroll_x,
                    shaped_base,
                    base_text: self.display_text.clone(),
                    is_placeholder: self.is_placeholder,
                },
            );
        });

        let caret_h = self.caret_height_px.min(line_height);
        let caret_top = origin_y + ((line_height - caret_h) * 0.5).max(0.0);

        // 4. 选区高亮块（仅聚焦且无 IME 预编辑且存在非空选区时绘制）
        let mut selection_quad = None;
        if is_active_focus && ime_span_bytes.is_none() && cursor_state.has_selection(&self.display_text)
        {
            let (sel_s, sel_e) = cursor_state.selection_chars(&self.display_text);
            let sb = char_to_byte(&self.display_text, sel_s);
            let eb = char_to_byte(&self.display_text, sel_e);
            let sx: f32 = shaped_display.x_for_index(sb).into();
            let ex: f32 = shaped_display.x_for_index(eb).into();
            let w = (ex - sx).max(2.0);
            selection_quad = Some(
                fill(
                    Bounds::new(
                        point(px(origin_x + sx - scroll_x), px(caret_top - 1.0)),
                        size(px(w), px(caret_h + 2.0)),
                    ),
                    self.selection_bg,
                )
                .corner_radii(px(3.0)),
            );
        }

        // 5. 光标绘制 + IME 候选框屏幕坐标回写
        let mut caret_quad = None;
        let mut request_blink_frame = false;
        if is_active_focus {
            let caret_screen_x = origin_x + caret_raw_x - scroll_x;
            set_active_ime_caret_bounds(Bounds::new(
                point(px(caret_screen_x), px(caret_top)),
                size(px(1.5), px(caret_h)),
            ));

            let has_sel = ime_span_bytes.is_none() && cursor_state.has_selection(&self.display_text);
            if !has_sel {
                let elapsed = CARET_BLINK_EPOCH.with(|e| e.borrow().elapsed().as_secs_f32());
                // 刚移动/键入的前 0.45s 保持 100% 实心，随后平滑正弦呼吸闪烁
                let alpha = if elapsed < 0.45 {
                    1.0
                } else {
                    let phase = ((elapsed - 0.45) / 1.05) * std::f32::consts::TAU;
                    (phase.cos() * 0.5 + 0.5).clamp(0.14, 1.0)
                };
                let mut c = self.caret_color;
                c.a *= alpha;
                caret_quad = Some(
                    fill(
                        Bounds::new(
                            point(px(caret_screen_x), px(caret_top)),
                            size(px(1.5), px(caret_h)),
                        ),
                        c,
                    )
                    .corner_radii(px(1.0)),
                );
                request_blink_frame = true;
            }
        }

        // 6. 聚焦且空文本时的可选淡色占位提示（紧贴光标右侧）
        let shaped_placeholder = if ime_marked.is_none() && self.display_text.is_empty() {
            self.placeholder_when_focused.as_ref().map(|ph| {
                let run = TextRun {
                    len: ph.len(),
                    font: font.clone(),
                    color: self.placeholder_color,
                    background_color: None,
                    underline: None,
                    strikethrough: None,
                };
                window.text_system().shape_line(
                    SharedString::from(ph.clone()),
                    px(fs),
                    &[run],
                    None,
                )
            })
        } else {
            None
        };

        EditableTextPrepaint {
            shaped_display,
            shaped_placeholder,
            scroll_x,
            line_height,
            selection_quad,
            ime_underline_quad: None,
            caret_quad,
            request_blink_frame,
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
        let origin_x: f32 = bounds.origin.x.into();
        let origin_y: f32 = bounds.origin.y.into();
        let lh_px = px(prepaint.line_height);

        window.with_content_mask(Some(ContentMask { bounds }), |window| {
            if let Some(quad) = prepaint.selection_quad.take() {
                window.paint_quad(quad);
            }
            if let Some(ph) = prepaint.shaped_placeholder.as_ref() {
                let _ = ph.paint(point(px(origin_x + 3.0), px(origin_y)), lh_px, window, cx);
            }
            if !self.display_text.is_empty() || active_ime_marked_text().is_some() {
                let _ = prepaint.shaped_display.paint(
                    point(px(origin_x - prepaint.scroll_x), px(origin_y)),
                    lh_px,
                    window,
                    cx,
                );
            }
            if let Some(quad) = prepaint.ime_underline_quad.take() {
                window.paint_quad(quad);
            }
            if let Some(quad) = prepaint.caret_quad.take() {
                window.paint_quad(quad);
            }
        });

        if prepaint.request_blink_frame {
            window.request_animation_frame();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::Modifiers;

    fn ks(key: &str, mods: Modifiers) -> Keystroke {
        Keystroke {
            modifiers: mods,
            key: key.to_string(),
            key_char: None,
        }
    }

    #[test]
    fn supports_mid_text_insert_move_select_cut_and_word_jumps() {
        let mut buf = "hello 世界 world".to_string();
        let mut st = TextCursorState::default();
        st.move_to_end(&buf);
        assert_eq!(st.cursor, 14);

        // 光标移到中间并插入字符
        st.set_caret(6, &buf);
        assert!(st.insert_text(&mut buf, "美妙", false));
        assert_eq!(buf, "hello 美妙世界 world");
        assert_eq!(st.cursor, 8);

        // Shift+Left 选中刚插入的两个汉字并剪切
        let shift = Modifiers {
            shift: true,
            ..Modifiers::default()
        };
        assert_eq!(st.apply_key(&mut buf, &ks("left", shift)), TextKeyOutcome::CursorMoved);
        assert_eq!(st.apply_key(&mut buf, &ks("left", shift)), TextKeyOutcome::CursorMoved);
        assert_eq!(st.selected_text(&buf), "美妙");
        assert_eq!(st.cut_selection(&mut buf).as_deref(), Some("美妙"));
        assert_eq!(buf, "hello 世界 world");
        assert_eq!(st.cursor, 6);

        // ⌘A 全选 + 粘贴替换
        let cmd = Modifiers {
            platform: true,
            ..Modifiers::default()
        };
        assert_eq!(st.apply_key(&mut buf, &ks("a", cmd)), TextKeyOutcome::CursorMoved);
        assert_eq!(st.selected_text(&buf), "hello 世界 world");
        assert!(st.insert_text(&mut buf, "127.0.0.1", false));
        assert_eq!(buf, "127.0.0.1");

        // 双击选词与 ⌥Backspace 按词删除
        st.select_word_at(&buf, 1);
        assert_eq!(st.selected_text(&buf), "127");
        assert_eq!(
            st.apply_key(&mut buf, &ks("backspace", Modifiers::default())),
            TextKeyOutcome::Modified
        );
        assert_eq!(buf, ".0.0.1");

        // Delete 向后删
        assert_eq!(
            st.apply_key(&mut buf, &ks("delete", Modifiers::default())),
            TextKeyOutcome::Modified
        );
        assert_eq!(buf, "0.0.1");
    }
}
