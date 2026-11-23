//! 纯 SSH 终端与递归分屏视图（严格对应 Figma 01 / 03 / 05 画板）
//!
//! 基于自定义 GPUI `TerminalElement`（`shape_line` + `paint_quad` + 增量行缓存）直接渲染终端画布，
//! 并内置 `⌘F` 终端搜索浮条与多行粘贴安全确认条（遵循统一黑白单色磨砂视觉红线）。

use crate::app::{ActiveInput, HoloSshApp, PendingPasteConfirm, TerminalSearchState};
use crate::i18n::{tr, trf};
use crate::services::terminal_emu::TerminalEmulator;
use crate::state::session::{SplitAxis, SplitNode, TerminalPaneState};
use crate::state::settings::CursorStyle;
use crate::theme::{LayoutMetrics, ThemeTokens};
use crate::ui::components::{editable_text_line, hairline_h, icon_svg, status_dot};
use crate::ui::workspace::terminal_element::{
    HoveredLinkSpan, SharedPaneMetrics, SharedPaneShapes, TerminalElement, TerminalSearchHighlight,
};
use gpui::{
    AnyElement, Context, Entity, ExternalPaths, FocusHandle, InteractiveElement, IntoElement,
    MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, ParentElement, ScrollWheelEvent,
    SharedString, StatefulInteractiveElement, Styled, div, px, relative,
};
use std::collections::HashMap;
use std::sync::Arc;

/// 渲染终端分屏树所需的统一只读上下文
pub struct TerminalRenderParams<'a> {
    pub focused_pane_id: &'a str,
    pub show_pane_headers: bool,
    pub emulators: &'a HashMap<String, TerminalEmulator>,
    pub font_family: &'a str,
    pub base_font_size: f32,
    pub cursor_style: CursorStyle,
    pub is_dark: bool,
    pub theme: &'a ThemeTokens,
    pub active_input: ActiveInput,
    pub ime_marked_text: Option<&'a str>,
    pub search_state: Option<&'a TerminalSearchState>,
    pub paste_confirm: Option<&'a PendingPasteConfirm>,
    pub hovered_link: Option<(&'a str, HoveredLinkSpan)>,
    pub focus_handle: FocusHandle,
    pub app_entity: Entity<HoloSshApp>,
    pub metrics_store: SharedPaneMetrics,
    pub shape_cache: SharedPaneShapes,
}

pub fn render_split_tree(
    node: &SplitNode,
    params: &TerminalRenderParams<'_>,
    cx: &mut Context<HoloSshApp>,
) -> AnyElement {
    render_split_tree_at(node, String::new(), params.show_pane_headers, params, cx)
}

