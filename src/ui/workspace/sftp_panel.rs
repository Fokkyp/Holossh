//! 右侧无缝 SFTP 文件面板（严格对应 Figma 04 画板最终版）
//!
//! 无搜索框、无文件图标、对齐四列（名称 | 大小 | 修改时间 | 权限）、最下方实时单色进度条。

use crate::app::{ActiveInput, HoloSshApp};
use crate::i18n::{tr, trf};
use crate::state::sftp::{SftpPanelState, TransferDirection, TransferTask};
use crate::theme::{LayoutMetrics, ThemeTokens};
use crate::ui::components::{editable_text_line, hairline_h, icon_svg, module_loading_overlay};
use gpui::{
    ClickEvent, Context, InteractiveElement, IntoElement, MouseButton, MouseDownEvent,
    ParentElement, StatefulInteractiveElement, Styled, Transformation, UniformListScrollHandle,
    div, percentage, px, relative, uniform_list,
};

#[allow(clippy::too_many_arguments)]
pub fn render_sftp_panel(
    sftp: &SftpPanelState,
    creating_dir: bool,
    new_dir_name: &str,
    new_dir_focused: bool,
    scroll_handle: UniformListScrollHandle,
    width_px: f32,
    theme: &ThemeTokens,
    cx: &mut Context<HoloSshApp>,
) -> impl IntoElement {
    // 1. 面包屑路径栏 + 裸操作图标（上传 / 新建文件夹 / 刷新）
    let mut crumbs = div()
        .flex_1()
        .min_w_0()
        .overflow_hidden()
        .flex()
        .items_center()
        .gap(px(5.0))
        .font_family("Menlo")
        .text_size(LayoutMetrics::font_control());
    let last_idx = sftp.path_segments.len().saturating_sub(1);
    for (i, seg) in sftp.path_segments.iter().enumerate() {
        if i > 0 {
            crumbs = crumbs.child(div().text_color(theme.text_tertiary).child("›"));
        }
        crumbs = crumbs.child(
            div()
                .id(("sftp-crumb", i))
                .cursor_pointer()
                .truncate()
                .text_color(if i == last_idx {
                    theme.text_primary
                } else {
                    theme.text_tertiary
                })
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.sftp_navigate_to_crumb(i);
                    cx.notify();
                }))
                .child(seg.clone()),
        );
    }

    let header = div()
        .w_full()
        .h(px(42.0))
        .px(px(14.0))
        .flex()
        .items_center()
        .justify_between()
        .gap(px(8.0))
        .overflow_hidden()
        .child(crumbs)
        .child(
            div()
                .flex_none()
                .flex()
                .items_center()
                .gap(px(4.0))
                .child(
                    div()
                        .id("btn-sftp-upload")
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
                            this.prompt_sftp_upload(cx);
                        }))
                        .child(icon_svg("icons/upload.svg", 13.5, theme.text_secondary)),
                )
                .child(
                    div()
                        .id("btn-sftp-mkdir")
                        .w(px(26.0))
                        .h(px(26.0))
                        .rounded(px(6.5))
                        .flex()
                        .items_center()
                        .justify_center()
                        .cursor_pointer()
                        .hover(|s| s.bg(theme.surface_pill))
                        .active(|s| s.opacity(0.70))
                        .on_click(cx.listener(|this, _, window, cx| {
                            window.focus(&this.focus_handle);
                            this.start_sftp_create_folder();
                            cx.notify();
                        }))
                        .child(icon_svg(
                            "icons/folder-plus.svg",
                            13.5,
                            theme.text_secondary,
                        )),
                )
                .child(
                    div()
                        .id("btn-sftp-refresh")
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
                            this.sftp_refresh();
                            cx.notify();
                        }))
                        .child(icon_svg("icons/refresh.svg", 13.0, theme.text_tertiary)),
                ),
        );

    // 2. 对齐四列表头：名称 | 大小 | 修改时间 | 权限（无搜索框）
    let col_header = div()
        .w_full()
        .px(px(16.0))
        .pt(px(9.0))
        .pb(px(6.0))
        .flex()
        .items_center()
        .gap(px(10.0))
        .overflow_hidden()
        .text_size(LayoutMetrics::font_caption())
        .text_color(theme.text_tertiary)
        .child(div().flex_1().min_w_0().truncate().child(tr("sftp.col_name")))
        .child(div().w(px(54.0)).flex_none().flex().justify_end().child(tr("sftp.col_size")))
        .child(div().w(px(64.0)).flex_none().flex().justify_end().child(tr("sftp.col_modified")))
        .child(div().w(px(68.0)).flex_none().flex().justify_end().child(tr("sftp.col_perms")));

    // 3. 虚拟滚动文件列表（uniform_list 仅渲染视口内可见行，万级文件依旧满帧流畅）
    let mut rows = div()
        .flex_1()
        .relative()
        .min_h_0()
        .w_full()
        .px(px(8.0))
        .flex()
        .flex_col()
        .gap(px(2.0))
        .overflow_hidden();

    if creating_dir {
        let input_left = div()
            .flex_1()
            .min_w_0()
            .overflow_hidden()
            .flex()
            .items_center()
            .cursor_text()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, ev: &MouseDownEvent, window, cx| {
                    cx.stop_propagation();
                    this.begin_text_input_mouse_down(ActiveInput::SftpNewDir, ev, window);
                    cx.notify();
                }),
            )
            .child(editable_text_line(
                ActiveInput::SftpNewDir,
                new_dir_name,
                tr("sftp.new_folder_placeholder"),
                false,
                new_dir_focused,
                LayoutMetrics::font_control(),
                13.0,
                theme.text_primary,
                theme.text_tertiary,
                Some("Menlo"),
                theme,
            ));

        rows = rows.child(
            div()
                .id("sftp-new-dir-row")
                .flex_none()
                .w_full()
                .h(px(32.0))
                .px(px(8.0))
                .mb(px(2.0))
                .rounded(px(7.5))
                .bg(theme.surface_input)
                .border_1()
                .border_color(theme.border_specular)
                .flex()
                .items_center()
                .justify_between()
                .gap(px(6.0))
                .child(input_left)
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(4.0))
                        .child(
                            div()
                                .id("btn-sftp-mkdir-confirm")
                                .px(px(5.0))
                                .rounded(px(4.0))
                                .text_size(LayoutMetrics::font_caption())
                                .text_color(theme.text_primary)
                                .cursor_pointer()
                                .hover(|s| s.bg(theme.surface_active))
                                .on_click(cx.listener(|this, _, _, cx| {
                                    cx.stop_propagation();
                                    this.commit_sftp_create_folder();
                                    cx.notify();
                                }))
                                .child("✓"),
                        )
                        .child(
                            div()
                                .id("btn-sftp-mkdir-cancel")
                                .w(px(16.0))
                                .h(px(16.0))
                                .rounded(px(4.0))
                                .flex()
                                .items_center()
                                .justify_center()
                                .cursor_pointer()
                                .hover(|s| s.bg(theme.surface_active))
                                .on_click(cx.listener(|this, _, _, cx| {
                                    cx.stop_propagation();
                                    this.cancel_sftp_create_folder();
                                    cx.notify();
                                }))
                                .child(icon_svg("icons/close.svg", 9.5, theme.text_tertiary)),
                        ),
                ),
        );
    }

    if !sftp.loading && sftp.entries.is_empty() {
        rows = rows.child(
            div()
                .flex_none()
                .px(px(8.0))
                .py(px(10.0))
                .text_size(LayoutMetrics::font_caption())
                .text_color(theme.text_tertiary)
                .child(tr("sftp.empty")),
        );
    }

    if !sftp.entries.is_empty() {
        let theme_copy = *theme;
        let entry_count = sftp.entries.len();
        let list = uniform_list(
            "sftp-file-list",
            entry_count,
            cx.processor(move |this, range: std::ops::Range<usize>, _window, cx| {
                let mut items = Vec::with_capacity(range.len());
                for idx in range {
                    let Some(entry) = this.sftp.entries.get(idx) else {
                        continue;
                    };
                    let selected = this.sftp.selected_name.as_deref() == Some(entry.name.as_str());
                    let entry_name = entry.name.clone();
                    let ctx_name = entry.name.clone();
                    let is_dir = entry.is_dir;
                    let is_parent = entry.name == "../";

                    let mut row = div()
                        .id(("sftp-row", idx))
                        .w_full()
                        .h(px(34.0))
                        .py(px(1.0))
                        .px(px(8.0))
                        .overflow_hidden()
                        .flex()
                        .items_center()
                        .gap(px(10.0))
                        .rounded(px(7.5))
                        .cursor_pointer()
                        .bg(if selected {
                            theme_copy.surface_active
                        } else {
                            gpui::transparent_black()
                        })
                        .hover(move |s| {
                            s.bg(if selected {
                                theme_copy.surface_active
                            } else {
                                theme_copy.surface_pill
                            })
                        })
                        .active(|s| s.opacity(0.78))
                        .on_click(cx.listener(move |this, event: &ClickEvent, _, cx| {
                            this.close_sftp_context_menu();
                            this.sftp_select_or_enter(&entry_name, is_dir, event.click_count() == 2);
                            cx.notify();
                        }));
                    if !is_parent {
                        row = row.on_mouse_down(
                            MouseButton::Right,
                            cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                                this.open_sftp_context_menu(&ctx_name, is_dir, event.position);
                                cx.notify();
                            }),
                        );
                    }
                    let row = row
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .text_size(LayoutMetrics::font_control())
                                .text_color(if entry.is_hidden {
                                    theme_copy.text_secondary
                                } else {
                                    theme_copy.text_primary
                                })
                                .child(entry.name.clone()),
                        )
                        .child(
                            div()
                                .w(px(54.0))
                                .flex_none()
                                .flex()
                                .justify_end()
                                .whitespace_nowrap()
                                .font_family("Menlo")
                                .text_size(LayoutMetrics::font_micro())
                                .text_color(theme_copy.text_tertiary)
                                .child(entry.size_display.clone()),
                        )
                        .child(
                            div()
                                .w(px(64.0))
                                .flex_none()
                                .flex()
                                .justify_end()
                                .whitespace_nowrap()
                                .text_size(LayoutMetrics::font_micro())
                                .text_color(theme_copy.text_tertiary)
                                .child(entry.modified_display.clone()),
                        )
                        .child(
                            div()
                                .w(px(68.0))
                                .flex_none()
                                .flex()
                                .justify_end()
                                .whitespace_nowrap()
                                .font_family("Menlo")
                                .text_size(LayoutMetrics::font_micro())
                                .text_color(if selected {
                                    theme_copy.text_secondary
                                } else {
                                    theme_copy.text_tertiary
                                })
                                .child(entry.permissions.clone()),
                        );
                    items.push(row);
                }
                items
            }),
        )
        .track_scroll(scroll_handle)
        .flex_1()
        .w_full();
        rows = rows.child(list);
    }

    if sftp.loading {
        rows = rows.child(module_loading_overlay("sftp-dir", theme));
    }

    let mut panel = div()
        .relative()
        .flex_none()
        .w(px(width_px))
        .h_full()
        .overflow_hidden()
        .flex()
        .flex_col()
        .bg(theme.sftp_bg)
        .border_l_1()
        .border_color(theme.border_subtle)
        .child(header)
        .child(hairline_h(theme))
        .child(col_header)
        .child(rows);

    // 4. 底部自适应单/多任务传输区：
    // - 0 项：隐藏，文件列表直通底边
    // - 1 项：单行极简进度条（独立方向图标 + 实时 MB/% + 单任务真实取消 ×）
    // - ≥2 项：默认紧凑汇总 1 行（点击展开向上滚动多任务清单，逐条可独立取消）
    if sftp.transfers.len() == 1 {
        let task = &sftp.transfers[0];
        panel = panel
            .child(hairline_h(theme))
            .child(render_single_transfer_row(task, false, theme, cx));
    } else if sftp.transfers.len() > 1 {
        if sftp.transfers_expanded {
            let mut list_box = div()
                .id("sftp-transfers-list")
                .w_full()
                .max_h(px(184.0))
                .overflow_y_scroll()
                .flex()
                .flex_col();
            for (i, task) in sftp.transfers.iter().enumerate() {
                if i > 0 {
                    list_box = list_box.child(div().w_full().px(px(14.0)).child(hairline_h(theme)));
                }
                list_box = list_box.child(render_single_transfer_row(task, true, theme, cx));
            }
            panel = panel.child(hairline_h(theme)).child(list_box);
        }

        let total_count = sftp.transfers.len();
        let active_count = sftp.transfers.iter().filter(|t| !t.done).count();
        let all_done = active_count == 0;
        let sum_transferred: f32 = sftp.transfers.iter().map(|t| t.transferred_mb).sum();
        let sum_total: f32 = sftp.transfers.iter().map(|t| t.total_mb).sum();
        let agg_progress = if sum_total > 0.0 {
            (sum_transferred / sum_total).clamp(0.0, 1.0)
        } else {
            let sum_p: f32 = sftp.transfers.iter().map(|t| t.progress.clamp(0.0, 1.0)).sum();
            (sum_p / total_count as f32).clamp(0.0, 1.0)
        };
        let agg_pct = (agg_progress * 100.0).round() as u32;
        let has_upload = sftp
            .transfers
            .iter()
            .any(|t| t.direction == TransferDirection::Upload && !t.done);
        let summary_icon = if has_upload {
            "icons/upload.svg"
        } else {
            "icons/cloud-download.svg"
        };
        let summary_label = if all_done {
            trf(
                "sftp.transfers_done_summary",
                &[("count", &total_count.to_string())],
            )
        } else {
            trf(
                "sftp.transfers_summary",
                &[("count", &active_count.to_string())],
            )
        };

        let chevron = if sftp.transfers_expanded {
            icon_svg("icons/chevron-down.svg", 11.0, theme.text_tertiary)
        } else {
            icon_svg("icons/chevron-down.svg", 11.0, theme.text_tertiary)
                .with_transformation(Transformation::rotate(percentage(0.5)))
        };

        let mut right_group = div()
            .flex_none()
            .flex()
            .items_center()
            .gap(px(8.0))
            .child(
                div()
                    .whitespace_nowrap()
                    .font_family("Menlo")
                    .text_size(LayoutMetrics::font_micro())
                    .text_color(theme.text_secondary)
                    .child(format!(
                        "{:.1} / {:.1} MB · {}%",
                        sum_transferred, sum_total, agg_pct
                    )),
            )
            .child(
                div()
                    .flex_none()
                    .w(px(16.0))
                    .h(px(16.0))
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(chevron),
            );
        if all_done {
            right_group = right_group.child(
                div()
                    .id("btn-sftp-clear-transfers")
                    .flex_none()
                    .w(px(16.0))
                    .h(px(16.0))
                    .rounded(px(4.0))
                    .flex()
                    .items_center()
                    .justify_center()
                    .cursor_pointer()
                    .hover(|s| s.bg(theme.surface_pill))
                    .on_click(cx.listener(|this, _, _, cx| {
                        cx.stop_propagation();
                        this.sftp_clear_transfers();
                        cx.notify();
                    }))
                    .child(icon_svg("icons/close.svg", 10.0, theme.text_tertiary)),
            );
        }

        panel = panel.child(hairline_h(theme)).child(
            div()
                .id("sftp-transfers-summary")
                .w_full()
                .px(px(14.0))
                .pt(px(11.0))
                .pb(px(13.0))
                .overflow_hidden()
                .flex()
                .flex_col()
                .gap(px(8.0))
                .cursor_pointer()
                .hover(|s| s.bg(theme.surface_pill))
                .on_click(cx.listener(|this, _, _, cx| {
                    this.sftp_toggle_transfers_expanded();
                    cx.notify();
                }))
                .child(
                    div()
                        .w_full()
                        .min_w_0()
                        .overflow_hidden()
                        .flex()
                        .items_center()
                        .justify_between()
                        .gap(px(10.0))
                        .child(
                            div()
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
                                        .child(icon_svg(summary_icon, 12.0, theme.text_secondary)),
                                )
                                .child(
                                    div()
                                        .flex_1()
                                        .min_w_0()
                                        .truncate()
                                        .text_size(LayoutMetrics::font_caption())
                                        .text_color(theme.text_primary)
                                        .child(summary_label),
                                ),
                        )
                        .child(right_group),
                )
                .child(
                    div()
                        .w_full()
                        .h(px(4.0))
                        .rounded(LayoutMetrics::radius_pill())
                        .bg(theme.surface_pill)
                        .overflow_hidden()
                        .child(
                            div()
                                .w(relative(agg_progress))
                                .h_full()
                                .rounded(LayoutMetrics::radius_pill())
                                .bg(theme.text_primary),
                        ),
                ),
        );
    }

    // 左侧 7px 可左右拖拽中线手柄（无缝叠在 border_l_1 上）
    panel.child(
        div()
            .id("sftp-resize-handle")
            .absolute()
            .top_0()
            .bottom_0()
            .left_0()
            .w(px(7.0))
            .cursor(gpui::CursorStyle::ResizeLeftRight)
            .hover(|s| s.bg(theme.surface_pill))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, event: &MouseDownEvent, _window, cx| {
                    this.start_drag_sftp(event.position);
                    cx.stop_propagation();
                }),
            ),
    )
}

