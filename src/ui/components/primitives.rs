//! 纯 GPUI 基础磨砂原子控件与动效辅助（严格映射 Figma `HoloSSH / Theme` 与同源 SVG 图标）

use crate::theme::{LayoutMetrics, ThemeTokens};
use gpui::{
    Animation, AnimationExt, Bounds, Div, Hsla, InteractiveElement, IntoElement, ParentElement,
    PathBuilder, Pixels, SharedString, Styled, Svg, Transformation, canvas, div, ease_out_quint,
    percentage, point, px, svg,
};
use std::cell::{Cell, RefCell};
use std::time::Duration;

thread_local! {
    static ACTIVE_IME_MARKED_TEXT: RefCell<Option<String>> = const { RefCell::new(None) };
    static ACTIVE_IME_CARET_BOUNDS: Cell<Option<Bounds<Pixels>>> = const { Cell::new(None) };
}

/// 每帧渲染前同步当前 IME 预编辑文本（供所有输入框内联展示未上屏拼音/假名/韩文）
pub fn set_active_ime_marked_text(text: Option<&str>) {
    ACTIVE_IME_MARKED_TEXT.with(|slot| {
        let mut guard = slot.borrow_mut();
        match text.filter(|s| !s.is_empty()) {
            Some(t) => *guard = Some(t.to_string()),
            None => *guard = None,
        }
    });
}

/// 读取当前 IME 预编辑文本（供通用输入框元素内联渲染）
pub fn active_ime_marked_text() -> Option<String> {
    ACTIVE_IME_MARKED_TEXT.with(|slot| slot.borrow().clone())
}

/// 更新当前聚焦输入框光标的精确屏幕位置（供 macOS IME 候选框紧贴光标下方弹出）
pub fn set_active_ime_caret_bounds(bounds: Bounds<Pixels>) {
    ACTIVE_IME_CARET_BOUNDS.with(|slot| slot.set(Some(bounds)));
}

/// 获取当前聚焦输入框光标的精确屏幕位置（供 macOS IME 候选框紧贴光标下方弹出）
pub fn current_ime_caret_bounds() -> Option<Bounds<Pixels>> {
    ACTIVE_IME_CARET_BOUNDS.with(|slot| slot.get())
}

/// 渲染 Figma 同源单色 SVG 图标
pub fn icon_svg(path: &'static str, size_px: f32, color: Hsla) -> Svg {
    svg().path(path).size(px(size_px)).text_color(color)
}

/// 1px 极细水平分割线（对应 Figma `border/subtle`）
pub fn hairline_h(theme: &ThemeTokens) -> Div {
    div().w_full().h(px(1.0)).bg(theme.border_subtle)
}

/// 在线/空闲状态圆点（对应 Figma `status/connected` / `status/idle`）
pub fn status_dot(connected: bool, theme: &ThemeTokens) -> Div {
    div()
        .w(px(6.5))
        .h(px(6.5))
        .rounded(LayoutMetrics::radius_pill())
        .bg(if connected {
            theme.status_connected
        } else {
            theme.status_idle
        })
}