fn render_split_tree_at(
    node: &SplitNode,
    path: String,
    show_header: bool,
    params: &TerminalRenderParams<'_>,
    cx: &mut Context<HoloSshApp>,
) -> AnyElement {
    match node {
        SplitNode::Leaf(pane) => {
            let is_focused = pane.pane_id == params.focused_pane_id;
            render_single_pane(pane, is_focused, show_header, params, cx).into_any_element()
        }
        SplitNode::Split {
            axis,
            ratio,
            first,
            second,
        } => {
            let first_path = format!("{path}0");
            let second_path = format!("{path}1");
            let drag_path = path.clone();
            let ratio_clamped = ratio.clamp(0.20, 0.80);

            match axis {
                SplitAxis::Horizontal => {
                    let first_pane = div()
                        .relative()
                        .flex_none()
                        .w(relative(ratio_clamped))
                        .min_w_0()
                        .min_h_0()
                        .h_full()
                        .overflow_hidden()
                        .border_r_1()
                        .border_color(params.theme.border_subtle)
                        .child(render_split_tree_at(first, first_path, true, params, cx))
                        .child(
                            div()
                                .id(SharedString::from(format!("split-drag-h-{path}")))
                                .absolute()
                                .top_0()
                                .bottom_0()
                                .right_0()
                                .w(px(7.0))
                                .cursor(gpui::CursorStyle::ResizeLeftRight)
                                .hover(|s| s.bg(params.theme.surface_pill))
                                .on_mouse_down(
                                    MouseButton::Left,
                                    cx.listener(move |this, event: &MouseDownEvent, _window, cx| {
                                        this.start_drag_split(
                                            drag_path.clone(),
                                            SplitAxis::Horizontal,
                                            event.position,
                                        );
                                        cx.stop_propagation();
                                    }),
                                ),
                        );

                    let second_pane = div()
                        .flex_1()
                        .min_w_0()
                        .min_h_0()
                        .h_full()
                        .overflow_hidden()
                        .child(render_split_tree_at(second, second_path, true, params, cx));

                    div()
                        .flex_1()
                        .min_w_0()
                        .min_h_0()
                        .overflow_hidden()
                        .size_full()
                        .flex()
                        .flex_row()
                        .child(first_pane)
                        .child(second_pane)
                        .into_any_element()
                }
                SplitAxis::Vertical => {
                    let first_pane = div()
                        .relative()
                        .flex_none()
                        .h(relative(ratio_clamped))
                        .min_w_0()
                        .min_h_0()
                        .w_full()
                        .overflow_hidden()
                        .border_b_1()
                        .border_color(params.theme.border_subtle)
                        .child(render_split_tree_at(first, first_path, true, params, cx))
                        .child(
                            div()
                                .id(SharedString::from(format!("split-drag-v-{path}")))
                                .absolute()
                                .left_0()
                                .right_0()
                                .bottom_0()
                                .h(px(7.0))
                                .cursor(gpui::CursorStyle::ResizeUpDown)
                                .hover(|s| s.bg(params.theme.surface_pill))
                                .on_mouse_down(
                                    MouseButton::Left,
                                    cx.listener(move |this, event: &MouseDownEvent, _window, cx| {
                                        this.start_drag_split(
                                            drag_path.clone(),
                                            SplitAxis::Vertical,
                                            event.position,
                                        );
                                        cx.stop_propagation();
                                    }),
                                ),
                        );

                    let second_pane = div()
                        .flex_1()
                        .min_w_0()
                        .min_h_0()
                        .w_full()
                        .overflow_hidden()
                        .child(render_split_tree_at(second, second_path, true, params, cx));

                    div()
                        .flex_1()
                        .min_w_0()
                        .min_h_0()
                        .overflow_hidden()
                        .size_full()
                        .flex()
                        .flex_col()
                        .child(first_pane)
                        .child(second_pane)
                        .into_any_element()
                }
            }
        }
    }
}