fn render_single_transfer_row(
    task: &TransferTask,
    compact: bool,
    theme: &ThemeTokens,
    cx: &mut Context<HoloSshApp>,
) -> impl IntoElement {
    let clamped_progress = task.progress.clamp(0.0, 1.0);
    let pct = (clamped_progress * 100.0).round() as u32;
    let task_id = task.id;
    let icon_path = match task.direction {
        TransferDirection::Upload => "icons/upload.svg",
        TransferDirection::Download => "icons/cloud-download.svg",
    };

    div()
        .w_full()
        .px(px(14.0))
        .pt(px(if compact { 8.0 } else { 11.0 }))
        .pb(px(if compact { 9.0 } else { 13.0 }))
        .overflow_hidden()
        .flex()
        .flex_col()
        .gap(px(if compact { 6.0 } else { 8.0 }))
        .child(
            div()
                .w_full()
                .min_w_0()
                .overflow_hidden()
                .flex()
                .items_center()
                .justify_between()
                .gap(px(10.0))
                .child(
                    div()
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
                                .child(icon_svg(icon_path, 12.0, theme.text_secondary)),
                        )
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .text_size(LayoutMetrics::font_caption())
                                .text_color(if task.done {
                                    theme.text_secondary
                                } else {
                                    theme.text_primary
                                })
                                .child(task.file_name.clone()),
                        ),
                )
                .child(
                    div()
                        .flex_none()
                        .flex()
                        .items_center()
                        .gap(px(8.0))
                        .child(
                            div()
                                .whitespace_nowrap()
                                .font_family("Menlo")
                                .text_size(LayoutMetrics::font_micro())
                                .text_color(theme.text_secondary)
                                .child(format!(
                                    "{:.1} / {:.1} MB · {}%",
                                    task.transferred_mb, task.total_mb, pct
                                )),
                        )
                        .child(
                            div()
                                .id(("btn-sftp-cancel-transfer", task_id as usize))
                                .flex_none()
                                .w(px(16.0))
                                .h(px(16.0))
                                .rounded(px(4.0))
                                .flex()
                                .items_center()
                                .justify_center()
                                .cursor_pointer()
                                .hover(|s| s.bg(theme.surface_pill))
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    cx.stop_propagation();
                                    this.sftp_cancel_or_remove_transfer(task_id);
                                    cx.notify();
                                }))
                                .child(icon_svg("icons/close.svg", 10.0, theme.text_tertiary)),
                        ),
                ),
        )
        .child(
            div()
                .w_full()
                .h(px(if compact { 3.0 } else { 4.0 }))
                .rounded(LayoutMetrics::radius_pill())
                .bg(theme.surface_pill)
                .overflow_hidden()
                .child(
                    div()
                        .w(relative(clamped_progress))
                        .h_full()
                        .rounded(LayoutMetrics::radius_pill())
                        .bg(if task.done {
                            theme.text_secondary
                        } else {
                            theme.text_primary
                        }),
                ),
        )
}