#[allow(dead_code)]
/// 平滑呼吸光标（自动记录屏幕坐标供 macOS IME 候选框紧贴跟随，并内联渲染中/日/韩预编辑文本）
pub fn blinking_caret(id: impl Into<String>, height_px: f32, theme: &ThemeTokens) -> impl IntoElement {
    let color = theme.text_primary;
    let id_str = id.into();
    let marked = ACTIVE_IME_MARKED_TEXT.with(|slot| slot.borrow().clone());

    let caret_bar = div()
        .relative()
        .flex_none()
        .w(px(1.5))
        .h(px(height_px))
        .rounded(px(1.0))
        .bg(color)
        .child(
            canvas(
                move |_bounds, _window, _cx| {},
                move |bounds: Bounds<Pixels>, _, _window, _cx| {
                    ACTIVE_IME_CARET_BOUNDS.with(|slot| slot.set(Some(bounds)));
                },
            )
            .absolute()
            .inset_0(),
        )
        .with_animation(
            SharedString::from(format!("caret-{id_str}")),
            Animation::new(Duration::from_millis(1050)).repeat(),
            move |el, delta| {
                // 平滑正弦呼吸闪烁（避免硬切闪烁）
                let alpha = ((delta * std::f32::consts::TAU).cos() * 0.5 + 0.5).clamp(0.12, 1.0);
                el.opacity(alpha)
            },
        );

    let mut row = div().flex_none().flex().items_center();
    if let Some(preedit) = marked {
        row = row.child(
            div()
                .flex_none()
                .px(px(1.0))
                .border_b_1()
                .border_color(color)
                .text_size(LayoutMetrics::font_caption())
                .text_color(color)
                .child(preedit),
        );
    }
    row.child(caret_bar)
}

/// 统一磨砂胶囊按钮（带悬停高光与按压触感回弹反馈）
pub fn glass_pill_button(label: impl Into<String>, primary: bool, theme: &ThemeTokens) -> Div {
    let label_str = label.into();
    let base_bg = if primary {
        theme.surface_active
    } else {
        theme.surface_pill
    };
    let hover_bg = if primary {
        boost_alpha(theme.surface_active, 0.08)
    } else {
        theme.surface_active
    };
    let specular = theme.border_specular;

    div()
        .h(px(30.0))
        .px(px(15.0))
        .flex()
        .items_center()
        .justify_center()
        .rounded(LayoutMetrics::radius_pill())
        .bg(base_bg)
        .border_1()
        .border_color(specular)
        .text_size(LayoutMetrics::font_control())
        .text_color(theme.text_primary)
        .cursor_pointer()
        .hover(move |s| s.bg(hover_bg).border_color(specular))
        .child(label_str)
}

/// 带唯一动效标识的原生单色磨砂开关（滑块丝滑左右滑动）
pub fn native_toggle_with_id(id_hint: &'static str, is_on: bool, theme: &ThemeTokens) -> Div {
    let base_bg = if is_on {
        theme.surface_active
    } else {
        theme.surface_pill
    };
    let hover_bg = boost_alpha(base_bg, 0.06);
    let thumb_color = theme.text_primary;

    let thumb = div()
        .absolute()
        .top(px(1.0))
        .w(px(15.0))
        .h(px(15.0))
        .rounded(LayoutMetrics::radius_pill())
        .bg(thumb_color)
        .with_animation(
            SharedString::from(format!("thumb-{id_hint}-{is_on}")),
            Animation::new(Duration::from_millis(165)).with_easing(ease_out_quint()),
            move |el, delta| {
                let left_px = if is_on {
                    2.0 + 13.0 * delta
                } else {
                    15.0 - 13.0 * delta
                };
                el.left(px(left_px))
            },
        );

    div()
        .relative()
        .w(px(34.0))
        .h(px(19.0))
        .rounded(LayoutMetrics::radius_pill())
        .bg(base_bg)
        .border_1()
        .border_color(theme.border_specular)
        .cursor_pointer()
        .hover(move |s| s.bg(hover_bg))
        .child(thumb)
}

/// 原生下拉框控件（带悬停高光边框反馈）
pub fn native_dropdown(value: impl Into<String>, width_px: f32, theme: &ThemeTokens) -> Div {
    let val = value.into();
    let hover_bg = theme.surface_active;
    let hover_border = theme.border_specular;

    div()
        .w(px(width_px))
        .h(px(28.0))
        .px(px(10.0))
        .flex()
        .items_center()
        .justify_between()
        .rounded(px(8.0))
        .bg(theme.surface_input)
        .border_1()
        .border_color(theme.border_subtle)
        .text_size(LayoutMetrics::font_control())
        .text_color(theme.text_primary)
        .cursor_pointer()
        .hover(move |s| s.bg(hover_bg).border_color(hover_border))
        .child(val)
        .child(icon_svg("icons/chevron-down.svg", 12.0, theme.text_tertiary))
}