fn render_single_pane(
    pane: &TerminalPaneState,
    is_focused: bool,
    show_header: bool,
    params: &TerminalRenderParams<'_>,
    cx: &mut Context<HoloSshApp>,
) -> impl IntoElement {
    let theme = params.theme;
    let scroll_id = pane.pane_id.clone();

    let mut container = div()
        .id(SharedString::from(format!("term-pane-{}", pane.pane_id)))
        .relative()
        .flex_1()
        .min_w_0()
        .min_h_0()
        .size_full()
        .flex()
        .flex_col()
        .overflow_hidden()
        .bg(if !is_focused && show_header {
            gpui::Rgba {
                r: 0.0,
                g: 0.0,
                b: 0.0,
                a: 0.08,
            }
            .into()
        } else {
            gpui::transparent_black()
        })
        .on_scroll_wheel(cx.listener(move |this, event: &ScrollWheelEvent, _window, cx| {
            if this.scroll_terminal_pane(&scroll_id, event) {
                cx.notify();
            }
        }));

    if show_header {
        let close_id = pane.pane_id.clone();
        let focus_hdr_id = pane.pane_id.clone();
        let mut right_actions = div().flex().items_center().gap(px(8.0));
        if is_focused {
            right_actions = right_actions
                .child(
                    div()
                        .id(SharedString::from(format!("pane-split-h-{}", pane.pane_id)))
                        .w(px(16.0))
                        .h(px(16.0))
                        .flex()
                        .items_center()
                        .justify_center()
                        .cursor_pointer()
                        .on_click(cx.listener(|this, _, _, cx| {
                            cx.stop_propagation();
                            this.split_focused_pane(SplitAxis::Horizontal);
                            cx.notify();
                        }))
                        .child(icon_svg("icons/split-pane.svg", 11.5, theme.text_tertiary)),
                )
                .child(
                    div()
                        .id(SharedString::from(format!("pane-split-v-{}", pane.pane_id)))
                        .w(px(16.0))
                        .h(px(16.0))
                        .flex()
                        .items_center()
                        .justify_center()
                        .cursor_pointer()
                        .on_click(cx.listener(|this, _, _, cx| {
                            cx.stop_propagation();
                            this.split_focused_pane(SplitAxis::Vertical);
                            cx.notify();
                        }))
                        .child(icon_svg(
                            "icons/split-horizontal.svg",
                            11.5,
                            theme.text_tertiary,
                        )),
                );
        }
        right_actions = right_actions.child(
            div()
                .id(SharedString::from(format!("pane-close-{}", pane.pane_id)))
                .w(px(16.0))
                .h(px(16.0))
                .flex()
                .items_center()
                .justify_center()
                .cursor_pointer()
                .on_click(cx.listener(move |this, _, _, cx| {
                    cx.stop_propagation();
                    this.close_pane(&close_id);
                    cx.notify();
                }))
                .child(icon_svg("icons/close.svg", 10.0, theme.text_tertiary)),
        );

        container = container
            .child(
                div()
                    .id(SharedString::from(format!("pane-hdr-{}", pane.pane_id)))
                    .w_full()
                    .h(px(LayoutMetrics::PANE_HEADER_HEIGHT))
                    .px(px(14.0))
                    .flex()
                    .items_center()
                    .justify_between()
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _, window, cx| {
                            window.focus(&this.focus_handle);
                            this.focus_pane(&focus_hdr_id);
                            cx.notify();
                        }),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .items_center()
                            .gap(px(7.0))
                            .child(status_dot(pane.connected, theme))
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .truncate()
                                    .text_size(LayoutMetrics::font_micro())
                                    .text_color(if is_focused {
                                        theme.text_secondary
                                    } else {
                                        theme.text_tertiary
                                    })
                                    .child(pane.title()),
                            ),
                    )
                    .child(right_actions),
            )
            .child(hairline_h(theme));
    }

    let font_size = if show_header {
        (params.base_font_size - 1.0).max(11.0)
    } else {
        params.base_font_size
    };

    let hovered_link_for_pane = params
        .hovered_link
        .as_ref()
        .filter(|(pid, _)| *pid == pane.pane_id.as_str())
        .map(|(_, span)| span.clone());
    let has_hovered_link = hovered_link_for_pane.is_some();

    let search_hl = if let Some(st) = params
        .search_state
        .filter(|s| s.pane_id == pane.pane_id)
    {
        TerminalSearchHighlight {
            matches: Arc::clone(&st.matches),
            active_index: st.active_index,
        }
    } else {
        TerminalSearchHighlight::default()
    };

    let pid_left_down = pane.pane_id.clone();
    let pid_mid_down = pane.pane_id.clone();
    let pid_right_down = pane.pane_id.clone();
    let pid_move = pane.pane_id.clone();
    let pid_left_up = pane.pane_id.clone();
    let pid_mid_up = pane.pane_id.clone();
    let pid_right_up = pane.pane_id.clone();
    let pid_drop = pane.pane_id.clone();

    let mut canvas_wrap = div()
        .id(SharedString::from(format!("term-body-{}", pane.pane_id)))
        .relative()
        .flex_1()
        .min_w_0()
        .min_h_0()
        .overflow_hidden()
        .w_full()
        .px(px(if show_header { 14.0 } else { 18.0 }))
        .py(px(if show_header { 12.0 } else { 14.0 }))
        .cursor(if has_hovered_link {
            gpui::CursorStyle::PointingHand
        } else {
            gpui::CursorStyle::IBeam
        })
        .on_drop::<ExternalPaths>(cx.listener(move |this, paths: &ExternalPaths, window, cx| {
            this.handle_terminal_external_paths_drop(&pid_drop, paths, window, cx);
            cx.notify();
        }))
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                this.handle_terminal_mouse_down(&pid_left_down, MouseButton::Left, event, window, cx);
                cx.stop_propagation();
                cx.notify();
            }),
        )
        .on_mouse_down(
            MouseButton::Middle,
            cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                this.handle_terminal_mouse_down(&pid_mid_down, MouseButton::Middle, event, window, cx);
                cx.stop_propagation();
                cx.notify();
            }),
        )
        .on_mouse_down(
            MouseButton::Right,
            cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                this.handle_terminal_mouse_down(&pid_right_down, MouseButton::Right, event, window, cx);
                cx.stop_propagation();
                cx.notify();
            }),
        )
        .on_mouse_move(cx.listener(move |this, event: &MouseMoveEvent, _, cx| {
            if this.handle_terminal_mouse_move(&pid_move, event) {
                cx.notify();
            }
        }))
        .on_mouse_up(
            MouseButton::Left,
            cx.listener(move |this, event: &MouseUpEvent, _, cx| {
                this.handle_terminal_mouse_up(&pid_left_up, MouseButton::Left, event, cx);
                cx.notify();
            }),
        )
        .on_mouse_up(
            MouseButton::Middle,
            cx.listener(move |this, event: &MouseUpEvent, _, cx| {
                this.handle_terminal_mouse_up(&pid_mid_up, MouseButton::Middle, event, cx);
                cx.notify();
            }),
        )
        .on_mouse_up(
            MouseButton::Right,
            cx.listener(move |this, event: &MouseUpEvent, _, cx| {
                this.handle_terminal_mouse_up(&pid_right_up, MouseButton::Right, event, cx);
                cx.notify();
            }),
        );

    let scroll_offset = params
        .emulators
        .get(&pane.pane_id)
        .map(|e| e.display_offset())
        .unwrap_or(0);

    if let Some(emu) = params.emulators.get(&pane.pane_id) {
        let grid = emu.render_grid(params.is_dark);
        let register_ime = is_focused && params.active_input == ActiveInput::Terminal;
        canvas_wrap = canvas_wrap.child(TerminalElement::new(
            pane.pane_id.clone(),
            grid,
            SharedString::from(params.font_family.to_string()),
            font_size,
            params.is_dark,
            is_focused,
            params.cursor_style,
            *theme,
            search_hl,
            hovered_link_for_pane,
            if is_focused {
                params.ime_marked_text.map(|s| s.to_string())
            } else {
                None
            },
            register_ime,
            params.focus_handle.clone(),
            params.app_entity.clone(),
            params.metrics_store.clone(),
            params.shape_cache.clone(),
        ));
    }

    // 浮层 0：历史翻阅时右下角极简黑白单色「↓ 回到底部」胶囊（距右侧 28px 为 4px 滚动条留出干净间距）
    if scroll_offset > 0 {
        let pid_bottom = pane.pane_id.clone();
        canvas_wrap = canvas_wrap.child(
            div()
                .id(SharedString::from(format!("term-scroll-bottom-{}", pane.pane_id)))
                .absolute()
                .right(px(28.0))
                .bottom(px(14.0))
                .h(px(26.0))
                .px(px(10.0))
                .rounded(LayoutMetrics::radius_pill())
                .bg(theme.sheet_bg)
                .border_1()
                .border_color(theme.border_specular)
                .flex()
                .items_center()
                .gap(px(5.0))
                .cursor_pointer()
                .hover(|s| s.bg(theme.surface_active))
                .active(|s| s.opacity(0.76))
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, _: &MouseDownEvent, _, cx| {
                        cx.stop_propagation();
                        this.scroll_pane_to_bottom(&pid_bottom);
                        cx.notify();
                    }),
                )
                .child(
                    div()
                        .text_size(LayoutMetrics::font_micro())
                        .text_color(theme.text_primary)
                        .child(format!("↓ {}", tr("term.scroll_to_bottom"))),
                ),
        );
    }

    // 浮层 1：终端内搜索条（⌘F，极简黑白磨砂胶囊，右上角悬浮）
    if is_focused {
        if let Some(st) = params
            .search_state
            .filter(|s| s.pane_id == pane.pane_id)
        {
            canvas_wrap = canvas_wrap.child(render_terminal_search_bar(
                st,
                params.active_input == ActiveInput::TerminalSearch,
                theme,
                cx,
            ));
        }

        // 浮层 2：多行粘贴安全确认条（非备用屏粘贴多行文本时触发，防止运维误触回车执行）
        if let Some(pending) = params
            .paste_confirm
            .filter(|p| p.pane_id == pane.pane_id)
        {
            canvas_wrap = canvas_wrap.child(render_paste_confirm_bar(pending, theme, cx));
        }
    }

    container.child(canvas_wrap)
}

