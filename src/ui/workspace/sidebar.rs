//! 左侧通顶磨砂侧边栏（对应 Figma 01 / 03 / 08 画板）
//!
//! 顶部 48px 保留展开收起与亮/暗色切换；「主机」标题行右侧并排展示「新建文件夹（分组）」与「+（新建主机）」裸图标。

use crate::app::{
    ActiveInput, CloudSyncVisualState, HoloSshApp, HostDragState, HostDropTarget, HostDropZone,
};
use crate::i18n::tr;
use crate::state::host::{HostProfile, HostStatus};
use crate::state::settings::{Language, SidebarSide};
use crate::theme::{LayoutMetrics, ThemeTokens};
use crate::ui::components::{editable_text_line, icon_svg, status_dot};
use gpui::{
    Animation, AnimationExt, Bounds, Context, InteractiveElement, IntoElement, MouseButton,
    MouseDownEvent, ParentElement, Pixels, SharedString, StatefulInteractiveElement, Styled,
    canvas, div, ease_out_quint, px,
};
use std::cell::RefCell;
use std::collections::{BTreeMap, HashSet};
use std::rc::Rc;
use std::time::Duration;

#[allow(clippy::too_many_arguments)]
pub fn render_sidebar(
    hosts: &[HostProfile],
    custom_groups: &[String],
    group_updated_at: &BTreeMap<String, u64>,
    active_host_id: &str,
    expanded_groups: &HashSet<String>,
    search_query: &str,
    search_shortcut_label: &str,
    search_focused: bool,
    search_expanded: bool,
    search_anim_gen: usize,
    creating_group: bool,
    new_group_name: &str,
    new_group_focused: bool,
    renaming_group: Option<&str>,
    rename_group_input: &str,
    rename_group_focused: bool,
    is_dark: bool,
    compact: bool,
    show_groups: bool,
    side: SidebarSide,
    width_px: f32,
    cloud_sync_state: CloudSyncVisualState,
    active_host_drag: Option<&HostDragState>,
    host_drop_zones: Rc<RefCell<Vec<(HostDropZone, Bounds<Pixels>)>>>,
    lang: Language,
    theme: &ThemeTokens,
    cx: &mut Context<HoloSshApp>,
) -> impl IntoElement {
    let row_height = if compact {
        30.0
    } else {
        LayoutMetrics::HOST_ROW_HEIGHT
    };
    let needle = search_query.trim().to_lowercase();

    let matches_search = |host: &HostProfile| -> bool {
        if needle.is_empty() {
            return true;
        }
        host.name.to_lowercase().contains(&needle)
            || host.hostname.to_lowercase().contains(&needle)
            || host.sidebar_subtitle().to_lowercase().contains(&needle)
            || host.group.to_lowercase().contains(&needle)
    };

    let mut host_tree = div()
        .id("sidebar-host-tree")
        .flex_1()
        .w_full()
        .overflow_y_scroll()
        .flex()
        .flex_col()
        .gap(px(2.0));

    // 点击「新建文件夹」图标后在主机树顶部展示内联分组名称输入框（回车确认 / Esc 取消）
    if creating_group {
        let input_text_wrap = div()
            .flex_1()
            .min_w_0()
            .overflow_hidden()
            .flex()
            .items_center()
            .child(editable_text_line(
                ActiveInput::SidebarNewGroup,
                new_group_name,
                tr("sidebar.new_group_placeholder"),
                false,
                new_group_focused,
                LayoutMetrics::font_control(),
                13.0,
                theme.sidebar_text_primary,
                theme.sidebar_text_tertiary,
                None,
                theme,
            ));

        let input_left = div()
            .flex_1()
            .min_w_0()
            .overflow_hidden()
            .flex()
            .items_center()
            .gap(px(6.0))
            .cursor_text()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, ev: &MouseDownEvent, window, cx| {
                    cx.stop_propagation();
                    this.begin_text_input_mouse_down(ActiveInput::SidebarNewGroup, ev, window);
                    cx.notify();
                }),
            )
            .child(icon_svg("icons/folder.svg", 13.0, theme.sidebar_text_secondary))
            .child(input_text_wrap);

        host_tree = host_tree.child(
            div()
                .id("sidebar-new-group-row")
                .w_full()
                .h(px(30.0))
                .px(px(8.0))
                .mb(px(2.0))
                .rounded(LayoutMetrics::radius_row())
                .bg(theme.surface_input)
                .border_1()
                .border_color(theme.border_specular)
                .flex()
                .items_center()
                .justify_between()
                .gap(px(6.0))
                .on_click(cx.listener(|this, _, window, cx| {
                    window.focus(&this.focus_handle);
                    this.set_active_input(ActiveInput::SidebarNewGroup);
                    cx.notify();
                }))
                .child(input_left)
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(4.0))
                        .child(
                            div()
                                .id("btn-confirm-new-group")
                                .px(px(4.0))
                                .rounded(px(4.0))
                                .text_size(LayoutMetrics::font_micro())
                                .text_color(theme.sidebar_text_primary)
                                .cursor_pointer()
                                .hover(|s| s.bg(theme.surface_active))
                                .active(|s| s.opacity(0.70))
                                .on_click(cx.listener(|this, _, _, cx| {
                                    cx.stop_propagation();
                                    this.commit_creating_group();
                                    cx.notify();
                                }))
                                .child("✓"),
                        )
                        .child(
                            div()
                                .id("btn-cancel-new-group")
                                .w(px(16.0))
                                .h(px(16.0))
                                .rounded(px(4.0))
                                .flex()
                                .items_center()
                                .justify_center()
                                .cursor_pointer()
                                .hover(|s| s.bg(theme.surface_active))
                                .active(|s| s.opacity(0.70))
                                .on_click(cx.listener(|this, _, _, cx| {
                                    cx.stop_propagation();
                                    this.cancel_creating_group();
                                    cx.notify();
                                }))
                                .child(icon_svg("icons/close.svg", 9.5, theme.sidebar_text_tertiary)),
                        ),
                )
                .with_animation(
                    "new-group-slide-in",
                    Animation::new(Duration::from_millis(165)).with_easing(ease_out_quint()),
                    |el, d| el.opacity(d).mt(px(-4.0 * (1.0 - d))),
                ),
        );
    }

    let mut rendered_count = 0usize;
    let hover_drop_target = active_host_drag.and_then(|d| d.hover_target.as_ref());

    if show_groups {
        let mut row_seq = 0usize;

        // 无分组（无文件夹）主机优先置顶展示在「主机」最上方，默认按添加时间倒序排列
        let mut ungrouped_hosts: Vec<&HostProfile> = hosts
            .iter()
            .filter(|h| h.group.trim().is_empty() && matches_search(h))
            .collect();
        ungrouped_hosts.sort_by(|a, b| {
            b.effective_created_at_ms()
                .cmp(&a.effective_created_at_ms())
        });
        for (idx_in_grp, host) in ungrouped_hosts.into_iter().enumerate() {
            host_tree = host_tree.child(render_host_row(
                row_seq,
                host,
                "",
                idx_in_grp,
                host.id == active_host_id,
                active_host_drag,
                &host_drop_zones,
                row_height,
                theme,
                cx,
            ));
            row_seq += 1;
            rendered_count += 1;
        }

        // 收集所有非空文件夹，并按文件夹创建/更新时间倒序（最新文件夹靠上）
        let mut group_order: Vec<String> = Vec::new();
        for host in hosts {
            let trimmed = host.group.trim();
            if !trimmed.is_empty() && !group_order.iter().any(|g| g == trimmed) {
                group_order.push(trimmed.to_string());
            }
        }
        for custom in custom_groups {
            let trimmed = custom.trim();
            if !trimmed.is_empty() && !group_order.iter().any(|g| g == trimmed) {
                group_order.push(trimmed.to_string());
            }
        }
        group_order.sort_by(|a, b| {
            let ts_a = group_updated_at.get(a).copied().unwrap_or_else(|| {
                hosts
                    .iter()
                    .filter(|h| h.group.trim() == a)
                    .map(|h| h.effective_created_at_ms())
                    .max()
                    .unwrap_or(0)
            });
            let ts_b = group_updated_at.get(b).copied().unwrap_or_else(|| {
                hosts
                    .iter()
                    .filter(|h| h.group.trim() == b)
                    .map(|h| h.effective_created_at_ms())
                    .max()
                    .unwrap_or(0)
            });
            ts_b.cmp(&ts_a)
        });

        for (g_idx, group_name) in group_order.into_iter().enumerate() {
            let mut group_hosts: Vec<&HostProfile> = hosts
                .iter()
                .filter(|h| h.group.trim() == group_name && matches_search(h))
                .collect();
            group_hosts.sort_by(|a, b| {
                b.effective_created_at_ms()
                    .cmp(&a.effective_created_at_ms())
            });

            if !needle.is_empty()
                && group_hosts.is_empty()
                && !group_name.to_lowercase().contains(&needle)
            {
                continue;
            }
            rendered_count += 1;

            let is_expanded = !needle.is_empty() || expanded_groups.contains(&group_name);
            let is_renaming = renaming_group == Some(group_name.as_str());

            if is_renaming {
                let rename_text_wrap = div()
                    .flex_1()
                    .min_w_0()
                    .overflow_hidden()
                    .flex()
                    .items_center()
                    .child(editable_text_line(
                        ActiveInput::SidebarRenameGroup,
                        rename_group_input,
                        "",
                        false,
                        rename_group_focused,
                        LayoutMetrics::font_control(),
                        13.0,
                        theme.sidebar_text_primary,
                        theme.sidebar_text_tertiary,
                        None,
                        theme,
                    ));

                let rename_left = div()
                    .flex_1()
                    .min_w_0()
                    .overflow_hidden()
                    .flex()
                    .items_center()
                    .gap(px(6.0))
                    .cursor_text()
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, ev: &MouseDownEvent, window, cx| {
                            cx.stop_propagation();
                            this.begin_text_input_mouse_down(
                                ActiveInput::SidebarRenameGroup,
                                ev,
                                window,
                            );
                            cx.notify();
                        }),
                    )
                    .child(icon_svg("icons/folder.svg", 13.0, theme.sidebar_text_secondary))
                    .child(rename_text_wrap);

                host_tree = host_tree.child(
                    div()
                        .id(("group-rename-row", g_idx))
                        .w_full()
                        .h(px(30.0))
                        .px(px(8.0))
                        .rounded(LayoutMetrics::radius_row())
                        .bg(theme.surface_input)
                        .border_1()
                        .border_color(theme.border_specular)
                        .flex()
                        .items_center()
                        .justify_between()
                        .gap(px(6.0))
                        .child(rename_left)
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap(px(4.0))
                                .child(
                                    div()
                                        .id("btn-confirm-rename-group")
                                        .px(px(4.0))
                                        .rounded(px(4.0))
                                        .text_size(LayoutMetrics::font_micro())
                                        .text_color(theme.sidebar_text_primary)
                                        .cursor_pointer()
                                        .hover(|s| s.bg(theme.surface_active))
                                        .active(|s| s.opacity(0.70))
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            cx.stop_propagation();
                                            this.commit_renaming_group();
                                            cx.notify();
                                        }))
                                        .child("✓"),
                                )
                                .child(
                                    div()
                                        .id("btn-cancel-rename-group")
                                        .w(px(16.0))
                                        .h(px(16.0))
                                        .rounded(px(4.0))
                                        .flex()
                                        .items_center()
                                        .justify_center()
                                        .cursor_pointer()
                                        .hover(|s| s.bg(theme.surface_active))
                                        .active(|s| s.opacity(0.70))
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            cx.stop_propagation();
                                            this.cancel_renaming_group();
                                            cx.notify();
                                        }))
                                        .child(icon_svg("icons/close.svg", 9.5, theme.sidebar_text_tertiary)),
                                ),
                        ),
                );
            } else {
                let group_for_click = group_name.clone();
                let group_for_ctx = group_name.clone();
                let is_folder_drop_hover =
                    hover_drop_target == Some(&HostDropTarget::Folder(group_name.clone()));

                let mut group_row_el = div()
                    .relative()
                    .id(("group-row", g_idx))
                    .w_full()
                    .h(px(30.0))
                    .px(px(6.0))
                    .rounded(LayoutMetrics::radius_row())
                    .flex()
                    .items_center()
                    .justify_between()
                    .cursor_pointer()
                    .bg(if is_folder_drop_hover {
                        theme.surface_active
                    } else {
                        gpui::transparent_black()
                    })
                    .border_1()
                    .border_color(if is_folder_drop_hover {
                        theme.border_specular
                    } else {
                        gpui::transparent_black()
                    })
                    .hover(|s| s.bg(theme.surface_pill))
                    .active(|s| s.opacity(0.78))
                    .on_mouse_down(
                        MouseButton::Right,
                        cx.listener(move |this, event: &MouseDownEvent, _window, cx| {
                            this.open_group_context_menu(&group_for_ctx, event.position);
                            cx.notify();
                        }),
                    )
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.close_group_context_menu();
                        this.toggle_group_expanded(&group_for_click);
                        cx.notify();
                    }))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(6.0))
                            .child(icon_svg(
                                if is_expanded {
                                    "icons/chevron-down.svg"
                                } else {
                                    "icons/chevron-right.svg"
                                },
                                11.5,
                                theme.sidebar_text_tertiary,
                            ))
                            .child(icon_svg("icons/folder.svg", 13.0, theme.sidebar_text_tertiary))
                            .child(
                                div()
                                    .text_size(LayoutMetrics::font_body())
                                    .text_color(if is_expanded || is_folder_drop_hover {
                                        theme.sidebar_text_primary
                                    } else {
                                        theme.sidebar_text_secondary
                                    })
                                    .child(group_name.clone()),
                            ),
                    )
                    .child(
                        div()
                            .pr(px(2.0))
                            .text_size(LayoutMetrics::font_micro())
                            .text_color(theme.sidebar_text_tertiary)
                            .child(group_hosts.len().to_string()),
                    );

                if active_host_drag.is_some() {
                    let drop_zones = host_drop_zones.clone();
                    let target_grp = group_name.clone();
                    group_row_el = group_row_el.child(
                        canvas(
                            move |_bounds, _window, _cx| {},
                            move |bounds: Bounds<Pixels>, _, _window, _cx| {
                                drop_zones.borrow_mut().push((
                                    HostDropZone::FolderHeader {
                                        group: target_grp.clone(),
                                    },
                                    bounds,
                                ));
                            },
                        )
                        .absolute()
                        .inset_0(),
                    );
                }

                host_tree = host_tree.child(group_row_el);
            }

            if is_expanded {
                let mut children_col = div()
                    .relative()
                    .w_full()
                    .pl(px(12.0))
                    .pb(px(4.0))
                    .flex()
                    .flex_col()
                    .gap(px(2.0));
                for (idx_in_grp, host) in group_hosts.into_iter().enumerate() {
                    children_col = children_col.child(render_host_row(
                        row_seq,
                        host,
                        &group_name,
                        idx_in_grp,
                        host.id == active_host_id,
                        active_host_drag,
                        &host_drop_zones,
                        row_height,
                        theme,
                        cx,
                    ));
                    row_seq += 1;
                }
                host_tree = host_tree.child(children_col.with_animation(
                    SharedString::from(format!("grp-expand-{g_idx}-{group_name}")),
                    Animation::new(Duration::from_millis(165)).with_easing(ease_out_quint()),
                    |el, d| el.opacity(d).mt(px(-5.0 * (1.0 - d))),
                ));
            }
        }
    } else {
        let mut flat_hosts: Vec<&HostProfile> =
            hosts.iter().filter(|h| matches_search(h)).collect();
        flat_hosts.sort_by(|a, b| {
            b.effective_created_at_ms()
                .cmp(&a.effective_created_at_ms())
        });
        for (idx, host) in flat_hosts.into_iter().enumerate() {
            host_tree = host_tree.child(render_host_row(
                idx,
                host,
                host.group.trim(),
                idx,
                host.id == active_host_id,
                active_host_drag,
                &host_drop_zones,
                row_height,
                theme,
                cx,
            ));
            rendered_count += 1;
        }
    }

    if !creating_group && rendered_count == 0 && !needle.is_empty() {
        host_tree = host_tree.child(
            div()
                .w_full()
                .px(px(8.0))
                .py(px(14.0))
                .text_size(LayoutMetrics::font_caption())
                .text_color(theme.sidebar_text_tertiary)
                .child(tr("sidebar.no_match")),
        );
    }

    let search_text_wrap = div()
        .flex_1()
        .min_w_0()
        .overflow_hidden()
        .flex()
        .items_center()
        .child(editable_text_line(
            ActiveInput::SidebarSearch,
            search_query,
            lang.t("sidebar.search_hosts"),
            false,
            search_focused,
            LayoutMetrics::font_caption(),
            12.5,
            theme.sidebar_text_primary,
            theme.sidebar_text_tertiary,
            None,
            theme,
        ));

    let search_left = div()
        .flex_1()
        .min_w_0()
        .overflow_hidden()
        .flex()
        .items_center()
        .gap(px(6.0))
        .child(
            div()
                .flex_none()
                .flex()
                .items_center()
                .child(icon_svg("icons/search.svg", 12.5, theme.sidebar_text_tertiary)),
        )
        .child(search_text_wrap);

    let search_box = div()
        .id("sidebar-search-box")
        .flex_1()
        .min_w_0()
        .h(px(28.0))
        .px(px(9.5))
        .flex()
        .items_center()
        .justify_between()
        .gap(px(4.0))
        .rounded(LayoutMetrics::radius_pill())
        .cursor_text()
        .bg(if search_focused {
            theme.surface_active
        } else {
            theme.surface_input
        })
        .border_1()
        .border_color(if search_focused {
            theme.border_specular
        } else {
            theme.border_subtle
        })
        .hover(|s| s.border_color(theme.border_specular))
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(|this, ev: &MouseDownEvent, window, cx| {
                cx.stop_propagation();
                this.begin_text_input_mouse_down(ActiveInput::SidebarSearch, ev, window);
                cx.notify();
            }),
        )
        .on_hover(cx.listener(|this, hovered: &bool, _, cx| {
            if !*hovered && this.collapse_sidebar_search_visual() {
                cx.notify();
            }
        }))
        .child(search_left)
        .child(if !search_query.is_empty() {
            div()
                .id("btn-clear-search")
                .flex_none()
                .w(px(16.0))
                .h(px(16.0))
                .rounded(px(4.0))
                .flex()
                .items_center()
                .justify_center()
                .cursor_pointer()
                .hover(|s| s.bg(theme.surface_pill))
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, _: &MouseDownEvent, _, cx| {
                        cx.stop_propagation();
                        this.search_query.clear();
                        cx.notify();
                    }),
                )
                .child(icon_svg("icons/close.svg", 9.0, theme.sidebar_text_tertiary))
        } else {
            div()
                .id("sidebar-search-kbd")
                .flex_none()
                .text_size(LayoutMetrics::font_micro())
                .text_color(theme.sidebar_text_tertiary)
                .child(search_shortcut_label.to_string())
        });

    let add_actions_inner = div()
        .flex_none()
        .w(px(60.0))
        .flex()
        .items_center()
        .justify_end()
        .gap(px(4.0))
        .child(
            div()
                .id("btn-add-group-folder")
                .flex_none()
                .w(px(28.0))
                .h(px(28.0))
                .rounded(px(7.0))
                .flex()
                .items_center()
                .justify_center()
                .cursor_pointer()
                .hover(|s| s.bg(theme.surface_pill))
                .active(|s| s.opacity(0.70))
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, _: &MouseDownEvent, window, cx| {
                        cx.stop_propagation();
                        window.focus(&this.focus_handle);
                        this.start_creating_group();
                        cx.notify();
                    }),
                )
                .child(icon_svg(
                    "icons/folder-plus.svg",
                    16.5,
                    theme.sidebar_text_secondary,
                )),
        )
        .child(
            div()
                .id("btn-add-host")
                .flex_none()
                .w(px(28.0))
                .h(px(28.0))
                .rounded(px(7.0))
                .flex()
                .items_center()
                .justify_center()
                .cursor_pointer()
                .hover(|s| s.bg(theme.surface_pill))
                .active(|s| s.opacity(0.70))
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, _: &MouseDownEvent, window, cx| {
                        cx.stop_propagation();
                        window.focus(&this.focus_handle);
                        this.open_new_host_modal();
                        cx.notify();
                    }),
                )
                .child(icon_svg("icons/plus.svg", 15.5, theme.sidebar_text_secondary)),
        );

    // 搜索框展开时平滑收起右侧添加按钮（60px→0px + 淡出），移出或失焦时平滑回弹展示
    let actions_animated: gpui::AnyElement = if search_anim_gen == 0 {
        if search_expanded {
            div().flex_none().w(px(0.0)).overflow_hidden().into_any_element()
        } else {
            div()
                .flex_none()
                .w(px(60.0))
                .ml(px(6.0))
                .overflow_hidden()
                .flex()
                .items_center()
                .child(add_actions_inner)
                .into_any_element()
        }
    } else {
        let expanding = search_expanded;
        let dur = if expanding { 200 } else { 215 };
        div()
            .flex_none()
            .overflow_hidden()
            .flex()
            .items_center()
            .child(add_actions_inner)
            .with_animation(
                SharedString::from(format!(
                    "sidebar-search-act-{search_anim_gen}-{}",
                    if expanding { "hide" } else { "show" }
                )),
                Animation::new(Duration::from_millis(dur)).with_easing(ease_out_quint()),
                move |el, d| {
                    let ratio = if expanding { 1.0 - d } else { d };
                    el.w(px(60.0 * ratio))
                        .ml(px(6.0 * ratio))
                        .opacity(ratio)
                },
            )
            .into_any_element()
    };

    let body = div()
        .flex_1()
        .w_full()
        .min_h_0()
        .px(px(12.0))
        .flex()
        .flex_col()
        .gap(px(10.0))
        // 紧凑胶囊搜索框 + 右侧「新建文件夹」「+ 新建主机」（聚焦平滑拉长、移出/点别处平滑恢复）
        .child(
            div()
                .w_full()
                .h(px(28.0))
                .flex()
                .items_center()
                .child(search_box)
                .child(actions_animated),
        )
        // 「主机」分组标题行（拖拽时兼作顶部置顶/移出文件夹热区，零多余文案）
        .child({
            let mut header = div()
                .relative()
                .w_full()
                .pl(px(6.0))
                .pr(px(2.0))
                .py(px(2.0))
                .flex()
                .items_center()
                .child(
                    div()
                        .text_size(LayoutMetrics::font_caption())
                        .text_color(theme.sidebar_text_tertiary)
                        .child(lang.t("sidebar.hosts")),
                );
            if active_host_drag.is_some() {
                let top_zones = host_drop_zones.clone();
                header = header.child(
                    canvas(
                        move |_bounds, _window, _cx| {},
                        move |bounds: Bounds<Pixels>, _, _window, _cx| {
                            top_zones
                                .borrow_mut()
                                .push((HostDropZone::TopHeader, bounds));
                        },
                    )
                    .absolute()
                    .inset_0(),
                );
            }
            header
        })
        .child(host_tree);

    let mut root = div()
        .relative()
        .flex_none()
        .w(px(width_px))
        .h_full()
        .overflow_hidden()
        .flex()
        .flex_col()
        .bg(theme.sidebar_bg)
        .border_color(theme.border_subtle);
    root = match side {
        SidebarSide::Left => root.border_r_1(),
        SidebarSide::Right => root.border_l_1(),
    };

    // 可左右拖拽缩放的隐形 7px 中线手柄（悬停时浮现柔和提示线与原生左右缩放指针）
    let mut resize_handle = div()
        .id("sidebar-resize-handle")
        .absolute()
        .top_0()
        .bottom_0()
        .w(px(7.0))
        .cursor(gpui::CursorStyle::ResizeLeftRight)
        .hover(|s| s.bg(theme.surface_pill))
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(|this, event: &MouseDownEvent, _window, cx| {
                this.start_drag_sidebar(event.position);
                cx.stop_propagation();
            }),
        );
    resize_handle = match side {
        SidebarSide::Left => resize_handle.right_0(),
        SidebarSide::Right => resize_handle.left_0(),
    };

    root
        // 1. 顶部 48px（侧栏居左时预留红黄绿位置，右侧裸图标：亮/暗切换、展开收起）
        .child(
            div()
                .w_full()
                .h(px(LayoutMetrics::TITLEBAR_HEIGHT))
                .pl(px(if side == SidebarSide::Left { 90.0 } else { 12.0 }))
                .pr(px(12.0))
                .flex()
                .items_center()
                .justify_end()
                .gap(px(4.0))
                .child(
                    div()
                        .id("btn-theme-toggle")
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
                            this.toggle_appearance();
                            cx.notify();
                        }))
                        .child(icon_svg(
                            if is_dark {
                                "icons/sun.svg"
                            } else {
                                "icons/moon.svg"
                            },
                            13.5,
                            theme.sidebar_text_secondary,
                        )),
                )
                .child(
                    div()
                        .id("btn-sidebar-toggle")
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
                            theme.sidebar_text_secondary,
                        )),
                ),
        )
        // 2. 主体内容
        .child(body)
        // 3. 底部左下角无外框标准六齿齿轮 `⚙ 设置` + 最右侧云同步状态裸图标
        .child(
            div()
                .w_full()
                .px(px(14.0))
                .py(px(12.0))
                .flex()
                .items_center()
                .justify_between()
                .child(
                    div()
                        .id("btn-open-settings")
                        .px(px(6.0))
                        .py(px(4.0))
                        .rounded(px(7.0))
                        .flex()
                        .items_center()
                        .gap(px(7.0))
                        .cursor_pointer()
                        .hover(|s| s.bg(theme.surface_pill))
                        .active(|s| s.opacity(0.72))
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.open_settings();
                            cx.notify();
                        }))
                        .child(icon_svg("icons/gear.svg", 14.0, theme.sidebar_text_secondary))
                        .child(
                            div()
                                .text_size(LayoutMetrics::font_control())
                                .text_color(theme.sidebar_text_secondary)
                                .child(lang.t("settings.title")),
                        ),
                )
                .child(render_sidebar_cloud_sync_icon(cloud_sync_state, theme, cx)),
        )
        .child(resize_handle)
}