/// 轻微提亮磨砂表面透明度，用于悬停态微光反馈
#[inline]
pub fn boost_alpha(color: Hsla, delta: f32) -> Hsla {
    let mut c = color;
    c.a = (c.a + delta).clamp(0.0, 1.0);
    c
}

/// 矢量圆环下载进度图标（底环 + 顺时针 0%→100% 进度弧，用于云端内置字体下载态）
pub fn circular_progress_ring(
    progress: f32,
    size_px: f32,
    theme: &ThemeTokens,
) -> impl IntoElement {
    let clamped = progress.clamp(0.04, 1.0);
    let track_color = theme.border_specular;
    let fill_color = theme.text_primary;

    canvas(
        move |_bounds, _window, _cx| {},
        move |bounds: Bounds<Pixels>, _, window, _cx| {
            let w = f32::from(bounds.size.width);
            let h = f32::from(bounds.size.height);
            let cx_p = f32::from(bounds.origin.x) + w * 0.5;
            let cy_p = f32::from(bounds.origin.y) + h * 0.5;
            let r = (w.min(h) * 0.5 - 1.5).max(2.0);
            let segs = 28usize;

            // 底环（完整浅灰圆圈）
            let mut track = PathBuilder::stroke(px(1.5));
            for i in 0..=segs {
                let angle = -std::f32::consts::FRAC_PI_2
                    + (i as f32 / segs as f32) * std::f32::consts::TAU;
                let pt = point(px(cx_p + r * angle.cos()), px(cy_p + r * angle.sin()));
                if i == 0 {
                    track.move_to(pt);
                } else {
                    track.line_to(pt);
                }
            }
            if let Ok(path) = track.build() {
                window.paint_path(path, track_color);
            }

            // 进度弧（从 12 点钟方向顺时针填充）
            let sweep = clamped * std::f32::consts::TAU;
            let arc_segs = ((segs as f32 * clamped).ceil() as usize).max(3);
            let mut arc = PathBuilder::stroke(px(1.65));
            for i in 0..=arc_segs {
                let angle = -std::f32::consts::FRAC_PI_2 + (i as f32 / arc_segs as f32) * sweep;
                let pt = point(px(cx_p + r * angle.cos()), px(cy_p + r * angle.sin()));
                if i == 0 {
                    arc.move_to(pt);
                } else {
                    arc.line_to(pt);
                }
            }
            if let Ok(path) = arc.build() {
                window.paint_path(path, fill_color);
            }
        },
    )
    .w(px(size_px))
    .h(px(size_px))
    .flex_none()
}

/// 全局复用的单色旋转加载图标（仅在调用处渲染，零文案）
pub fn loading_spinner(id: &'static str, size_px: f32, theme: &ThemeTokens) -> impl IntoElement {
    let color = theme.text_primary;
    svg()
        .path("icons/spinner.svg")
        .size(px(size_px))
        .text_color(color)
        .with_animation(
            SharedString::from(format!("loading-spin-{id}")),
            Animation::new(Duration::from_millis(820)).repeat(),
            move |el, delta| el.with_transformation(Transformation::rotate(percentage(delta))),
        )
}

/// 模块级居中加载弹层（严格限定在所属 `.relative()` 模块区域内居中浮现，不遮挡全局、无任何文案）
pub fn module_loading_overlay(id: &'static str, theme: &ThemeTokens) -> Div {
    div()
        .absolute()
        .inset_0()
        .flex()
        .items_center()
        .justify_center()
        .child(
            div()
                .w(px(40.0))
                .h(px(40.0))
                .rounded(px(12.0))
                .bg(theme.sheet_bg)
                .border_1()
                .border_color(theme.border_specular)
                .flex()
                .items_center()
                .justify_center()
                .child(loading_spinner(id, 18.0, theme)),
        )
}