fn render_terminal_search_bar(
    st: &TerminalSearchState,
    is_editing: bool,
    theme: &ThemeTokens,
    cx: &mut Context<HoloSshApp>,
) -> impl IntoElement {
    let total = st.matches.len();
    let count_text = if st.query.is_empty() {
        String::new()
    } else if total == 0 {
        tr("term.search_no_match").to_string()
    } else {
        let cur = st.active_index.map(|i| i + 1).unwrap_or(1);
        format!("{cur}/{total}")
    };

    div()
        .id("term-search-overlay")
        .absolute()
        .top(px(10.0))
        .right(px(14.0))
        .h(px(34.0))
        .px(px(8.0))
        .rounded(px(9.0))
        .bg(theme.sheet_bg)
        .border_1()
        .border_color(theme.border_specular)
        .flex()
        .items_center()
        .gap(px(5.0))
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(|_, _: &MouseDownEvent, _, cx| {
                cx.stop_propagation();
            }),
        )
        .child(
            div()
                .id("term-search-input")
                .w(px(176.0))
                .h(px(24.0))
                .px(px(7.0))
                .rounded(px(6.0))
                .bg(theme.surface_input)
                .border_1()
                .border_color(if is_editing {
                    theme.border_specular
                } else {
                    theme.border_subtle
                })
                .flex()
                .items_center()
                .overflow_hidden()
                .cursor_text()
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, ev: &MouseDownEvent, window, cx| {
                        cx.stop_propagation();
                        this.begin_text_input_mouse_down(ActiveInput::TerminalSearch, ev, window);
                        cx.notify();
                    }),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .overflow_hidden()
                        .flex()
                        .items_center()
                        .cursor_text()
                        .child(editable_text_line(
                            ActiveInput::TerminalSearch,
                            &st.query,
                            tr("term.search_placeholder"),
                            false,
                            is_editing,
                            LayoutMetrics::font_control(),
                            13.0,
                            theme.text_primary,
                            theme.text_tertiary,
                            None,
                            theme,
                        )),
                ),
        )
        .when(!count_text.is_empty(), |el| {
            el.child(
                div()
                    .px(px(4.0))
                    .text_size(LayoutMetrics::font_micro())
                    .text_color(if total == 0 {
                        theme.text_tertiary
                    } else {
                        theme.text_secondary
                    })
                    .child(count_text),
            )
        })
        .child(search_toggle_btn(
            "term-search-case",
            "Aa",
            st.case_sensitive,
            theme,
            cx.listener(|this, _, _, cx| {
                this.toggle_terminal_search_case();
                cx.notify();
            }),
        ))
        .child(search_toggle_btn(
            "term-search-regex",
            ".*",
            st.is_regex,
            theme,
            cx.listener(|this, _, _, cx| {
                this.toggle_terminal_search_regex();
                cx.notify();
            }),
        ))
        .child(search_icon_btn(
            "term-search-prev",
            "↑",
            theme,
            cx.listener(|this, _, _, cx| {
                this.step_terminal_search(-1);
                cx.notify();
            }),
        ))
        .child(search_icon_btn(
            "term-search-next",
            "↓",
            theme,
            cx.listener(|this, _, _, cx| {
                this.step_terminal_search(1);
                cx.notify();
            }),
        ))
        .child(
            div()
                .id("term-search-close")
                .w(px(20.0))
                .h(px(20.0))
                .rounded(px(5.0))
                .flex()
                .items_center()
                .justify_center()
                .cursor_pointer()
                .hover(|s| s.bg(theme.surface_pill))
                .on_click(cx.listener(|this, _, _, cx| {
                    this.close_terminal_search();
                    cx.notify();
                }))
                .child(icon_svg("icons/close.svg", 9.5, theme.text_tertiary)),
        )
}