/// 侧栏底部最右侧同步状态云图标：
/// - 未开启同步：半灰云；已开启/同步成功：正常白色云
/// - 上传同步中：云内向上循环流动箭头；下载中：云内向下循环流动箭头
/// - 同步失败：云内叉号；点击直达设置 → 同步页
fn render_sidebar_cloud_sync_icon(
    state: CloudSyncVisualState,
    theme: &ThemeTokens,
    cx: &mut Context<HoloSshApp>,
) -> impl IntoElement {
    // 与左下角齿轮 `gear.svg` 严格保持相同尺寸（14.0px）与相同色值（sidebar_text_secondary，深浅双模式统一）
    let icon_size = 14.0;
    let color = theme.sidebar_text_secondary;
    let glyph = match state {
        CloudSyncVisualState::Disabled => div()
            .flex()
            .items_center()
            .justify_center()
            .opacity(0.40)
            .child(icon_svg("icons/cloud.svg", icon_size, color)),
        CloudSyncVisualState::IdleSynced => div()
            .flex()
            .items_center()
            .justify_center()
            .child(icon_svg("icons/cloud.svg", icon_size, color)),
        CloudSyncVisualState::Uploading => div()
            .relative()
            .w(px(icon_size))
            .h(px(icon_size))
            .overflow_hidden()
            .flex()
            .items_center()
            .justify_center()
            .child(icon_svg("icons/cloud.svg", icon_size, color))
            .child(
                div()
                    .absolute()
                    .inset_0()
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(icon_svg("icons/cloud-arrow-up.svg", icon_size, color))
                    .with_animation(
                        "sidebar-cloud-sync-up",
                        Animation::new(Duration::from_millis(950)).repeat(),
                        |el, d| {
                            let offset_y = 1.8 - 3.6 * d;
                            let alpha = (d * std::f32::consts::PI).sin().clamp(0.18, 1.0);
                            el.mt(px(offset_y)).opacity(alpha)
                        },
                    ),
            ),
        CloudSyncVisualState::Downloading => div()
            .relative()
            .w(px(icon_size))
            .h(px(icon_size))
            .overflow_hidden()
            .flex()
            .items_center()
            .justify_center()
            .child(icon_svg("icons/cloud.svg", icon_size, color))
            .child(
                div()
                    .absolute()
                    .inset_0()
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(icon_svg("icons/cloud-arrow-down.svg", icon_size, color))
                    .with_animation(
                        "sidebar-cloud-sync-down",
                        Animation::new(Duration::from_millis(950)).repeat(),
                        |el, d| {
                            let offset_y = -1.8 + 3.6 * d;
                            let alpha = (d * std::f32::consts::PI).sin().clamp(0.18, 1.0);
                            el.mt(px(offset_y)).opacity(alpha)
                        },
                    ),
            ),
        CloudSyncVisualState::Failed => div()
            .flex()
            .items_center()
            .justify_center()
            .child(icon_svg("icons/cloud-x.svg", icon_size, color)),
    };

    div()
        .id("btn-sidebar-sync-status")
        .w(px(28.0))
        .h(px(26.0))
        .rounded(px(7.0))
        .flex()
        .items_center()
        .justify_center()
        .cursor_pointer()
        .hover(|s| s.bg(theme.surface_pill))
        .active(|s| s.opacity(0.72))
        .on_click(cx.listener(|this, _, _, cx| {
            this.open_sync_settings();
            cx.notify();
        }))
        .child(glyph)
}

