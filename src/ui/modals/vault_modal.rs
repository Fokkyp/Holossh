//! 密钥库首次设置主密码 / 解锁磨砂 Sheet 弹窗
//!
//! - 首次在首页点击「+ 添加主机」或进入未初始化密钥库时，必须先设置密钥库主密码，设置完成后自动进入添加主机弹窗；
//! - 密钥库处于锁定态时点击添加/编辑主机，先验证主密码解锁后再读写加密凭据。

use crate::app::{ActiveInput, HoloSshApp, VaultModalDraft};
use crate::i18n::tr;
use crate::theme::{LayoutMetrics, ThemeTokens};
use crate::ui::components::{editable_text_line, glass_pill_button, icon_svg};
use gpui::{
    Context, InteractiveElement, IntoElement, MouseButton, MouseDownEvent, ParentElement,
    StatefulInteractiveElement, Styled, div, px,
};

pub fn render_vault_modal(
    draft: &VaultModalDraft,
    active_input: ActiveInput,
    theme: &ThemeTokens,
    cx: &mut Context<HoloSshApp>,
) -> impl IntoElement {
    let is_setup = draft.is_setup;
    let title = if is_setup {
        tr("modal.vault.title_setup")
    } else {
        tr("modal.vault.title_unlock")
    };
    let subtitle = if is_setup {
        tr("modal.vault.subtitle_setup")
    } else {
        tr("modal.vault.subtitle_unlock")
    };

    let mut sheet = div()
        .id("vault-modal-sheet")
        .w(px(380.0))
        .p(px(22.0))
        .flex()
        .flex_col()
        .gap(px(12.0))
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
        .child(
            div()
                .w_full()
                .flex()
                .items_center()
                .justify_between()
                .child(
                    div()
                        .text_size(LayoutMetrics::font_title())
                        .text_color(theme.text_primary)
                        .child(title),
                )
                .child(
                    div()
                        .id("btn-vault-modal-close")
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
                            this.close_vault_modal();
                            cx.notify();
                        }))
                        .child(icon_svg("icons/close.svg", 11.0, theme.text_tertiary)),
                ),
        )
        .child(
            div()
                .text_size(LayoutMetrics::font_control())
                .text_color(theme.text_tertiary)
                .child(subtitle),
        )
        .child(vault_modal_secret_field(
            "input-vault-modal-pwd",
            &draft.password,
            tr("modal.vault.placeholder_pwd"),
            draft.show_password,
            active_input == ActiveInput::VaultModalPassword,
            ActiveInput::VaultModalPassword,
            theme,
            cx,
        ));

    if is_setup {
        sheet = sheet.child(vault_modal_secret_field(
            "input-vault-modal-confirm",
            &draft.confirm_password,
            tr("modal.vault.placeholder_confirm"),
            draft.show_password,
            active_input == ActiveInput::VaultModalConfirm,
            ActiveInput::VaultModalConfirm,
            theme,
            cx,
        ));
    }

    sheet.child(
        div()
            .w_full()
            .pt(px(4.0))
            .flex()
            .items_center()
            .justify_end()
            .gap(px(8.0))
            .child(
                glass_pill_button(tr("common.cancel"), false, theme)
                    .id("btn-vault-modal-cancel")
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.close_vault_modal();
                        cx.notify();
                    })),
            )
            .child(
                glass_pill_button(
                    if is_setup {
                        tr("modal.vault.btn_setup")
                    } else {
                        tr("modal.vault.btn_unlock")
                    },
                    true,
                    theme,
                )
                .id("btn-vault-modal-submit")
                .on_click(cx.listener(|this, _, _, cx| {
                    this.submit_vault_modal();
                    cx.notify();
                })),
            ),
    )
}

#[allow(clippy::too_many_arguments)]
fn vault_modal_secret_field(
    id: &'static str,
    value: &str,
    placeholder: &str,
    show_plain: bool,
    focused: bool,
    target: ActiveInput,
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
            target,
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
        .h(px(34.0))
        .px(px(11.0))
        .flex()
        .items_center()
        .justify_between()
        .gap(px(6.0))
        .rounded(px(8.5))
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
            cx.listener(move |this, ev: &MouseDownEvent, window, cx| {
                cx.stop_propagation();
                this.begin_text_input_mouse_down(target, ev, window);
                cx.notify();
            }),
        )
        .child(left)
        .child(
            div()
                .id(gpui::SharedString::from(format!("{id}-eye")))
                .w(px(22.0))
                .h(px(22.0))
                .flex_none()
                .flex()
                .items_center()
                .justify_center()
                .cursor_pointer()
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, _: &MouseDownEvent, _, cx| {
                        cx.stop_propagation();
                        if let Some(m) = this.vault_modal.as_mut() {
                            m.show_password = !m.show_password;
                        }
                        cx.notify();
                    }),
                )
                .child(icon_svg("icons/eye.svg", 14.0, theme.text_tertiary)),
        )
}