fn search_toggle_btn(
    id: &'static str,
    label: &'static str,
    active: bool,
    theme: &ThemeTokens,
    on_click: impl Fn(&gpui::ClickEvent, &mut gpui::Window, &mut gpui::App) + 'static,
) -> impl IntoElement {
    div()
        .id(id)
        .h(px(22.0))
        .px(px(6.0))
        .rounded(px(5.0))
        .flex()
        .items_center()
        .justify_center()
        .cursor_pointer()
        .bg(if active {
            theme.surface_active
        } else {
            gpui::transparent_black()
        })
        .border_1()
        .border_color(if active {
            theme.border_specular
        } else {
            gpui::transparent_black()
        })
        .hover(|s| s.bg(theme.surface_pill))
        .on_click(on_click)
        .child(
            div()
                .text_size(LayoutMetrics::font_micro())
                .text_color(if active {
                    theme.text_primary
                } else {
                    theme.text_tertiary
                })
                .child(label),
        )
}

fn search_icon_btn(
    id: &'static str,
    glyph: &'static str,
    theme: &ThemeTokens,
    on_click: impl Fn(&gpui::ClickEvent, &mut gpui::Window, &mut gpui::App) + 'static,
) -> impl IntoElement {
    div()
        .id(id)
        .w(px(22.0))
        .h(px(22.0))
        .rounded(px(5.0))
        .flex()
        .items_center()
        .justify_center()
        .cursor_pointer()
        .hover(|s| s.bg(theme.surface_pill))
        .on_click(on_click)
        .child(
            div()
                .text_size(LayoutMetrics::font_caption())
                .text_color(theme.text_secondary)
                .child(glyph),
        )
}

