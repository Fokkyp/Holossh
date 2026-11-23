//! 新建主机磨砂 Sheet 弹窗（严格对应 Figma 02 画板）
//!
//! 所有输入字段支持直接点击聚焦、键入、Tab 切换、⌘V 粘贴与回车保存并连接；
//! 密码直接内联输入并加密存入跨平台密钥库；「添加私钥」调用系统原生文件选择器。

use crate::app::{ActiveInput, DropdownKind, HoloSshApp, NewHostDraft};
use crate::i18n::tr;
use crate::state::host::{AuthMethod, ConnectionKind};
use crate::theme::{LayoutMetrics, ThemeTokens};
use crate::ui::components::{
    editable_text_line, glass_pill_button, hairline_h, icon_svg, native_dropdown,
};
use gpui::{
    Bounds, Context, Div, InteractiveElement, IntoElement, MouseButton, MouseDownEvent,
    ParentElement, Pixels, StatefulInteractiveElement, Styled, div, px,
};
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

pub fn render_new_host_modal(
    draft: &NewHostDraft,
    jump_host_label: Option<&str>,
    is_editing: bool,
    active_input: ActiveInput,
    dropdown_anchors: &Rc<RefCell<HashMap<DropdownKind, Bounds<Pixels>>>>,
    theme: &ThemeTokens,
    cx: &mut Context<HoloSshApp>,
) -> impl IntoElement + use<> {
    let methods = [
        AuthMethod::Auto,
        AuthMethod::Password,
        AuthMethod::PrivateKey,
        AuthMethod::Agent,
        AuthMethod::KeyboardInteractive,
    ];

    let mut auth_bar = div()
        .w_full()
        .p(px(3.0))
        .flex()
        .items_center()
        .gap(px(3.0))
        .rounded(px(10.0))
        .bg(theme.surface_input)
        .border_1()
        .border_color(theme.border_subtle);

    for (idx, m) in methods.into_iter().enumerate() {
        let active = m == draft.auth_method;
        auth_bar = auth_bar.child(
            div()
                .id(("auth-method", idx))
                .flex_1()
                .py(px(8.0))
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .gap(px(5.0))
                .rounded(px(7.5))
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
                .hover(|s| {
                    s.bg(if active {
                        theme.surface_active
                    } else {
                        theme.surface_pill
                    })
                })
                .active(|s| s.opacity(0.78))
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.new_host_draft.auth_method = m;
                    cx.notify();
                }))
                .child(icon_svg(
                    m.icon_path(),
                    15.0,
                    if active {
                        theme.text_primary
                    } else {
                        theme.text_secondary
                    },
                ))
                .child(
                    div()
                        .text_size(LayoutMetrics::font_caption())
                        .text_color(if active {
                            theme.text_primary
                        } else {
                            theme.text_secondary
                        })
                        .child(m.label_zh()),
                ),
        );
    }

    let pwd_focused = active_input == ActiveInput::ModalPassword;
    let pwd_placeholder = match draft.auth_method {
        AuthMethod::PrivateKey => tr("modal.host.passphrase_placeholder"),
        AuthMethod::Password | AuthMethod::KeyboardInteractive | AuthMethod::Auto => {
            tr("modal.host.password_optional_placeholder")
        }
        AuthMethod::Agent => tr("modal.host.password_placeholder"),
    };
    let pwd_left = div()
        .flex_1()
        .min_w_0()
        .overflow_hidden()
        .flex()
        .items_center()
        .child(editable_text_line(
            ActiveInput::ModalPassword,
            &draft.password,
            pwd_placeholder,
            !draft.show_password,
            pwd_focused,
            LayoutMetrics::font_body(),
            14.0,
            theme.text_primary,
            theme.text_tertiary,
            None,
            theme,
        ));

    let is_local = draft.connection == ConnectionKind::Local;
    let name_placeholder = if is_local {
        tr("modal.host.local_name_placeholder")
    } else {
        tr("modal.host.name_placeholder")
    };

    let mut sheet = div()
        .id("new-host-sheet-card")
        .w(px(LayoutMetrics::NEW_HOST_SHEET_WIDTH))
        .p(px(22.0))
        .flex()
        .flex_col()
        .gap(px(15.0))
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
        // 标题行
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
                        .child(if is_editing {
                            tr("modal.host.title_edit")
                        } else {
                            tr("modal.host.title_new")
                        }),
                )
                .child(
                    div()
                        .id("btn-modal-close")
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
                            this.close_new_host_modal();
                            cx.notify();
                        }))
                        .child(icon_svg("icons/close.svg", 11.0, theme.text_tertiary)),
                ),
        )
        // 名称 + 分组
        .child(
            div()
                .w_full()
                .flex()
                .gap(px(10.0))
                .child(
                    editable_labeled_input(
                        "input-modal-name",
                        tr("modal.host.name"),
                        &draft.name,
                        name_placeholder,
                        false,
                        active_input == ActiveInput::ModalName,
                        ActiveInput::ModalName,
                        theme,
                        cx,
                    )
                    .flex_1(),
                )
                .child({
                    let group_anchors = Rc::clone(dropdown_anchors);
                    div()
                        .w(px(146.0))
                        .flex()
                        .flex_col()
                        .gap(px(6.0))
                        .child(field_label(tr("modal.host.group"), theme))
                        .child(
                            div()
                                .w_full()
                                .on_children_prepainted(move |bounds: Vec<Bounds<Pixels>>, _, _| {
                                    if let Some(first) = bounds.first().copied() {
                                        group_anchors
                                            .borrow_mut()
                                            .insert(DropdownKind::DraftGroup, first);
                                    }
                                })
                                .child(
                                    native_dropdown(
                                        if draft.group.trim().is_empty() {
                                            tr("common.none")
                                        } else {
                                            draft.group.as_str()
                                        },
                                        146.0,
                                        theme,
                                    )
                                    .id("btn-draft-group")
                                    .w_full()
                                    .h(px(34.0))
                                    .on_mouse_down(
                                        MouseButton::Left,
                                        cx.listener(|this, event: &MouseDownEvent, _, cx| {
                                            this.toggle_dropdown(
                                                DropdownKind::DraftGroup,
                                                event.position,
                                                146.0,
                                            );
                                            cx.notify();
                                        }),
                                    ),
                                ),
                        )
                }),
        );

    // 连接方式行：本地终端时仅保留通栏连接下拉，隐藏所有远端地址/端口/凭据字段
    if is_local {
        let conn_anchors = Rc::clone(dropdown_anchors);
        let full_w = LayoutMetrics::NEW_HOST_SHEET_WIDTH - 44.0;
        sheet = sheet.child(
            div()
                .w_full()
                .flex()
                .flex_col()
                .gap(px(6.0))
                .child(field_label(tr("modal.host.connection"), theme))
                .child(
                    div()
                        .w_full()
                        .on_children_prepainted(move |bounds: Vec<Bounds<Pixels>>, _, _| {
                            if let Some(first) = bounds.first().copied() {
                                conn_anchors
                                    .borrow_mut()
                                    .insert(DropdownKind::DraftConnection, first);
                            }
                        })
                        .child(
                            native_dropdown(draft.connection.label_zh(), full_w, theme)
                                .id("btn-draft-conn")
                                .w_full()
                                .h(px(34.0))
                                .on_mouse_down(
                                    MouseButton::Left,
                                    cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                                        this.toggle_dropdown(
                                            DropdownKind::DraftConnection,
                                            event.position,
                                            full_w,
                                        );
                                        cx.notify();
                                    }),
                                ),
                        ),
                ),
        );
    } else {
        // 连接 + 主机 + 端口
        sheet = sheet.child(
            div()
                .w_full()
                .flex()
                .gap(px(10.0))
                .child({
                    let conn_anchors = Rc::clone(dropdown_anchors);
                    div()
                        .w(px(96.0))
                        .flex()
                        .flex_col()
                        .gap(px(6.0))
                        .child(field_label(tr("modal.host.connection"), theme))
                        .child(
                            div()
                                .w_full()
                                .on_children_prepainted(move |bounds: Vec<Bounds<Pixels>>, _, _| {
                                    if let Some(first) = bounds.first().copied() {
                                        conn_anchors
                                            .borrow_mut()
                                            .insert(DropdownKind::DraftConnection, first);
                                    }
                                })
                                .child(
                                    native_dropdown(draft.connection.label_zh(), 96.0, theme)
                                        .id("btn-draft-conn")
                                        .w_full()
                                        .h(px(34.0))
                                        .on_mouse_down(
                                            MouseButton::Left,
                                            cx.listener(|this, event: &MouseDownEvent, _, cx| {
                                                this.toggle_dropdown(
                                                    DropdownKind::DraftConnection,
                                                    event.position,
                                                    132.0,
                                                );
                                                cx.notify();
                                            }),
                                        ),
                                ),
                        )
                })
                .child(
                    editable_labeled_input(
                        "input-modal-host",
                        tr("modal.host.host"),
                        &draft.hostname,
                        tr("modal.host.host_placeholder"),
                        true,
                        active_input == ActiveInput::ModalHost,
                        ActiveInput::ModalHost,
                        theme,
                        cx,
                    )
                    .flex_1(),
                )
                .child(
                    editable_labeled_input(
                        "input-modal-port",
                        tr("modal.host.port"),
                        &draft.port,
                        "22",
                        true,
                        active_input == ActiveInput::ModalPort,
                        ActiveInput::ModalPort,
                        theme,
                        cx,
                    )
                    .w(px(78.0)),
                ),
        );

        // 参照 Tabby 规范：跳板机下拉选择已添加的其他主机；SOCKS5 / HTTP 代理采用「代理主机 + 端口」双栏
        match draft.connection {
            ConnectionKind::Direct | ConnectionKind::Local => {}
            ConnectionKind::JumpHost => {
                let selected_label =
                    jump_host_label.unwrap_or_else(|| tr("modal.host.jump_host_select"));
                let jump_anchors = Rc::clone(dropdown_anchors);
                sheet = sheet.child(
                    div()
                        .w_full()
                        .flex()
                        .flex_col()
                        .gap(px(6.0))
                        .child(field_label(tr("modal.host.jump_host_label"), theme))
                        .child(
                            div()
                                .w_full()
                                .on_children_prepainted(move |bounds: Vec<Bounds<Pixels>>, _, _| {
                                    if let Some(first) = bounds.first().copied() {
                                        jump_anchors
                                            .borrow_mut()
                                            .insert(DropdownKind::DraftJumpHost, first);
                                    }
                                })
                                .child(
                                    native_dropdown(
                                        selected_label,
                                        LayoutMetrics::NEW_HOST_SHEET_WIDTH - 44.0,
                                        theme,
                                    )
                                    .id("btn-draft-jump-host")
                                    .w_full()
                                    .h(px(34.0))
                                    .on_mouse_down(
                                        MouseButton::Left,
                                        cx.listener(|this, event: &MouseDownEvent, _, cx| {
                                            this.toggle_dropdown(
                                                DropdownKind::DraftJumpHost,
                                                event.position,
                                                LayoutMetrics::NEW_HOST_SHEET_WIDTH - 44.0,
                                            );
                                            cx.notify();
                                        }),
                                    ),
                                ),
                        ),
                );
            }
            ConnectionKind::Socks5 | ConnectionKind::HttpProxy => {
                let (host_label, port_placeholder) = match draft.connection {
                    ConnectionKind::Socks5 => (tr("modal.host.socks_host"), "1080"),
                    ConnectionKind::HttpProxy => (tr("modal.host.http_host"), "8080"),
                    _ => ("", "1080"),
                };
                sheet = sheet.child(
                    div()
                        .w_full()
                        .flex()
                        .gap(px(10.0))
                        .child(
                            editable_labeled_input(
                                "input-modal-proxy-host",
                                host_label,
                                &draft.proxy_host,
                                "127.0.0.1",
                                true,
                                active_input == ActiveInput::ModalProxyHost,
                                ActiveInput::ModalProxyHost,
                                theme,
                                cx,
                            )
                            .flex_1(),
                        )
                        .child(
                            editable_labeled_input(
                                "input-modal-proxy-port",
                                tr("modal.host.proxy_port"),
                                &draft.proxy_port,
                                port_placeholder,
                                true,
                                active_input == ActiveInput::ModalProxyPort,
                                ActiveInput::ModalProxyPort,
                                theme,
                                cx,
                            )
                            .w(px(78.0)),
                        ),
                );
            }
        }

        sheet = sheet
            // 用户名
            .child(
                editable_labeled_input(
                    "input-modal-user",
                    tr("modal.host.username"),
                    &draft.username,
                    "root",
                    true,
                    active_input == ActiveInput::ModalUser,
                    ActiveInput::ModalUser,
                    theme,
                    cx,
                )
                .w_full(),
            )
            // 5 种身份验证方法
            .child(
                div()
                    .w_full()
                    .flex()
                    .flex_col()
                    .gap(px(7.0))
                    .child(field_label(tr("modal.host.auth_method"), theme))
                    .child(auth_bar),
            )
            .child(hairline_h(theme));

        // 参照 Tabby 认证联动：不同认证方式仅展示对应需要的凭据字段
        let show_password_row = matches!(
            draft.auth_method,
            AuthMethod::Auto
                | AuthMethod::Password
                | AuthMethod::PrivateKey
                | AuthMethod::KeyboardInteractive
        );
        let show_key_row = matches!(
            draft.auth_method,
            AuthMethod::Auto | AuthMethod::PrivateKey | AuthMethod::Agent
        );

        if draft.auth_method == AuthMethod::Agent {
            let agent_ok = std::env::var("SSH_AUTH_SOCK")
                .ok()
                .filter(|s| !s.trim().is_empty())
                .is_some();
            sheet = sheet.child(
                div()
                    .w_full()
                    .h(px(34.0))
                    .px(px(11.0))
                    .rounded(px(8.5))
                    .bg(theme.surface_input)
                    .border_1()
                    .border_color(theme.border_subtle)
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(
                        div()
                            .text_size(LayoutMetrics::font_control())
                            .text_color(theme.text_secondary)
                            .child(tr("modal.host.agent_status")),
                    )
                    .child(
                        div()
                            .text_size(LayoutMetrics::font_caption())
                            .text_color(if agent_ok {
                                theme.text_primary
                            } else {
                                theme.text_tertiary
                            })
                            .child(if agent_ok {
                                tr("modal.host.agent_detected")
                            } else {
                                tr("modal.host.agent_missing")
                            }),
                    ),
            );
        }

        if show_password_row {
            let pwd_label = if draft.auth_method == AuthMethod::PrivateKey {
                tr("modal.host.passphrase")
            } else {
                tr("modal.host.password")
            };
            sheet = sheet.child(
                div()
                    .w_full()
                    .flex()
                    .flex_col()
                    .gap(px(6.0))
                    .child(field_label(pwd_label, theme))
                    .child(
                        div()
                            .id("input-modal-password")
                            .w_full()
                            .h(px(34.0))
                            .px(px(11.0))
                            .flex()
                            .items_center()
                            .justify_between()
                            .rounded(px(8.5))
                            .cursor_text()
                            .bg(theme.surface_input)
                            .border_1()
                            .border_color(if pwd_focused {
                                theme.border_specular
                            } else {
                                theme.border_subtle
                            })
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(|this, ev: &MouseDownEvent, window, cx| {
                                    cx.stop_propagation();
                                    this.begin_text_input_mouse_down(
                                        ActiveInput::ModalPassword,
                                        ev,
                                        window,
                                    );
                                    cx.notify();
                                }),
                            )
                            .child(pwd_left)
                            .child(
                                div()
                                    .id("btn-toggle-pwd-eye")
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
                                            this.new_host_draft.show_password =
                                                !this.new_host_draft.show_password;
                                            cx.notify();
                                        }),
                                    )
                                    .child(icon_svg("icons/eye.svg", 14.0, theme.text_tertiary)),
                            ),
                    ),
            );
        }

        if show_key_row {
            let key_title = if draft.auth_method == AuthMethod::Agent {
                tr("modal.host.agent_key_hint")
            } else {
                tr("modal.host.private_key")
            };
            sheet = sheet.child(
                div()
                    .w_full()
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(
                        div()
                            .id("input-modal-keypath")
                            .flex_1()
                            .min_w_0()
                            .pr(px(10.0))
                            .flex()
                            .flex_col()
                            .gap(px(2.0))
                            .cursor_text()
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(|this, ev: &MouseDownEvent, window, cx| {
                                    cx.stop_propagation();
                                    this.begin_text_input_mouse_down(
                                        ActiveInput::ModalKeyPath,
                                        ev,
                                        window,
                                    );
                                    cx.notify();
                                }),
                            )
                            .child(
                                div()
                                    .text_size(LayoutMetrics::font_body())
                                    .text_color(theme.text_primary)
                                    .child(key_title),
                            )
                            .child({
                                let key_focused = active_input == ActiveInput::ModalKeyPath;
                                div()
                                    .w_full()
                                    .overflow_hidden()
                                    .flex()
                                    .items_center()
                                    .child(editable_text_line(
                                        ActiveInput::ModalKeyPath,
                                        &draft.private_key_path,
                                        tr("modal.host.no_private_key"),
                                        false,
                                        key_focused,
                                        LayoutMetrics::font_micro(),
                                        12.0,
                                        if key_focused {
                                            theme.text_primary
                                        } else {
                                            theme.text_tertiary
                                        },
                                        theme.text_tertiary,
                                        None,
                                        theme,
                                    ))
                            }),
                    )
                    .child(
                        div()
                            .id("btn-pick-private-key")
                            .flex_none()
                            .h(px(30.0))
                            .px(px(12.0))
                            .flex()
                            .items_center()
                            .gap(px(6.0))
                            .rounded(px(8.0))
                            .bg(theme.surface_pill)
                            .border_1()
                            .border_color(theme.border_subtle)
                            .cursor_pointer()
                            .hover(|s| {
                                s.bg(theme.surface_active)
                                    .border_color(theme.border_specular)
                            })
                            .active(|s| s.opacity(0.76))
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.prompt_private_key_file(cx);
                            }))
                            .child(icon_svg("icons/folder.svg", 13.5, theme.text_secondary))
                            .child(
                                div()
                                    .text_size(LayoutMetrics::font_control())
                                    .text_color(theme.text_primary)
                                    .child(tr("modal.host.add_private_key")),
                            ),
                    ),
            );
        }
    }

    // 底部配套磨砂按钮（取消 / 保存并连接）
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
                    .id("btn-modal-cancel")
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.close_new_host_modal();
                        cx.notify();
                    })),
            )
            .child(
                glass_pill_button(
                    if is_editing {
                        tr("modal.host.save_changes")
                    } else {
                        tr("modal.host.save_and_connect")
                    },
                    true,
                    theme,
                )
                .id("btn-modal-save")
                .on_click(cx.listener(|this, _, _, cx| {
                    this.save_and_connect_draft();
                    cx.notify();
                })),
            ),
    )
}

fn field_label(text: &str, theme: &ThemeTokens) -> Div {
    div()
        .text_size(LayoutMetrics::font_control())
        .text_color(theme.text_secondary)
        .child(text.to_string())
}

#[allow(clippy::too_many_arguments)]
fn editable_labeled_input(
    id: &'static str,
    label: &str,
    value: &str,
    placeholder: &str,
    _mono: bool,
    focused: bool,
    target: ActiveInput,
    theme: &ThemeTokens,
    cx: &mut Context<HoloSshApp>,
) -> Div {
    let box_el = div()
        .id(id)
        .w_full()
        .h(px(34.0))
        .px(px(11.0))
        .overflow_hidden()
        .flex()
        .items_center()
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
        .child(editable_text_line(
            target,
            value,
            placeholder,
            false,
            focused,
            LayoutMetrics::font_body(),
            14.0,
            theme.text_primary,
            theme.text_tertiary,
            None,
            theme,
        ));

    div()
        .flex()
        .flex_col()
        .gap(px(6.0))
        .child(field_label(label, theme))
        .child(box_el)
}
