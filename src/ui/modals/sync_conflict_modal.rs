//! 全局云端同步冲突 / 首次绑定决策磨砂弹窗（挂接于根视图最顶层，任意页面触发均可弹出）
//!
//! 严格遵循黑白单色磨砂体系：
//! - 第一步：清晰展示云端快照版本、更新时间、密钥库是否同源及本机主机数，由用户明确决策：
//!   1. 从云端拉取并覆盖本地（自动先在本地 backups/ 备份）
//!   2. 双向智能合并（仅同密钥库可用）
//!   3. 以本地为准覆盖云端（生成全新 sync_epoch）
//!   4. 暂不同步（分叉态下自动暂停后台同步防止反复打扰）
//! - 第二步（按需）：若所选拉取/合并操作需要验证云端密钥库主密码，原地无缝切换为密码输入视图。

use crate::app::{ActiveInput, HoloSshApp, SyncConflictModalState, SyncConflictPasswordAction};
use crate::i18n::{tr, trf};
use crate::services::sync::SyncConflictReason;
use crate::theme::{LayoutMetrics, ThemeTokens};
use crate::ui::components::{editable_text_line, glass_pill_button, icon_svg};
use gpui::{
    Context, InteractiveElement, IntoElement, MouseButton, MouseDownEvent, ParentElement,
    StatefulInteractiveElement, Styled, div, px,
};