fn render_paste_confirm_bar(
    pending: &PendingPasteConfirm,
    theme: &ThemeTokens,
    cx: &mut Context<HoloSshApp>,
) -> impl IntoElement {
    let title = trf(
        "term.paste_confirm_title",
        &[("lines", &pending.line_count.to_string())],
    );
    div()
        .id("term-paste-confirm")
        .absolute()
        .top(px(52.0))
        .right(px(14.0))
        .max_w(px(360.0))
        .px(px(12.0))
        .py(px(9.0))
        .rounded(px(10.0))
        .bg(theme.sheet_bg)
        .border_1()
        .border_color(theme.border_specular)
        .flex()
        .items_center()
        .gap(px(10.0))
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(|_, _: &MouseDownEvent, _, cx| {
                cx.stop_propagation();
            }),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .gap(px(2.0))
                .child(
                    div()
                        .truncate()
                        .text_size(LayoutMetrics::font_control())
                        .text_color(theme.text_primary)
                        .child(title),
                )
                .child(
                    div()
                        .truncate()
                        .text_size(LayoutMetrics::font_micro())
                        .text_color(theme.text_tertiary)
                        .child(pending.preview.clone()),
                ),
        )
        .child(
            div()
                .id("term-paste-cancel")
                .h(px(26.0))
                .px(px(9.0))
                .rounded(px(6.5))
                .flex()
                .items_center()
                .justify_center()
                .cursor_pointer()
                .hover(|s| s.bg(theme.surface_pill))
                .on_click(cx.listener(|this, _, _, cx| {
                    this.cancel_pending_paste();
                    cx.notify();
                }))
                .child(
                    div()
                        .text_size(LayoutMetrics::font_caption())
                        .text_color(theme.text_secondary)
                        .child(tr("term.paste_confirm_cancel")),
                ),
        )
        .child(
            div()
                .id("term-paste-ok")
                .h(px(26.0))
                .px(px(10.0))
                .rounded(px(6.5))
                .bg(theme.surface_active)
                .border_1()
                .border_color(theme.border_specular)
                .flex()
                .items_center()
                .justify_center()
                .cursor_pointer()
                .hover(|s| s.opacity(0.9))
                .on_click(cx.listener(|this, _, _, cx| {
                    this.confirm_pending_paste();
                    cx.notify();
                }))
                .child(
                    div()
                        .text_size(LayoutMetrics::font_caption())
                        .text_color(theme.text_primary)
                        .child(tr("term.paste_confirm_ok")),
                ),
        )
}

trait WhenExt: Sized {
    fn when(self, cond: bool, f: impl FnOnce(Self) -> Self) -> Self {
        if cond { f(self) } else { self }
    }
}

impl<T> WhenExt for T {}