#[allow(clippy::too_many_arguments)]
fn render_host_row(
    idx: usize,
    host: &HostProfile,
    row_group: &str,
    index_in_group: usize,
    selected: bool,
    active_host_drag: Option<&HostDragState>,
    host_drop_zones: &Rc<RefCell<Vec<(HostDropZone, Bounds<Pixels>)>>>,
    row_height: f32,
    theme: &ThemeTokens,
    cx: &mut Context<HoloSshApp>,
) -> impl IntoElement {
    let host_id = host.id.clone();
    let drag_host_id = host.id.clone();
    let ctx_host_id = host.id.clone();
    let is_dragging_self = active_host_drag.is_some_and(|d| d.host_id == host.id);

    let mut row = div()
        .relative()
        .id(("host-row", idx))
        .w_full()
        .h(px(row_height))
        .px(px(10.0))
        .flex()
        .items_center()
        .justify_between()
        .rounded(LayoutMetrics::radius_row())
        .cursor_pointer()
        .opacity(if is_dragging_self { 0.35 } else { 1.0 })
        .bg(if selected {
            theme.row_selected
        } else {
            gpui::transparent_black()
        })
        .border_1()
        .border_color(if selected {
            theme.border_row_selected
        } else {
            gpui::transparent_black()
        })
        .hover(|s| {
            s.bg(if selected {
                theme.row_selected
            } else {
                theme.surface_pill
            })
        })
        .active(|s| s.opacity(if is_dragging_self { 0.35 } else { 0.78 }))
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(move |this, event: &MouseDownEvent, _window, _cx| {
                this.begin_host_press(&drag_host_id, event.position);
            }),
        )
        .on_mouse_down(
            MouseButton::Right,
            cx.listener(move |this, event: &MouseDownEvent, _window, cx| {
                this.open_host_context_menu(&ctx_host_id, event.position);
                cx.notify();
            }),
        )
        .on_click(cx.listener(move |this, _event, window, cx| {
            this.close_host_context_menu();
            window.focus(&this.focus_handle);
            this.handle_host_click(&host_id);
            cx.notify();
        }))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .items_center()
                .gap(px(8.0))
                .overflow_hidden()
                .child(div().flex_none().child(status_dot(
                    host.status == HostStatus::Connected,
                    theme,
                )))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .text_size(LayoutMetrics::font_body())
                        .line_height(px(18.0))
                        .text_color(if selected || host.status == HostStatus::Connected {
                            theme.sidebar_text_primary
                        } else {
                            theme.sidebar_text_secondary
                        })
                        .child(host.name.clone()),
                ),
        )
        .child(
            div()
                .flex_none()
                .pl(px(8.0))
                .pt(px(0.5))
                .whitespace_nowrap()
                .text_size(LayoutMetrics::font_micro())
                .line_height(px(18.0))
                .text_color(if selected {
                    theme.sidebar_text_secondary
                } else {
                    theme.sidebar_text_tertiary
                })
                .child(host.sidebar_subtitle()),
        );

    if active_host_drag.is_some() {
        let zones = host_drop_zones.clone();
        let hid = host.id.clone();
        let grp = row_group.to_string();
        row = row.child(
            canvas(
                move |_bounds, _window, _cx| {},
                move |bounds: Bounds<Pixels>, _, _window, _cx| {
                    zones.borrow_mut().push((
                        HostDropZone::HostRow {
                            host_id: hid.clone(),
                            group: grp.clone(),
                            index_in_group,
                        },
                        bounds,
                    ));
                },
            )
            .absolute()
            .inset_0(),
        );
    }

    row
}