pub fn render_sync_conflict_modal(
    state: &SyncConflictModalState,
    active_input: ActiveInput,
    theme: &ThemeTokens,
    cx: &mut Context<HoloSshApp>,
) -> impl IntoElement {
    let is_initial = state.reason == SyncConflictReason::InitialBind;
    let rev_str = format!("#{}", state.snapshot.revision);
    let local_count_str = state.local_host_count.to_string();

    let (title, subtitle) = match state.password_action {
        Some(SyncConflictPasswordAction::OverwriteLocalFromCloud) => (
            tr("modal.sync_conflict.pwd_title_pull"),
            tr("modal.sync_conflict.pwd_desc_pull"),
        ),
        Some(SyncConflictPasswordAction::MergeBoth) => (
            tr("modal.sync_conflict.pwd_title_merge"),
            tr("modal.sync_conflict.pwd_desc_merge"),
        ),
        None if is_initial => (
            tr("modal.sync_conflict.title_initial"),
            tr("modal.sync_conflict.desc_initial"),
        ),
        None => (
            tr("modal.sync_conflict.title_diverged"),
            tr("modal.sync_conflict.desc_diverged"),
        ),
    };

    let mut sheet = div()
        .id("sync-conflict-modal-sheet")
        .w(px(430.0))
        .p(px(22.0))
        .flex()
        .flex_col()
        .gap(px(14.0))
        .rounded(LayoutMetrics::radius_window())
        .bg(theme.sheet_bg)
        .border_1()
        .border_color(theme.border_specular)
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(|_, _: &MouseDownEvent, _, cx| {
                cx.stop_propagation();
            }),
        )
        // 顶部标题行
        .child(
            div()
                .w_full()
                .flex()
                .items_center()
                .justify_between()
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(8.0))
                        .child(icon_svg("icons/sync.svg", 15.0, theme.text_primary))
                        .child(
                            div()
                                .text_size(LayoutMetrics::font_title())
                                .text_color(theme.text_primary)
                                .child(title),
                        ),
                )
                .child(
                    div()
                        .id("btn-sync-conflict-close")
                        .w(px(24.0))
                        .h(px(24.0))
                        .rounded(px(6.0))
                        .flex()
                        .items_center()
                        .justify_center()
                        .cursor_pointer()
                        .hover(|s| s.bg(theme.surface_pill))
                        .active(|s| s.opacity(0.70))
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.dismiss_sync_conflict_modal(false);
                            cx.notify();
                        }))
                        .child(icon_svg("icons/close.svg", 11.0, theme.text_tertiary)),
                ),
        )
        // 说明文案
        .child(
            div()
                .text_size(LayoutMetrics::font_control())
                .text_color(theme.text_secondary)
                .child(subtitle),
        );

    if let Some(_pwd_action) = state.password_action {
        // 第二步：原地输入云端密钥库主密码
        sheet = sheet.child(sync_conflict_secret_field(
            "input-sync-conflict-pwd",
            &state.password,
            tr("modal.sync_conflict.pwd_placeholder"),
            state.show_password,
            active_input == ActiveInput::SyncConflictPassword,
            theme,
            cx,
        ));
        if let Some(err) = state.error.as_deref() {
            sheet = sheet.child(
                div()
                    .text_size(LayoutMetrics::font_caption())
                    .text_color(theme.text_secondary)
                    .child(err.to_string()),
            );
        }
        sheet = sheet.child(
            div()
                .w_full()
                .pt(px(4.0))
                .flex()
                .items_center()
                .justify_end()
                .gap(px(8.0))
                .child(
                    glass_pill_button(tr("modal.sync_conflict.btn_back"), false, theme)
                        .id("btn-sync-conflict-back")
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.back_from_sync_conflict_password();
                            cx.notify();
                        })),
                )
                .child(
                    glass_pill_button(tr("modal.sync_conflict.btn_confirm"), true, theme)
                        .id("btn-sync-conflict-submit-pwd")
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.submit_sync_conflict_password();
                            cx.notify();
                        })),
                ),
        );
    } else {
        // 第一步：展示云端与本机状态摘要卡片 + 明确决策按钮
        let vault_status_text = if state.same_vault_salt {
            tr("modal.sync_conflict.vault_same")
        } else {
            tr("modal.sync_conflict.vault_diff")
        };

        let info_card = div()
            .w_full()
            .px(px(12.0))
            .py(px(10.0))
            .rounded(px(10.0))
            .bg(theme.card_bg)
            .border_1()
            .border_color(theme.border_subtle)
            .flex()
            .flex_col()
            .gap(px(6.0))
            .child(meta_row(
                tr("modal.sync_conflict.meta_remote_rev"),
                &rev_str,
                theme,
            ))
            .child(meta_row(
                tr("modal.sync_conflict.meta_remote_time"),
                &state.remote_time_display,
                theme,
            ))
            .child(meta_row(
                tr("modal.sync_conflict.meta_vault"),
                vault_status_text,
                theme,
            ))
            .child(meta_row(
                tr("modal.sync_conflict.meta_local_hosts"),
                &trf("modal.sync_conflict.host_count_fmt", &[("count", &local_count_str)]),
                theme,
            ));

        sheet = sheet.child(info_card);

        let mut actions = div().w_full().flex().flex_col().gap(px(8.0));

        // 1. 主推荐：从云端拉取并覆盖本地（自动先备份本地）
        actions = actions.child(
            glass_pill_button(tr("modal.sync_conflict.btn_pull_overwrite"), true, theme)
                .id("btn-sync-conflict-pull")
                .w_full()
                .h(px(34.0))
                .on_click(cx.listener(|this, _, _, cx| {
                    this.choose_sync_conflict_pull_overwrite();
                    cx.notify();
                })),
        );

        // 2. 同密钥库时提供无损双向合并
        if state.same_vault_salt {
            actions = actions.child(
                glass_pill_button(tr("modal.sync_conflict.btn_merge"), false, theme)
                    .id("btn-sync-conflict-merge")
                    .w_full()
                    .h(px(34.0))
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.choose_sync_conflict_merge();
                        cx.notify();
                    })),
            );
        }

        // 3. 以本机为准强制覆盖云端（生成新 sync_epoch）
        actions = actions.child(
            glass_pill_button(tr("modal.sync_conflict.btn_push_overwrite"), false, theme)
                .id("btn-sync-conflict-push")
                .w_full()
                .h(px(34.0))
                .on_click(cx.listener(|this, _, _, cx| {
                    this.choose_sync_conflict_push_overwrite();
                    cx.notify();
                })),
        );

        // 4. 底部辅助提示 + 暂不同步按钮
        let cancel_label = if is_initial {
            tr("common.cancel")
        } else {
            tr("modal.sync_conflict.btn_pause_auto")
        };

        sheet = sheet
            .child(actions)
            .child(
                div()
                    .w_full()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap(px(8.0))
                    .child(
                        div()
                            .flex_1()
                            .text_size(LayoutMetrics::font_micro())
                            .text_color(theme.text_tertiary)
                            .child(tr("modal.sync_conflict.backup_hint")),
                    )
                    .child(
                        div()
                            .id("btn-sync-conflict-dismiss")
                            .px(px(10.0))
                            .py(px(5.0))
                            .rounded(px(6.0))
                            .cursor_pointer()
                            .text_size(LayoutMetrics::font_caption())
                            .text_color(theme.text_tertiary)
                            .hover(|s| s.bg(theme.surface_pill).text_color(theme.text_primary))
                            .active(|s| s.opacity(0.75))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.dismiss_sync_conflict_modal(!is_initial);
                                cx.notify();
                            }))
                            .child(cancel_label),
                    ),
            );
    }

    sheet
}

