//! 右侧未选择主机时的极简初始空状态
//!
//! 居中仅包含单色磨砂「+ 添加主机」主按钮，附带 220ms 柔和淡入浮起动效。

use crate::app::HoloSshApp;
use crate::state::settings::{Language, SidebarSide};
use crate::theme::{LayoutMetrics, ThemeTokens};
use crate::ui::components::{boost_alpha, icon_svg};
use gpui::{
    Animation, AnimationExt, Context, InteractiveElement, IntoElement, ParentElement,
    StatefulInteractiveElement, Styled, div, ease_out_quint, px,
};
use std::time::Duration;

pub fn render_empty_workspace_pane(
    sidebar_collapsed: bool,
    sidebar_side: SidebarSide,
    lang: Language,
    theme: &ThemeTokens,
    cx: &mut Context<HoloSshApp>,
) -> impl IntoElement {
    let hover_bg = boost_alpha(theme.surface_active, 0.08);
    let specular = theme.border_specular;

    let center_stack = div()
        .flex()
        .flex_col()
        .items_center()
        .gap(px(14.0))
        // 第一行：单色磨砂「+ 添加主机」胶囊按钮（带悬停高光与按压微反馈）
        .child(
            div()
                .id("btn-empty-add-host")
                .h(px(36.0))
                .pl(px(18.0))
                .pr(px(20.0))
                .flex()
                .items_center()
                .justify_center()
                .gap(px(7.0))
                .rounded(LayoutMetrics::radius_pill())
                .cursor_pointer()
                .bg(theme.surface_active)
                .border_1()
                .border_color(specular)
                .hover(move |s| s.bg(hover_bg).border_color(specular))
                .active(|s| s.opacity(0.75))
                .on_click(cx.listener(|this, _, window, cx| {
                    window.focus(&this.focus_handle);
                    this.open_new_host_modal();
                    cx.notify();
                }))
                .child(icon_svg("icons/plus.svg", 13.0, theme.text_primary))
                .child(
                    div()
                        .text_size(LayoutMetrics::font_body())
                        .text_color(theme.text_primary)
                        .child(lang.t("workspace.add_host")),
                ),
        )
        .with_animation(
            "empty-state-enter",
            Animation::new(Duration::from_millis(220)).with_easing(ease_out_quint()),
            |el, d| el.opacity(d).mt(px(10.0 * (1.0 - d))),
        );

    let mut pane = div()
        .relative()
        .flex_1()
        .h_full()
        .flex()
        .items_center()
        .justify_center()
        .bg(theme.terminal_bg)
        .child(center_stack);

    // 仅当侧栏被收起时，在对应侧顶部提供裸图标以便重新展开侧栏
    if sidebar_collapsed {
        let mut expand_btn = div()
            .id("btn-empty-sidebar-expand")
            .absolute()
            .top(px(11.0))
            .w(px(26.0))
            .h(px(26.0))
            .rounded(px(6.5))
            .flex()
            .items_center()
            .justify_center()
            .cursor_pointer()
            .hover(|s| s.bg(theme.surface_pill))
            .active(|s| s.opacity(0.70))
            .on_click(cx.listener(|this, _, _, cx| {
                this.toggle_sidebar();
                cx.notify();
            }))
            .child(icon_svg(
                "icons/sidebar-toggle.svg",
                14.0,
                theme.text_secondary,
            ));
        expand_btn = match sidebar_side {
            SidebarSide::Left => expand_btn.left(px(90.0)),
            SidebarSide::Right => expand_btn.right(px(14.0)),
        };
        pane = pane.child(expand_btn);
    }

    pane
}
