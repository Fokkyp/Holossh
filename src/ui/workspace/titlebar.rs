//! 右侧统一顶部标题栏（对应 Figma 01 / 03 / 04 / 05 画板）

use crate::app::HoloSshApp;
use crate::state::session::SessionTab;
use crate::state::settings::SidebarSide;
use crate::theme::{LayoutMetrics, ThemeTokens};
use crate::ui::components::{icon_svg, status_dot};
use gpui::{
    Context, InteractiveElement, IntoElement, ParentElement, StatefulInteractiveElement, Styled,
    div, px,
};

#[allow(clippy::too_many_arguments)]
pub fn render_titlebar(
    tabs: &[SessionTab],
    active_tab_id: &str,
    sidebar_collapsed: bool,
    sidebar_side: SidebarSide,
    split_active: bool,
    sftp_open: bool,
    sftp_disabled: bool,
    theme: &ThemeTokens,
    cx: &mut Context<HoloSshApp>,
) -> impl IntoElement {
    let mut tab_strip = div()
        .flex_1()
        .min_w_0()
        .overflow_hidden()
        .flex()
        .items_center()
        .gap(px(6.0));

    // 当左侧无侧栏（侧栏已收起或侧栏置于右侧）时，标题栏左侧为 macOS 红黄绿预留空间
    let needs_traffic_light_inset = sidebar_collapsed || sidebar_side == SidebarSide::Right;
    if sidebar_collapsed && sidebar_side == SidebarSide::Left {
        tab_strip = tab_strip.child(
            div()
                .id("btn-sidebar-expand")
                .w(px(26.0))
                .h(px(26.0))
                .rounded(px(6.5))
                .flex()
                .items_center()
                .justify_center()
                .cursor_pointer()
                .hover(|s| s.bg(theme.surface_pill))
                .active(|s| s.opacity(0.60))
                .on_click(cx.listener(|this, _, _, cx| {
                    this.toggle_sidebar();
                    cx.notify();
                }))
                .child(icon_svg(
                    "icons/sidebar-toggle.svg",
                    14.0,
                    theme.text_secondary,
                )),
        );
    }

    for (idx, tab) in tabs.iter().enumerate() {
        let is_active = tab.id == active_tab_id;
        let select_id = tab.id.clone();
        let dup_id = tab.id.clone();
        let close_id = tab.id.clone();

        let mut pill = div()
            .id(("tab-pill", idx))
            .h(px(LayoutMetrics::TAB_HEIGHT))
            .px(px(11.0))
            .flex()
            .items_center()
            .gap(px(7.0))
            .rounded(LayoutMetrics::radius_pill())
            .cursor_pointer()
            .bg(if is_active {
                theme.surface_active
            } else {
                gpui::transparent_black()
            })
            .border_1()
            .border_color(if is_active {
                theme.border_specular
            } else {
                gpui::transparent_black()
            })
            .hover(|s| {
                s.bg(if is_active {
                    theme.surface_active
                } else {
                    theme.surface_pill
                })
            })
            .active(|s| s.opacity(0.78))
            .on_click(cx.listener(move |this, _, _, cx| {
                this.select_tab(&select_id);
                cx.notify();
            }))
            .child(status_dot(tab.connected, theme))
            .child(
                div()
                    .text_size(LayoutMetrics::font_control())
                    .text_color(if is_active {
                        theme.text_primary
                    } else {
                        theme.text_secondary
                    })
                    .child(tab.title.clone()),
            );

        // 非当前活跃标签收到终端响铃（\x07）时，展示克制的单色圆点提醒
        if !is_active && tab.has_bell {
            pill = pill.child(
                div()
                    .w(px(5.0))
                    .h(px(5.0))
                    .rounded_full()
                    .bg(theme.text_primary),
            );
        }

        // 仅在当前选中标签内展示「复制会话」图标，明确所复制的目标 SSH 主机
        if is_active {
            pill = pill.child(
                div()
                    .id(("tab-duplicate", idx))
                    .w(px(16.0))
                    .h(px(16.0))
                    .rounded(px(4.0))
                    .flex()
                    .items_center()
                    .justify_center()
                    .cursor_pointer()
                    .hover(|s| s.bg(theme.surface_pill))
                    .active(|s| s.opacity(0.65))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        cx.stop_propagation();
                        this.duplicate_tab(&dup_id);
                        cx.notify();
                    }))
                    .child(icon_svg("icons/copy.svg", 11.5, theme.text_secondary)),
            );
        }

        pill = pill.child(
            div()
                .id(("tab-close", idx))
                .w(px(14.0))
                .h(px(14.0))
                .rounded(px(4.0))
                .flex()
                .items_center()
                .justify_center()
                .cursor_pointer()
                .hover(|s| s.bg(theme.surface_pill))
                .active(|s| s.opacity(0.65))
                .on_click(cx.listener(move |this, _, _, cx| {
                    cx.stop_propagation();
                    this.close_tab(&close_id);
                    cx.notify();
                }))
                .child(icon_svg("icons/close.svg", 9.5, theme.text_tertiary)),
        );
        tab_strip = tab_strip.child(pill);
    }

    // 右上角 73x28 全高零空隙分段工具胶囊（分屏 | SFTP 文件，锁定 flex_none 永不被挤压或顶出）
    let capsule = div()
        .flex_none()
        .w(px(LayoutMetrics::TOOLBAR_CAPSULE_WIDTH))
        .h(px(LayoutMetrics::TOOLBAR_CAPSULE_HEIGHT))
        .flex()
        .items_center()
        .overflow_hidden()
        .rounded(LayoutMetrics::radius_pill())
        .bg(theme.surface_pill)
        .border_1()
        .border_color(theme.border_specular)
        // 左半：分屏（flex_1 全高零空隙贴合胶囊左侧圆角，避免直角溢出）
        .child(
            div()
                .id("btn-toggle-split")
                .flex_1()
                .h_full()
                .rounded_tl(px(13.0))
                .rounded_bl(px(13.0))
                .flex()
                .items_center()
                .justify_center()
                .cursor_pointer()
                .bg(if split_active {
                    theme.surface_active
                } else {
                    gpui::transparent_black()
                })
                .hover(|s| s.bg(theme.surface_active))
                .active(|s| s.opacity(0.74))
                .on_click(cx.listener(|this, _, _, cx| {
                    this.toggle_split_view();
                    cx.notify();
                }))
                .child(icon_svg("icons/split-pane.svg", 13.5, theme.text_primary)),
        )
        // 1px 全高分割中线
        .child(div().flex_none().w(px(1.0)).h_full().bg(theme.border_specular))
        // 右半：SFTP 文件（flex_1 全高零空隙贴合胶囊右侧圆角；本地终端下置灰禁用）
        .child(if sftp_disabled {
            div()
                .id("btn-toggle-sftp")
                .flex_1()
                .h_full()
                .rounded_tr(px(13.0))
                .rounded_br(px(13.0))
                .flex()
                .items_center()
                .justify_center()
                .opacity(0.32)
                .child(icon_svg("icons/folder.svg", 13.5, theme.text_tertiary))
        } else {
            div()
                .id("btn-toggle-sftp")
                .flex_1()
                .h_full()
                .rounded_tr(px(13.0))
                .rounded_br(px(13.0))
                .flex()
                .items_center()
                .justify_center()
                .cursor_pointer()
                .bg(if sftp_open {
                    theme.surface_active
                } else {
                    gpui::transparent_black()
                })
                .hover(|s| s.bg(theme.surface_active))
                .active(|s| s.opacity(0.74))
                .on_click(cx.listener(|this, _, _, cx| {
                    this.toggle_sftp_panel();
                    cx.notify();
                }))
                .child(icon_svg("icons/folder.svg", 13.5, theme.text_primary))
        });

    let mut right_group = div().flex_none().flex().items_center().gap(px(8.0)).child(capsule);
    if sidebar_collapsed && sidebar_side == SidebarSide::Right {
        right_group = right_group.child(
            div()
                .id("btn-sidebar-expand-right")
                .w(px(26.0))
                .h(px(26.0))
                .rounded(px(6.5))
                .flex()
                .items_center()
                .justify_center()
                .cursor_pointer()
                .hover(|s| s.bg(theme.surface_pill))
                .active(|s| s.opacity(0.60))
                .on_click(cx.listener(|this, _, _, cx| {
                    this.toggle_sidebar();
                    cx.notify();
                }))
                .child(icon_svg(
                    "icons/sidebar-toggle.svg",
                    14.0,
                    theme.text_secondary,
                )),
        );
    }

    div()
        .w_full()
        .h(px(LayoutMetrics::TITLEBAR_HEIGHT))
        .pl(px(if needs_traffic_light_inset { 90.0 } else { 14.0 }))
        .pr(px(14.0))
        .flex()
        .items_center()
        .justify_between()
        .gap(px(8.0))
        .child(tab_strip)
        .child(right_group)
}