fn meta_row(label: &str, value: &str, theme: &ThemeTokens) -> impl IntoElement {
    div()
        .w_full()
        .flex()
        .items_center()
        .justify_between()
        .gap(px(12.0))
        .child(
            div()
                .text_size(LayoutMetrics::font_caption())
                .text_color(theme.text_tertiary)
                .child(label.to_string()),
        )
        .child(
            div()
                .text_size(LayoutMetrics::font_caption())
                .text_color(theme.text_primary)
                .child(value.to_string()),
        )
}

fn sync_conflict_secret_field(
    id: &'static str,
    value: &str,
    placeholder: &str,
    show_plain: bool,
    focused: bool,
    theme: &ThemeTokens,
    cx: &mut Context<HoloSshApp>,
) -> impl IntoElement {
    let left = div()
        .flex_1()
        .min_w_0()
        .overflow_hidden()
        .flex()
        .items_center()
        .child(editable_text_line(
            ActiveInput::SyncConflictPassword,
            value,
            placeholder,
            !show_plain,
            focused,
            LayoutMetrics::font_body(),
            14.0,
            theme.text_primary,
            theme.text_tertiary,
            None,
            theme,
        ));

    div()
        .id(id)
        .w_full()
        .h(px(36.0))
        .px(px(12.0))
        .flex()
        .items_center()
        .justify_between()
        .gap(px(8.0))
        .rounded(px(9.0))
        .cursor_text()
        .bg(theme.surface_input)
        .border_1()
        .border_color(if focused {
            theme.border_specular
        } else {
            theme.border_subtle
        })
        .hover(|s| s.border_color(theme.border_specular))
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(|this, ev: &MouseDownEvent, window, cx| {
                cx.stop_propagation();
                this.begin_text_input_mouse_down(ActiveInput::SyncConflictPassword, ev, window);
                cx.notify();
            }),
        )
        .child(left)
        .child(
            div()
                .id("btn-sync-conflict-pwd-eye")
                .w(px(22.0))
                .h(px(22.0))
                .flex_none()
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(6.0))
                .cursor_pointer()
                .hover(|s| s.bg(theme.surface_pill))
                .active(|s| s.opacity(0.70))
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, _: &MouseDownEvent, _, cx| {
                        cx.stop_propagation();
                        this.toggle_sync_conflict_password_visibility();
                        cx.notify();
                    }),
                )
                .child(icon_svg("icons/eye.svg", 14.0, theme.text_tertiary)),
        )
}
