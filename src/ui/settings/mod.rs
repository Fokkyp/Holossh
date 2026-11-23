//! 设置页视图（严格对应 Figma 06 画板最终版）
//!
//! 左侧导航严格分组：通用（外观 / 终端 / 快捷键）、连接（连接，无命令片段）、安全与同步（密钥库 / 同步）、关于。
//! 右侧支持平滑纵向滚动，全部 7 个模块的每一个控件均连接至真实状态持久化、系统 API 或协议参数。

use crate::app::{ActiveInput, DropdownKind, HoloSshApp, VaultSettingsForm};
use crate::i18n::{tr, trf};
use crate::services::sync::SyncAction;
use crate::services::vault::VaultSecretEntry;
use crate::state::settings::{
    AppSettings, CursorStyle, GlassScope, SettingsSection, SettingsSubPage, ShortcutAction,
    SidebarSide, SyncProvider,
};
use crate::theme::{AppearanceMode, LayoutMetrics, ThemeTokens};
use crate::ui::components::{
    editable_text_line, glass_pill_button, hairline_h, icon_svg, module_loading_overlay,
    native_dropdown, native_toggle_with_id,
};
use gpui::{
    Animation, AnimationExt, AnyElement, Context, Div, InteractiveElement, IntoElement,
    MouseButton, MouseDownEvent, ParentElement, SharedString, StatefulInteractiveElement, Styled,
    div, ease_out_quint, img, px,
};
use std::path::PathBuf;
use std::time::Duration;

#[allow(clippy::too_many_arguments)]
pub fn render_settings_view(
    settings: &AppSettings,
    active_section: SettingsSection,
    active_subpage: Option<SettingsSubPage>,
    vault_exists: bool,
    vault_unlocked: bool,
    vault_entries: &[VaultSecretEntry],
    vault_revealed_host_id: Option<&str>,
    vault_form: &VaultSettingsForm,
    vault_local_keys: &[PathBuf],
    total_hosts: usize,
    active_input: ActiveInput,
    recording_shortcut: Option<ShortcutAction>,
    sync_show_secret: bool,
    sync_in_progress: bool,
    theme: &ThemeTokens,
    cx: &mut Context<HoloSshApp>,
) -> impl IntoElement {
    let lang = settings.language;

    let sidebar = div()
        .relative()
        .flex_none()
        .w(px(settings.sidebar_width.clamp(190.0, 440.0)))
        .h_full()
        .overflow_hidden()
        .flex()
        .flex_col()
        .bg(theme.sidebar_bg)
        .border_r_1()
        .border_color(theme.border_subtle)
        // 红黄绿预留行（44px）
        .child(div().w_full().h(px(44.0)))
        // `< 设置` 返回行（紧接红黄绿下方左对齐，带悬停微反馈）
        .child(
            div()
                .id("btn-settings-back")
                .mx(px(8.0))
                .mb(px(10.0))
                .px(px(6.0))
                .py(px(6.0))
                .rounded(px(8.0))
                .flex()
                .items_center()
                .gap(px(8.0))
                .cursor_pointer()
                .hover(|s| s.bg(theme.surface_pill))
                .active(|s| s.opacity(0.72))
                .on_click(cx.listener(|this, _, _, cx| {
                    this.close_settings();
                    cx.notify();
                }))
                .child(icon_svg(
                    "icons/chevron-left.svg",
                    14.0,
                    theme.sidebar_text_secondary,
                ))
                .child(
                    div()
                        .text_size(LayoutMetrics::font_title())
                        .text_color(theme.sidebar_text_primary)
                        .child(lang.t("settings.title")),
                ),
        )
        // 分组导航（通用 / 连接 / 安全与同步 / 关于，无命令片段）
        .child(
            div()
                .flex_1()
                .w_full()
                .px(px(12.0))
                .flex()
                .flex_col()
                .gap(px(18.0))
                .child(nav_group(
                    Some(lang.t("settings.group.general")),
                    &[
                        SettingsSection::Appearance,
                        SettingsSection::Terminal,
                        SettingsSection::Shortcuts,
                    ],
                    active_section,
                    lang,
                    theme,
                    cx,
                ))
                .child(nav_group(
                    Some(lang.t("settings.group.connection")),
                    &[SettingsSection::Connections],
                    active_section,
                    lang,
                    theme,
                    cx,
                ))
                .child(nav_group(
                    Some(lang.t("settings.group.security_sync")),
                    &[SettingsSection::Vault, SettingsSection::Sync],
                    active_section,
                    lang,
                    theme,
                    cx,
                ))
                .child(nav_group(
                    None,
                    &[SettingsSection::About],
                    active_section,
                    lang,
                    theme,
                    cx,
                )),
        )
        .child(
            div()
                .id("settings-sidebar-resize-handle")
                .absolute()
                .top_0()
                .bottom_0()
                .right_0()
                .w(px(7.0))
                .cursor(gpui::CursorStyle::ResizeLeftRight)
                .hover(|s| s.bg(theme.surface_pill))
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, event: &MouseDownEvent, _window, cx| {
                        this.start_drag_sidebar(event.position);
                        cx.stop_propagation();
                    }),
                ),
        );

    let section_body: AnyElement = match active_section {
        SettingsSection::Appearance => {
            render_appearance_section(settings, theme, cx).into_any_element()
        }
        SettingsSection::Terminal => {
            render_terminal_section(settings, theme, cx).into_any_element()
        }
        SettingsSection::Shortcuts => {
            render_shortcuts_section(settings, recording_shortcut, theme, cx).into_any_element()
        }
        SettingsSection::Connections => {
            render_connections_section(settings, total_hosts, theme, cx).into_any_element()
        }
        SettingsSection::Vault => render_vault_section(
            settings,
            active_subpage,
            vault_exists,
            vault_unlocked,
            vault_entries,
            vault_revealed_host_id,
            vault_form,
            vault_local_keys,
            active_input,
            theme,
            cx,
        )
        .into_any_element(),
        SettingsSection::Sync => render_sync_section(
            settings,
            active_subpage,
            total_hosts,
            active_input,
            sync_show_secret,
            sync_in_progress,
            theme,
            cx,
        )
        .into_any_element(),
        SettingsSection::About => render_about_section(theme, cx).into_any_element(),
    };

    let is_centered_vault_gate = active_section == SettingsSection::Vault
        && active_subpage.is_none()
        && (!vault_exists || !vault_unlocked);

    let mut scroll_container = if is_centered_vault_gate {
        div()
            .id("settings-scroll-body")
            .flex_1()
            .w_full()
            .px(px(36.0))
            .py(px(24.0))
            .flex()
            .flex_col()
            .gap(px(16.0))
    } else {
        div()
            .id("settings-scroll-body")
            .flex_1()
            .w_full()
            .overflow_y_scroll()
            .px(px(36.0))
            .py(px(24.0))
            .flex()
            .flex_col()
            .gap(px(20.0))
    };

    // 切换设置子模块或进出二级页面时 180ms 平滑淡入与微上浮过渡
    let body_box = if is_centered_vault_gate {
        div()
    } else {
        div().w_full()
    };
    let anim_id = match active_subpage {
        Some(sub) => format!("settings-sub-{}", sub.anim_key()),
        None => format!("settings-sec-{}", active_section as usize),
    };
    let animated_body = body_box
        .child(section_body)
        .with_animation(
            SharedString::from(anim_id),
            Animation::new(Duration::from_millis(185)).with_easing(ease_out_quint()),
            |el, d| el.opacity(0.15 + 0.85 * d).mt(px(8.0 * (1.0 - d))),
        );

    scroll_container = if is_centered_vault_gate {
        scroll_container.child(
            div()
                .flex_1()
                .w_full()
                .pb(px(32.0))
                .flex()
                .items_center()
                .justify_center()
                .child(animated_body),
        )
    } else {
        scroll_container.child(animated_body)
    };

    let mut header = div()
        .w_full()
        .h(px(LayoutMetrics::TITLEBAR_HEIGHT))
        .px(px(if active_subpage.is_some() { 20.0 } else { 32.0 }))
        .flex()
        .items_center()
        .gap(px(6.0))
        .border_b_1()
        .border_color(theme.border_subtle);

    if let Some(sub) = active_subpage {
        header = header
            .child(
                div()
                    .id("btn-settings-subpage-back")
                    .w(px(28.0))
                    .h(px(28.0))
                    .rounded(px(7.0))
                    .flex()
                    .items_center()
                    .justify_center()
                    .cursor_pointer()
                    .hover(|s| s.bg(theme.surface_pill))
                    .active(|s| s.opacity(0.70))
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.pop_settings_subpage();
                        cx.notify();
                    }))
                    .child(icon_svg("icons/chevron-left.svg", 15.0, theme.text_secondary)),
            )
            .child(
                div()
                    .text_size(LayoutMetrics::font_title())
                    .text_color(theme.text_primary)
                    .child(sub.title()),
            );
    } else {
        header = header.child(
            div()
                .text_size(LayoutMetrics::font_title())
                .text_color(theme.text_primary)
                .child(active_section.title(lang)),
        );
    }

    let content = div()
        .flex_1()
        .h_full()
        .flex()
        .flex_col()
        .bg(theme.terminal_bg)
        .child(header)
        .child(scroll_container);

    div()
        .size_full()
        .flex()
        .flex_row()
        .child(sidebar)
        .child(content)
}

fn render_appearance_section(
    settings: &AppSettings,
    theme: &ThemeTokens,
    cx: &mut Context<HoloSshApp>,
) -> impl IntoElement {
    let appearance_seg = div()
        .p(px(2.0))
        .flex()
        .items_center()
        .gap(px(2.0))
        .rounded(LayoutMetrics::radius_pill())
        .bg(theme.surface_input)
        .border_1()
        .border_color(theme.border_subtle)
        .child(seg_pill(
            "settings-app-sys",
            tr("settings.appearance.system"),
            settings.appearance == AppearanceMode::System,
            AppearanceMode::System,
            theme,
            cx,
        ))
        .child(seg_pill(
            "settings-app-dark",
            tr("settings.appearance.dark"),
            settings.appearance == AppearanceMode::Dark,
            AppearanceMode::Dark,
            theme,
            cx,
        ))
        .child(seg_pill(
            "settings-app-light",
            tr("settings.appearance.light"),
            settings.appearance == AppearanceMode::Light,
            AppearanceMode::Light,
            theme,
            cx,
        ));

    let sidebar_seg = div()
        .p(px(2.0))
        .flex()
        .items_center()
        .gap(px(2.0))
        .rounded(LayoutMetrics::radius_pill())
        .bg(theme.surface_input)
        .border_1()
        .border_color(theme.border_subtle)
        .child(sidebar_side_pill(
            "side-left",
            tr("settings.appearance.side_left"),
            settings.sidebar_side == SidebarSide::Left,
            SidebarSide::Left,
            theme,
            cx,
        ))
        .child(sidebar_side_pill(
            "side-right",
            tr("settings.appearance.side_right"),
            settings.sidebar_side == SidebarSide::Right,
            SidebarSide::Right,
            theme,
            cx,
        ));

    let glass_scope_seg = div()
        .p(px(2.0))
        .flex()
        .items_center()
        .gap(px(2.0))
        .rounded(LayoutMetrics::radius_pill())
        .bg(theme.surface_input)
        .border_1()
        .border_color(theme.border_subtle)
        .child(glass_scope_pill(
            "glass-scope-left",
            tr("settings.appearance.glass_left"),
            settings.glass_scope == GlassScope::LeftOnly,
            GlassScope::LeftOnly,
            theme,
            cx,
        ))
        .child(glass_scope_pill(
            "glass-scope-right",
            tr("settings.appearance.glass_right"),
            settings.glass_scope == GlassScope::RightOnly,
            GlassScope::RightOnly,
            theme,
            cx,
        ))
        .child(glass_scope_pill(
            "glass-scope-all",
            tr("settings.appearance.glass_all"),
            settings.glass_scope == GlassScope::AllWindow,
            GlassScope::AllWindow,
            theme,
            cx,
        ));

    div()
        .w_full()
        .flex()
        .flex_col()
        .gap(px(22.0))
        .child(settings_card(
            tr("settings.appearance.card_theme"),
            vec![
                setting_row(
                    tr("settings.appearance.mode"),
                    None,
                    appearance_seg.into_any_element(),
                    theme,
                ),
                setting_row(
                    tr("settings.appearance.language"),
                    None,
                    native_dropdown(settings.language.display_name(), 132.0, theme)
                        .id("btn-settings-lang")
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|this, event: &MouseDownEvent, _, cx| {
                                this.toggle_dropdown(
                                    DropdownKind::Language,
                                    event.position,
                                    144.0,
                                );
                                cx.notify();
                            }),
                        )
                        .into_any_element(),
                    theme,
                ),
            ],
            theme,
        ))
        .child(settings_card(
            tr("settings.appearance.card_glass"),
            vec![
                setting_row(
                    tr("settings.appearance.glass_enable"),
                    None,
                    native_toggle_with_id("glass", settings.glass_enabled, theme)
                        .id("toggle-glass")
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.settings.glass_enabled = !this.settings.glass_enabled;
                            this.settings.save();
                            cx.notify();
                        }))
                        .into_any_element(),
                    theme,
                ),
                setting_row(
                    tr("settings.appearance.glass_scope"),
                    None,
                    glass_scope_seg.into_any_element(),
                    theme,
                ),
                setting_row(
                    tr("settings.appearance.opacity"),
                    None,
                    render_interactive_slider(
                        "opacity",
                        &[0.40, 0.45, 0.50, 0.55, 0.60, 0.65, 0.70, 0.75, 0.80, 0.85, 0.90, 0.95],
                        settings.terminal_opacity,
                        format!("{}%", (settings.terminal_opacity * 100.0).round() as u32),
                        theme,
                        |this, val| this.set_terminal_opacity(val),
                        |this| this.adjust_terminal_opacity(-0.05),
                        |this| this.adjust_terminal_opacity(0.05),
                        cx,
                    )
                    .into_any_element(),
                    theme,
                ),
                setting_row(
                    tr("settings.appearance.blur_radius"),
                    None,
                    render_interactive_slider(
                        "blur",
                        &[16.0, 20.0, 24.0, 28.0, 32.0, 36.0, 40.0, 44.0, 48.0, 52.0, 56.0, 60.0],
                        settings.blur_radius,
                        format!("{} px", settings.blur_radius.round() as u32),
                        theme,
                        |this, val| this.set_blur_radius(val),
                        |this| this.adjust_blur_radius(-4.0),
                        |this| this.adjust_blur_radius(4.0),
                        cx,
                    )
                    .into_any_element(),
                    theme,
                ),
            ],
            theme,
        ))
        .child(settings_card(
            tr("settings.appearance.card_layout"),
            vec![
                setting_row(
                    tr("settings.appearance.sidebar_side"),
                    None,
                    sidebar_seg.into_any_element(),
                    theme,
                ),
                setting_row(
                    tr("settings.appearance.compact_list"),
                    None,
                    native_toggle_with_id("compact", settings.compact_host_list, theme)
                        .id("toggle-compact")
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.settings.compact_host_list = !this.settings.compact_host_list;
                            this.settings.save();
                            cx.notify();
                        }))
                        .into_any_element(),
                    theme,
                ),
                setting_row(
                    tr("settings.appearance.show_groups"),
                    None,
                    native_toggle_with_id("groups", settings.show_groups, theme)
                        .id("toggle-groups")
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.settings.show_groups = !this.settings.show_groups;
                            this.settings.save();
                            cx.notify();
                        }))
                        .into_any_element(),
                    theme,
                ),
            ],
            theme,
        ))
}

fn render_terminal_section(
    settings: &AppSettings,
    theme: &ThemeTokens,
    cx: &mut Context<HoloSshApp>,
) -> impl IntoElement {
    let cursor_seg = div()
        .p(px(2.0))
        .flex()
        .items_center()
        .gap(px(2.0))
        .rounded(LayoutMetrics::radius_pill())
        .bg(theme.surface_input)
        .border_1()
        .border_color(theme.border_subtle)
        .child(cursor_style_pill(
            "cur-block",
            CursorStyle::Block,
            settings.cursor_style == CursorStyle::Block,
            theme,
            cx,
        ))
        .child(cursor_style_pill(
            "cur-beam",
            CursorStyle::Beam,
            settings.cursor_style == CursorStyle::Beam,
            theme,
            cx,
        ))
        .child(cursor_style_pill(
            "cur-under",
            CursorStyle::Underline,
            settings.cursor_style == CursorStyle::Underline,
            theme,
            cx,
        ));

    div()
        .w_full()
        .flex()
        .flex_col()
        .gap(px(22.0))
        .child(settings_card(
            tr("settings.terminal.card_font"),
            vec![
                setting_row(
                    tr("settings.terminal.font_family"),
                    None,
                    native_dropdown(
                        crate::services::fonts::display_name_for_setting(
                            &settings.terminal_font_family,
                        ),
                        164.0,
                        theme,
                    )
                    .id("btn-term-font")
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, event: &MouseDownEvent, _, cx| {
                            this.toggle_dropdown(
                                DropdownKind::TerminalFont,
                                event.position,
                                188.0,
                            );
                            cx.notify();
                        }),
                    )
                    .into_any_element(),
                    theme,
                ),
                setting_row(
                    tr("settings.terminal.font_size"),
                    None,
                    render_interactive_slider(
                        "fontsize",
                        &[11.0, 12.0, 13.0, 14.0, 15.0, 16.0, 17.0, 18.0],
                        settings.terminal_font_size,
                        format!("{} px", settings.terminal_font_size.round() as u32),
                        theme,
                        |this, val| this.set_font_size(val),
                        |this| this.adjust_font_size(-1.0),
                        |this| this.adjust_font_size(1.0),
                        cx,
                    )
                    .into_any_element(),
                    theme,
                ),
                setting_row(
                    tr("settings.terminal.cursor_style"),
                    None,
                    cursor_seg.into_any_element(),
                    theme,
                ),
            ],
            theme,
        ))
        .child(settings_card(
            tr("settings.terminal.card_buffer"),
            vec![
                setting_row(
                    tr("settings.terminal.scrollback"),
                    None,
                    native_dropdown(
                        &trf(
                            "settings.terminal.lines_unit",
                            &[("n", &settings.scrollback_lines.to_string())],
                        ),
                        120.0,
                        theme,
                    )
                    .id("btn-scrollback")
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, event: &MouseDownEvent, _, cx| {
                            this.toggle_dropdown(
                                DropdownKind::ScrollbackLines,
                                event.position,
                                136.0,
                            );
                            cx.notify();
                        }),
                    )
                    .into_any_element(),
                    theme,
                ),
                setting_row(
                    tr("settings.terminal.copy_on_select"),
                    None,
                    native_toggle_with_id("copy-select", settings.copy_on_select, theme)
                        .id("toggle-copy-on-select")
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.settings.copy_on_select = !this.settings.copy_on_select;
                            this.settings.save();
                            cx.notify();
                        }))
                        .into_any_element(),
                    theme,
                ),
                setting_row(
                    tr("settings.terminal.trim_paste"),
                    None,
                    native_toggle_with_id("trim-paste", settings.trim_paste_newlines, theme)
                        .id("toggle-trim-paste")
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.settings.trim_paste_newlines = !this.settings.trim_paste_newlines;
                            this.settings.save();
                            cx.notify();
                        }))
                        .into_any_element(),
                    theme,
                ),
            ],
            theme,
        ))
}

fn render_shortcuts_section(
    settings: &AppSettings,
    recording_shortcut: Option<ShortcutAction>,
    theme: &ThemeTokens,
    cx: &mut Context<HoloSshApp>,
) -> impl IntoElement {
    let mut shortcut_rows: Vec<Div> = Vec::with_capacity(ShortcutAction::ALL.len() + 1);
    for action in ShortcutAction::ALL {
        let is_recording = recording_shortcut == Some(action);
        let label = if is_recording {
            tr("settings.shortcuts.recording").to_string()
        } else {
            settings.shortcuts.display_for(action)
        };
        shortcut_rows.push(editable_shortcut_row(
            tr(action.i18n_key()),
            action,
            &label,
            is_recording,
            theme,
            cx,
        ));
    }
    shortcut_rows.push(shortcut_row(tr("settings.shortcuts.esc"), "Esc", theme));

    div()
        .w_full()
        .flex()
        .flex_col()
        .gap(px(22.0))
        .child(settings_card(
            tr("settings.shortcuts.card_ctrl"),
            vec![
                setting_row(
                    tr("settings.shortcuts.enable"),
                    None,
                    native_toggle_with_id("shortcuts", settings.shortcuts_enabled, theme)
                        .id("toggle-shortcuts-enabled")
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.settings.shortcuts_enabled = !this.settings.shortcuts_enabled;
                            this.settings.save();
                            cx.notify();
                        }))
                        .into_any_element(),
                    theme,
                ),
                setting_row(
                    tr("settings.shortcuts.reset_title"),
                    None,
                    glass_pill_button(tr("settings.shortcuts.reset_btn"), false, theme)
                        .id("btn-reset-shortcuts")
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|this, _: &MouseDownEvent, _, cx| {
                                cx.stop_propagation();
                                this.reset_shortcuts_to_default();
                                cx.notify();
                            }),
                        )
                        .into_any_element(),
                    theme,
                ),
            ],
            theme,
        ))
        .child(settings_card(
            tr("settings.shortcuts.card_list"),
            shortcut_rows,
            theme,
        ))
}

fn editable_shortcut_row(
    title: &str,
    action: ShortcutAction,
    key_label: &str,
    is_recording: bool,
    theme: &ThemeTokens,
    cx: &mut Context<HoloSshApp>,
) -> Div {
    let pill = div()
        .id(SharedString::from(format!("shortcut-cap-{}", action.id_slug())))
        .min_w(px(46.0))
        .px(px(10.0))
        .py(px(4.0))
        .rounded(px(6.5))
        .flex()
        .items_center()
        .justify_center()
        .cursor_pointer()
        .bg(if is_recording {
            theme.surface_active
        } else {
            theme.surface_pill
        })
        .border_1()
        .border_color(if is_recording {
            theme.text_primary
        } else {
            theme.border_subtle
        })
        .hover(|s| {
            s.bg(theme.surface_active).border_color(if is_recording {
                theme.text_primary
            } else {
                theme.border_specular
            })
        })
        .active(|s| s.opacity(0.78))
        .text_size(LayoutMetrics::font_micro())
        .text_color(if is_recording {
            theme.text_primary
        } else {
            theme.text_secondary
        })
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(move |this, _: &MouseDownEvent, window, cx| {
                cx.stop_propagation();
                window.focus(&this.focus_handle);
                this.toggle_recording_shortcut(action);
                cx.notify();
            }),
        )
        .child(key_label.to_string());

    setting_row(title, None, pill.into_any_element(), theme)
}

fn shortcut_row(title: &str, key_label: &str, theme: &ThemeTokens) -> Div {
    setting_row(
        title,
        None,
        div()
            .min_w(px(46.0))
            .px(px(10.0))
            .py(px(4.0))
            .rounded(px(6.5))
            .flex()
            .items_center()
            .justify_center()
            .bg(theme.surface_pill)
            .border_1()
            .border_color(theme.border_subtle)
            .text_size(LayoutMetrics::font_micro())
            .text_color(theme.text_tertiary)
            .child(key_label.to_string())
            .into_any_element(),
        theme,
    )
}

fn render_connections_section(
    settings: &AppSettings,
    total_hosts: usize,
    theme: &ThemeTokens,
    cx: &mut Context<HoloSshApp>,
) -> impl IntoElement {
    let ka_label = if settings.keepalive_secs == 0 {
        tr("common.off").to_string()
    } else {
        trf(
            "settings.conn.every_secs",
            &[("s", &settings.keepalive_secs.to_string())],
        )
    };

    div()
        .w_full()
        .flex()
        .flex_col()
        .gap(px(22.0))
        .child(settings_card(
            tr("settings.conn.card_ssh"),
            vec![
                setting_row(
                    tr("settings.conn.keepalive"),
                    None,
                    native_dropdown(&ka_label, 124.0, theme)
                        .id("btn-keepalive-cycle")
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|this, event: &MouseDownEvent, _, cx| {
                                this.toggle_dropdown(
                                    DropdownKind::KeepAlive,
                                    event.position,
                                    138.0,
                                );
                                cx.notify();
                            }),
                        )
                        .into_any_element(),
                    theme,
                ),
                setting_row(
                    tr("settings.conn.timeout"),
                    None,
                    native_dropdown(
                        &trf(
                            "settings.conn.secs_unit",
                            &[("s", &settings.connect_timeout_secs.to_string())],
                        ),
                        124.0,
                        theme,
                    )
                    .id("btn-timeout-cycle")
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, event: &MouseDownEvent, _, cx| {
                            this.toggle_dropdown(
                                DropdownKind::ConnectTimeout,
                                event.position,
                                136.0,
                            );
                            cx.notify();
                        }),
                    )
                    .into_any_element(),
                    theme,
                ),
                setting_row(
                    tr("settings.conn.default_user"),
                    None,
                    native_dropdown(&settings.default_username, 124.0, theme)
                        .id("btn-default-user-cycle")
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|this, event: &MouseDownEvent, _, cx| {
                                this.toggle_dropdown(
                                    DropdownKind::DefaultUsername,
                                    event.position,
                                    138.0,
                                );
                                cx.notify();
                            }),
                        )
                        .into_any_element(),
                    theme,
                ),
                setting_row(
                    tr("settings.conn.default_port"),
                    None,
                    native_dropdown(&settings.default_port.to_string(), 124.0, theme)
                        .id("btn-default-port-cycle")
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|this, event: &MouseDownEvent, _, cx| {
                                this.toggle_dropdown(
                                    DropdownKind::DefaultPort,
                                    event.position,
                                    132.0,
                                );
                                cx.notify();
                            }),
                        )
                        .into_any_element(),
                    theme,
                ),
                setting_row(
                    tr("settings.conn.auto_hostkey"),
                    None,
                    native_toggle_with_id("hostkey", settings.auto_accept_host_key, theme)
                        .id("toggle-auto-hostkey")
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.settings.auto_accept_host_key =
                                !this.settings.auto_accept_host_key;
                            this.settings.save();
                            cx.notify();
                        }))
                        .into_any_element(),
                    theme,
                ),
            ],
            theme,
        ))
        .child(settings_card(
            tr("settings.conn.card_ssh_config"),
            vec![setting_row(
                tr("settings.conn.import_title"),
                Some(&trf(
                    "settings.conn.import_sub",
                    &[("count", &total_hosts.to_string())],
                )),
                glass_pill_button(tr("settings.conn.import_btn"), false, theme)
                    .id("btn-reimport-ssh-config")
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.import_ssh_config_now();
                        cx.notify();
                    }))
                    .into_any_element(),
                theme,
            )],
            theme,
        ))
}

#[allow(clippy::too_many_arguments)]
fn render_vault_section(
    settings: &AppSettings,
    active_subpage: Option<SettingsSubPage>,
    vault_exists: bool,
    vault_unlocked: bool,
    vault_entries: &[VaultSecretEntry],
    vault_revealed_host_id: Option<&str>,
    vault_form: &VaultSettingsForm,
    vault_local_keys: &[PathBuf],
    active_input: ActiveInput,
    theme: &ThemeTokens,
    cx: &mut Context<HoloSshApp>,
) -> impl IntoElement {
    // 1. 首次进入尚未创建密钥库：居中极简设置主密码视图
    if !vault_exists {
        return div()
            .w(px(300.0))
            .flex()
            .flex_col()
            .items_center()
            .gap(px(12.0))
            .child(
                div()
                    .w_full()
                    .mb(px(6.0))
                    .flex()
                    .flex_col()
                    .items_center()
                    .gap(px(6.0))
                    .child(
                        div()
                            .text_size(LayoutMetrics::font_hero())
                            .text_color(theme.text_primary)
                            .child(tr("modal.vault.title_setup")),
                    )
                    .child(
                        div()
                            .text_size(LayoutMetrics::font_control())
                            .text_color(theme.text_tertiary)
                            .child(tr("modal.vault.subtitle_setup")),
                    ),
            )
            .child(vault_centered_secret_input(
                "input-vault-setup-pwd",
                &vault_form.setup_password,
                tr("modal.vault.placeholder_pwd"),
                vault_form.show_passwords,
                active_input == ActiveInput::VaultSetupPassword,
                ActiveInput::VaultSetupPassword,
                theme,
                cx,
            ))
            .child(vault_centered_secret_input(
                "input-vault-setup-confirm",
                &vault_form.setup_confirm,
                tr("modal.vault.placeholder_confirm"),
                vault_form.show_passwords,
                active_input == ActiveInput::VaultSetupConfirm,
                ActiveInput::VaultSetupConfirm,
                theme,
                cx,
            ))
            .child(
                div()
                    .id("btn-vault-create-inline")
                    .mt(px(4.0))
                    .w_full()
                    .h(px(34.0))
                    .rounded(px(9.0))
                    .flex()
                    .items_center()
                    .justify_center()
                    .cursor_pointer()
                    .bg(theme.surface_active)
                    .border_1()
                    .border_color(theme.border_specular)
                    .text_size(LayoutMetrics::font_body())
                    .text_color(theme.text_primary)
                    .hover(|s| s.bg(theme.surface_pill))
                    .active(|s| s.opacity(0.75))
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.submit_settings_vault_setup();
                        cx.notify();
                    }))
                    .child(tr("settings.vault.setup_btn")),
            );
    }

    // 2. 已存在密钥库但当前处于锁定态：居中极简解锁视图
    if !vault_unlocked {
        return div()
            .w(px(300.0))
            .flex()
            .flex_col()
            .items_center()
            .gap(px(12.0))
            .child(
                div()
                    .w_full()
                    .mb(px(6.0))
                    .flex()
                    .flex_col()
                    .items_center()
                    .gap(px(6.0))
                    .child(
                        div()
                            .text_size(LayoutMetrics::font_hero())
                            .text_color(theme.text_primary)
                            .child(tr("modal.vault.title_unlock")),
                    )
                    .child(
                        div()
                            .text_size(LayoutMetrics::font_control())
                            .text_color(theme.text_tertiary)
                            .child(tr("settings.vault.unlock_sub")),
                    ),
            )
            .child(vault_centered_secret_input(
                "input-vault-unlock-pwd",
                &vault_form.unlock_password,
                tr("modal.vault.placeholder_pwd"),
                vault_form.show_passwords,
                active_input == ActiveInput::VaultUnlockPassword,
                ActiveInput::VaultUnlockPassword,
                theme,
                cx,
            ))
            .child(
                div()
                    .id("btn-vault-unlock-inline")
                    .mt(px(4.0))
                    .w_full()
                    .h(px(34.0))
                    .rounded(px(9.0))
                    .flex()
                    .items_center()
                    .justify_center()
                    .cursor_pointer()
                    .bg(theme.surface_active)
                    .border_1()
                    .border_color(theme.border_specular)
                    .text_size(LayoutMetrics::font_body())
                    .text_color(theme.text_primary)
                    .hover(|s| s.bg(theme.surface_pill))
                    .active(|s| s.opacity(0.75))
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.submit_settings_vault_unlock();
                        cx.notify();
                    }))
                    .child(tr("settings.vault.unlock_btn")),
            );
    }

    // 3. 已解锁状态：二级子页面按需渲染（macOS 系统设置风格）
    let agent_online = std::env::var("SSH_AUTH_SOCK")
        .map(|s| !s.is_empty() && std::path::Path::new(&s).exists())
        .unwrap_or(false);

    match active_subpage {
        Some(SettingsSubPage::VaultChangePassword) => {
            let change_pwd_rows = vec![
                setting_row(
                    tr("settings.vault.old_pwd"),
                    None,
                    vault_secret_input(
                        "input-vault-old-pwd",
                        &vault_form.change_old,
                        tr("settings.vault.old_pwd_ph"),
                        vault_form.show_passwords,
                        active_input == ActiveInput::VaultChangeOld,
                        ActiveInput::VaultChangeOld,
                        theme,
                        cx,
                    )
                    .into_any_element(),
                    theme,
                ),
                setting_row(
                    tr("settings.vault.new_pwd"),
                    None,
                    vault_secret_input(
                        "input-vault-new-pwd",
                        &vault_form.change_new,
                        tr("settings.vault.new_pwd_ph"),
                        vault_form.show_passwords,
                        active_input == ActiveInput::VaultChangeNew,
                        ActiveInput::VaultChangeNew,
                        theme,
                        cx,
                    )
                    .into_any_element(),
                    theme,
                ),
                setting_row(
                    tr("settings.vault.confirm_pwd"),
                    None,
                    vault_secret_input(
                        "input-vault-new-confirm",
                        &vault_form.change_confirm,
                        tr("settings.vault.confirm_pwd_ph"),
                        vault_form.show_passwords,
                        active_input == ActiveInput::VaultChangeConfirm,
                        ActiveInput::VaultChangeConfirm,
                        theme,
                        cx,
                    )
                    .into_any_element(),
                    theme,
                ),
                setting_row(
                    tr("settings.vault.reencrypt"),
                    None,
                    glass_pill_button(tr("settings.vault.change_pwd_btn"), true, theme)
                        .id("btn-vault-change-pwd")
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.submit_change_vault_password();
                            cx.notify();
                        }))
                        .into_any_element(),
                    theme,
                ),
            ];

            return div()
                .w_full()
                .flex()
                .flex_col()
                .gap(px(22.0))
                .child(settings_card(
                    tr("settings.vault.card_change_pwd"),
                    change_pwd_rows,
                    theme,
                ));
        }
        Some(SettingsSubPage::VaultHostSecrets) => {
            let mut host_secret_rows = Vec::new();
            if vault_entries.is_empty() {
                host_secret_rows.push(setting_row(
                    tr("settings.vault.no_host_secrets"),
                    None,
                    div()
                        .text_size(LayoutMetrics::font_caption())
                        .text_color(theme.text_tertiary)
                        .child(trf("settings.vault.items_count", &[("count", "0")]))
                        .into_any_element(),
                    theme,
                ));
            } else {
                for (idx, entry) in vault_entries.iter().enumerate() {
                    let hid_toggle = entry.host_id.clone();
                    let hid_del = entry.host_id.clone();
                    let hname_del = entry.host_name.clone();
                    let is_revealed = vault_revealed_host_id == Some(entry.host_id.as_str());

                    let mut tags = Vec::new();
                    if let Some(pwd) = entry.password.as_deref().filter(|s| !s.is_empty()) {
                        if is_revealed {
                            tags.push(trf("settings.vault.pwd_revealed", &[("pwd", pwd)]));
                        } else {
                            tags.push(tr("settings.vault.pwd_saved").to_string());
                        }
                    }
                    if let Some(kn) = entry.private_key_name.as_deref() {
                        tags.push(trf("settings.vault.key_saved_named", &[("name", kn)]));
                    } else if entry.private_key_pem.is_some() {
                        tags.push(tr("settings.vault.key_saved").to_string());
                    }
                    let subtitle = if tags.is_empty() {
                        trf("settings.vault.updated_at", &[("time", &entry.updated_at)])
                    } else {
                        format!("{} · {}", tags.join(" · "), entry.updated_at)
                    };

                    let mut actions = div().flex().items_center().gap(px(6.0));
                    if entry.password.as_deref().is_some_and(|s| !s.is_empty()) {
                        actions = actions.child(
                            glass_pill_button(
                                if is_revealed {
                                    tr("settings.vault.hide_pwd")
                                } else {
                                    tr("settings.vault.show_pwd")
                                },
                                false,
                                theme,
                            )
                            .id(("btn-reveal-vault-pwd", idx))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.toggle_reveal_vault_host(&hid_toggle);
                                cx.notify();
                            })),
                        );
                    }
                    actions = actions.child(
                        glass_pill_button(tr("settings.vault.delete_secret"), false, theme)
                            .id(("btn-del-vault-entry", idx))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.delete_host_vault_secret(&hid_del, &hname_del);
                                cx.notify();
                            })),
                    );

                    let mut title_line = div()
                        .flex()
                        .items_center()
                        .gap(px(8.0))
                        .min_w_0()
                        .child(
                            div()
                                .text_size(LayoutMetrics::font_body())
                                .text_color(theme.text_primary)
                                .child(entry.host_name.clone()),
                        );
                    if !entry.hostname.is_empty() && entry.hostname != entry.host_name {
                        title_line = title_line.child(
                            div()
                                .text_size(LayoutMetrics::font_caption())
                                .text_color(theme.text_tertiary)
                                .child(entry.hostname.clone()),
                        );
                    }

                    host_secret_rows.push(
                        div()
                            .w_full()
                            .px(px(16.0))
                            .py(px(12.0))
                            .flex()
                            .items_center()
                            .justify_between()
                            .gap(px(12.0))
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .flex()
                                    .flex_col()
                                    .gap(px(2.0))
                                    .child(title_line)
                                    .child(
                                        div()
                                            .text_size(LayoutMetrics::font_caption())
                                            .text_color(theme.text_tertiary)
                                            .child(subtitle),
                                    ),
                            )
                            .child(actions.flex_none()),
                    );
                }
            }

            return div()
                .w_full()
                .flex()
                .flex_col()
                .gap(px(22.0))
                .child(settings_card(
                    tr("settings.sub.vault_host_secrets"),
                    host_secret_rows,
                    theme,
                ));
        }
        Some(SettingsSubPage::VaultLocalKeys) => {
            let mut key_rows = vec![
                setting_row(
                    tr("settings.vault.agent_status"),
                    None,
                    div()
                        .text_size(LayoutMetrics::font_caption())
                        .text_color(if agent_online {
                            theme.text_primary
                        } else {
                            theme.text_tertiary
                        })
                        .child(if agent_online {
                            tr("settings.vault.agent_online")
                        } else {
                            tr("settings.vault.agent_offline")
                        })
                        .into_any_element(),
                    theme,
                ),
                setting_row(
                    tr("settings.vault.extra_keys"),
                    None,
                    div()
                        .flex()
                        .items_center()
                        .gap(px(8.0))
                        .child(
                            glass_pill_button(tr("settings.vault.reveal_finder"), false, theme)
                                .id("btn-reveal-ssh-dir")
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.reveal_ssh_dir(cx);
                                    cx.notify();
                                })),
                        )
                        .child(
                            glass_pill_button(tr("settings.vault.import_key"), false, theme)
                                .id("btn-import-vault-key")
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.prompt_import_vault_key(cx);
                                })),
                        )
                        .into_any_element(),
                    theme,
                ),
            ];

            for (idx, extra) in settings.extra_key_paths.iter().enumerate() {
                let p = extra.clone();
                key_rows.push(setting_row(
                    extra,
                    None,
                    glass_pill_button(tr("common.remove"), false, theme)
                        .id(("btn-rm-extra-key", idx))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.remove_extra_vault_key(&p);
                            cx.notify();
                        }))
                        .into_any_element(),
                    theme,
                ));
            }

            if vault_local_keys.is_empty() && settings.extra_key_paths.is_empty() {
                key_rows.push(setting_row(
                    tr("settings.vault.local_ssh_keys"),
                    None,
                    div()
                        .text_size(LayoutMetrics::font_caption())
                        .text_color(theme.text_tertiary)
                        .child(tr("settings.vault.zero_keys"))
                        .into_any_element(),
                    theme,
                ));
            } else {
                for key in vault_local_keys.iter().take(8) {
                    let name = key
                        .file_name()
                        .and_then(|n| n.to_str())
                        .unwrap_or("key")
                        .to_string();
                    let path_text = key.display().to_string();
                    key_rows.push(setting_row(
                        &name,
                        Some(&path_text),
                        div()
                            .text_size(LayoutMetrics::font_micro())
                            .text_color(theme.text_tertiary)
                            .child(tr("settings.vault.auto_discovered"))
                            .into_any_element(),
                        theme,
                    ));
                }
            }

            return div()
                .w_full()
                .flex()
                .flex_col()
                .gap(px(22.0))
                .child(settings_card(
                    tr("settings.sub.vault_local_keys"),
                    key_rows,
                    theme,
                ));
        }
        _ => {}
    }

    // 一级概览页：精简清爽，复杂改密与凭据/私钥明细全部收敛至二级页
    let policy_rows = vec![
        setting_row(
            tr("settings.vault.status"),
            Some(tr("settings.vault.unlocked")),
            glass_pill_button(tr("settings.vault.lock_now"), false, theme)
                .id("btn-vault-lock-now")
                .on_click(cx.listener(|this, _, _, cx| {
                    this.lock_vault_now();
                    cx.notify();
                }))
                .into_any_element(),
            theme,
        ),
        setting_row(
            tr("settings.vault.remember_policy"),
            Some(tr("settings.vault.remember_sub")),
            native_dropdown(settings.vault_remember_policy.label_zh(), 138.0, theme)
                .id("btn-vault-remember-policy")
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, event: &MouseDownEvent, _, cx| {
                        this.toggle_dropdown(
                            DropdownKind::VaultRememberPolicy,
                            event.position,
                            152.0,
                        );
                        cx.notify();
                    }),
                )
                .into_any_element(),
            theme,
        ),
        setting_nav_row(
            "nav-vault-change-pwd",
            tr("settings.sub.vault_change_pwd"),
            None,
            None,
            SettingsSubPage::VaultChangePassword,
            theme,
            cx,
        ),
    ];

    let total_local_keys = vault_local_keys.len() + settings.extra_key_paths.len();
    let total_keys_str = total_local_keys.to_string();
    let local_keys_summary = if agent_online {
        trf("settings.vault.keys_online", &[("count", &total_keys_str)])
    } else {
        trf("settings.vault.keys_count", &[("count", &total_keys_str)])
    };
    let host_secrets_summary = trf(
        "settings.vault.items_count",
        &[("count", &vault_entries.len().to_string())],
    );

    let manage_rows = vec![
        setting_nav_row(
            "nav-vault-host-secrets",
            tr("settings.sub.vault_host_secrets"),
            None,
            Some(&host_secrets_summary),
            SettingsSubPage::VaultHostSecrets,
            theme,
            cx,
        ),
        setting_nav_row(
            "nav-vault-local-keys",
            tr("settings.sub.vault_local_keys"),
            None,
            Some(&local_keys_summary),
            SettingsSubPage::VaultLocalKeys,
            theme,
            cx,
        ),
    ];

    div()
        .w_full()
        .flex()
        .flex_col()
        .gap(px(22.0))
        .child(settings_card(
            tr("settings.vault.card_policy"),
            policy_rows,
            theme,
        ))
        .child(settings_card(
            tr("settings.vault.card_manage"),
            manage_rows,
            theme,
        ))
}

#[allow(clippy::too_many_arguments)]
fn vault_centered_secret_input(
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
        .px(px(12.0))
        .flex()
        .items_center()
        .justify_between()
        .gap(px(6.0))
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
            cx.listener(move |this, ev: &MouseDownEvent, window, cx| {
                cx.stop_propagation();
                this.begin_text_input_mouse_down(target, ev, window);
                cx.notify();
            }),
        )
        .child(left)
        .child(
            div()
                .id(SharedString::from(format!("{id}-eye")))
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
                        this.vault_settings_form.show_passwords =
                            !this.vault_settings_form.show_passwords;
                        cx.notify();
                    }),
                )
                .child(icon_svg("icons/eye.svg", 14.0, theme.text_tertiary)),
        )
}

#[allow(clippy::too_many_arguments)]
fn vault_secret_input(
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
            LayoutMetrics::font_control(),
            13.0,
            theme.text_primary,
            theme.text_tertiary,
            None,
            theme,
        ));

    div()
        .id(id)
        .w(px(296.0))
        .h(px(30.0))
        .px(px(10.0))
        .flex()
        .items_center()
        .justify_between()
        .gap(px(6.0))
        .rounded(px(8.0))
        .cursor_text()
        .bg(theme.surface_input)
        .border_1()
        .border_color(if focused {
            theme.border_specular
        } else {
            theme.border_subtle
        })
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
                .id(SharedString::from(format!("{id}-eye")))
                .w(px(20.0))
                .h(px(20.0))
                .flex_none()
                .flex()
                .items_center()
                .justify_center()
                .cursor_pointer()
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, _: &MouseDownEvent, _, cx| {
                        cx.stop_propagation();
                        this.vault_settings_form.show_passwords =
                            !this.vault_settings_form.show_passwords;
                        cx.notify();
                    }),
                )
                .child(icon_svg("icons/eye.svg", 13.5, theme.text_tertiary)),
        )
}

#[allow(clippy::too_many_arguments)]
fn render_sync_section(
    settings: &AppSettings,
    active_subpage: Option<SettingsSubPage>,
    total_hosts: usize,
    active_input: ActiveInput,
    sync_show_secret: bool,
    sync_in_progress: bool,
    theme: &ThemeTokens,
    cx: &mut Context<HoloSshApp>,
) -> impl IntoElement {
    let sync_gist_token = settings.sync_gist_token.as_str();
    let sync_s3_secret = settings.sync_s3_secret.as_str();
    // 二级详情页：GitHub Gist / S3 多字段配置表单
    match active_subpage {
        Some(SettingsSubPage::SyncGistConfig) => {
            let mut wrap = div()
                .relative()
                .w_full()
                .flex()
                .flex_col()
                .gap(px(22.0))
                .child(settings_card(
                    tr("settings.sync.card_gist"),
                    vec![
                        setting_row(
                            "Personal Access Token",
                            Some(tr("settings.sync.gist_token_sub")),
                            sync_secret_input(
                                "input-sync-gist-token",
                                sync_gist_token,
                                tr("settings.sync.gist_token_ph"),
                                sync_show_secret,
                                active_input == ActiveInput::SyncGistToken,
                                ActiveInput::SyncGistToken,
                                theme,
                                cx,
                            )
                            .into_any_element(),
                            theme,
                        ),
                        setting_row(
                            "Gist ID",
                            None,
                            sync_text_input(
                                "input-sync-gist-id",
                                &settings.gist_id,
                                tr("settings.sync.gist_id_ph"),
                                active_input == ActiveInput::SyncGistId,
                                ActiveInput::SyncGistId,
                                296.0,
                                theme,
                                cx,
                            )
                            .into_any_element(),
                            theme,
                        ),
                        setting_row(
                            tr("settings.sync.gist_filename"),
                            None,
                            sync_text_input(
                                "input-sync-gist-file",
                                &settings.gist_filename,
                                "holossh-sync.json",
                                active_input == ActiveInput::SyncGistFilename,
                                ActiveInput::SyncGistFilename,
                                296.0,
                                theme,
                                cx,
                            )
                            .into_any_element(),
                            theme,
                        ),
                        setting_row(
                            tr("settings.sync.verify_cfg"),
                            None,
                            div()
                                .flex()
                                .items_center()
                                .gap(px(8.0))
                                .child(
                                    glass_pill_button(tr("settings.sync.test_conn"), false, theme)
                                        .id("btn-sync-gist-test")
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            this.trigger_cloud_sync(SyncAction::TestConnection);
                                            cx.notify();
                                        })),
                                )
                                .child(
                                    glass_pill_button(
                                        tr("settings.sync.save_and_check"),
                                        true,
                                        theme,
                                    )
                                    .id("btn-sync-gist-save-probe")
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.save_and_probe_cloud_sync(SyncProvider::GitHubGist);
                                        cx.notify();
                                    })),
                                )
                                .into_any_element(),
                            theme,
                        ),
                    ],
                    theme,
                ));
            if sync_in_progress {
                wrap = wrap.child(module_loading_overlay("sync-gist", theme));
            }
            return wrap;
        }
        Some(SettingsSubPage::SyncS3Config) => {
            let mut wrap = div()
                .relative()
                .w_full()
                .flex()
                .flex_col()
                .gap(px(22.0))
                .child(settings_card(
                    tr("settings.sync.card_s3"),
                    vec![
                        setting_row(
                            "Endpoint URL",
                            None,
                            sync_text_input(
                                "input-sync-s3-ep",
                                &settings.s3_endpoint,
                                "https://s3.amazonaws.com",
                                active_input == ActiveInput::SyncS3Endpoint,
                                ActiveInput::SyncS3Endpoint,
                                296.0,
                                theme,
                                cx,
                            )
                            .into_any_element(),
                            theme,
                        ),
                        setting_row(
                            "Region",
                            None,
                            sync_text_input(
                                "input-sync-s3-region",
                                &settings.s3_region,
                                "us-east-1",
                                active_input == ActiveInput::SyncS3Region,
                                ActiveInput::SyncS3Region,
                                296.0,
                                theme,
                                cx,
                            )
                            .into_any_element(),
                            theme,
                        ),
                        setting_row(
                            tr("settings.sync.s3_bucket"),
                            None,
                            sync_text_input(
                                "input-sync-s3-bucket",
                                &settings.s3_bucket,
                                tr("settings.sync.s3_bucket_ph"),
                                active_input == ActiveInput::SyncS3Bucket,
                                ActiveInput::SyncS3Bucket,
                                296.0,
                                theme,
                                cx,
                            )
                            .into_any_element(),
                            theme,
                        ),
                        setting_row(
                            tr("settings.sync.s3_key"),
                            None,
                            sync_text_input(
                                "input-sync-s3-key",
                                &settings.s3_object_key,
                                "holossh/sync.json",
                                active_input == ActiveInput::SyncS3Key,
                                ActiveInput::SyncS3Key,
                                296.0,
                                theme,
                                cx,
                            )
                            .into_any_element(),
                            theme,
                        ),
                        setting_row(
                            "Access Key ID",
                            None,
                            sync_text_input(
                                "input-sync-s3-ak",
                                &settings.s3_access_key_id,
                                tr("settings.sync.s3_ak_ph"),
                                active_input == ActiveInput::SyncS3AccessKey,
                                ActiveInput::SyncS3AccessKey,
                                296.0,
                                theme,
                                cx,
                            )
                            .into_any_element(),
                            theme,
                        ),
                        setting_row(
                            "Secret Access Key",
                            None,
                            sync_secret_input(
                                "input-sync-s3-sk",
                                sync_s3_secret,
                                tr("settings.sync.s3_sk_ph"),
                                sync_show_secret,
                                active_input == ActiveInput::SyncS3SecretKey,
                                ActiveInput::SyncS3SecretKey,
                                theme,
                                cx,
                            )
                            .into_any_element(),
                            theme,
                        ),
                        setting_row(
                            tr("settings.sync.verify_cfg"),
                            None,
                            div()
                                .flex()
                                .items_center()
                                .gap(px(8.0))
                                .child(
                                    glass_pill_button(tr("settings.sync.test_conn"), false, theme)
                                        .id("btn-sync-s3-test")
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            this.trigger_cloud_sync(SyncAction::TestConnection);
                                            cx.notify();
                                        })),
                                )
                                .child(
                                    glass_pill_button(
                                        tr("settings.sync.save_and_check"),
                                        true,
                                        theme,
                                    )
                                    .id("btn-sync-s3-save-probe")
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.save_and_probe_cloud_sync(SyncProvider::S3);
                                        cx.notify();
                                    })),
                                )
                                .into_any_element(),
                            theme,
                        ),
                    ],
                    theme,
                ));
            if sync_in_progress {
                wrap = wrap.child(module_loading_overlay("sync-s3", theme));
            }
            return wrap;
        }
        _ => {}
    }

    let provider_seg = div()
        .p(px(2.0))
        .flex()
        .items_center()
        .gap(px(2.0))
        .rounded(LayoutMetrics::radius_pill())
        .bg(theme.surface_input)
        .border_1()
        .border_color(theme.border_subtle)
        .child(sync_provider_pill(
            "sync-prov-off",
            SyncProvider::Disabled,
            settings.sync_provider == SyncProvider::Disabled,
            theme,
            cx,
        ))
        .child(sync_provider_pill(
            "sync-prov-gist",
            SyncProvider::GitHubGist,
            settings.sync_provider == SyncProvider::GitHubGist,
            theme,
            cx,
        ))
        .child(sync_provider_pill(
            "sync-prov-s3",
            SyncProvider::S3,
            settings.sync_provider == SyncProvider::S3,
            theme,
            cx,
        ));

    let status_text = if let Some(last) = &settings.last_sync_at {
        last.clone()
    } else {
        trf(
            "settings.sync.never_synced",
            &[("count", &total_hosts.to_string())],
        )
    };

    let mut service_rows = vec![setting_row(
        tr("settings.sync.backend"),
        None,
        provider_seg.into_any_element(),
        theme,
    )];

    match settings.sync_provider {
        SyncProvider::GitHubGist => {
            let gist_summary = if !sync_gist_token.trim().is_empty() {
                tr("common.configured")
            } else {
                tr("common.pending_config")
            };
            service_rows.push(setting_nav_row(
                "nav-sync-gist-cfg",
                tr("settings.sub.sync_gist"),
                None,
                Some(gist_summary),
                SettingsSubPage::SyncGistConfig,
                theme,
                cx,
            ));
        }
        SyncProvider::S3 => {
            let s3_summary = if !settings.s3_bucket.trim().is_empty() {
                settings.s3_bucket.trim().to_string()
            } else {
                tr("common.pending_config").to_string()
            };
            service_rows.push(setting_nav_row(
                "nav-sync-s3-cfg",
                tr("settings.sub.sync_s3"),
                None,
                Some(&s3_summary),
                SettingsSubPage::SyncS3Config,
                theme,
                cx,
            ));
        }
        SyncProvider::Disabled => {}
    }

    service_rows.push(setting_row(
        tr("settings.sync.auto_push"),
        None,
        native_toggle_with_id("sync-auto", settings.sync_auto_on_change, theme)
            .id("toggle-sync-auto")
            .on_click(cx.listener(|this, _, _, cx| {
                this.settings.sync_auto_on_change = !this.settings.sync_auto_on_change;
                this.settings.save();
                cx.notify();
            }))
            .into_any_element(),
        theme,
    ));

    service_rows.push(setting_row(
        tr("settings.sync.auto_pull_start"),
        None,
        native_toggle_with_id("sync-auto-pull", settings.sync_auto_pull_on_start, theme)
            .id("toggle-sync-auto-pull")
            .on_click(cx.listener(|this, _, _, cx| {
                this.settings.sync_auto_pull_on_start = !this.settings.sync_auto_pull_on_start;
                this.settings.save();
                cx.notify();
            }))
            .into_any_element(),
        theme,
    ));

    service_rows.push(setting_row(
        tr("settings.sync.status"),
        Some(&status_text),
        div()
            .text_size(LayoutMetrics::font_caption())
            .text_color(theme.text_tertiary)
            .child(tr("common.ready"))
            .into_any_element(),
        theme,
    ));

    let mut col = div()
        .relative()
        .w_full()
        .flex()
        .flex_col()
        .gap(px(22.0))
        .child(settings_card(
            tr("settings.sync.card_service"),
            service_rows,
            theme,
        ));

    if settings.sync_provider != SyncProvider::Disabled {
        col = col.child(settings_card(
            tr("settings.sync.card_actions"),
            vec![setting_row(
                tr("settings.sync.run_sync"),
                None,
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.0))
                    .child(
                        glass_pill_button(tr("settings.sync.test_conn"), false, theme)
                            .id("btn-sync-test")
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.trigger_cloud_sync(SyncAction::TestConnection);
                                cx.notify();
                            })),
                    )
                    .child(
                        glass_pill_button(tr("settings.sync.pull"), false, theme)
                            .id("btn-sync-pull")
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.trigger_cloud_sync(SyncAction::Pull);
                                cx.notify();
                            })),
                    )
                    .child(
                        glass_pill_button(tr("settings.sync.push"), true, theme)
                            .id("btn-sync-push")
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.trigger_cloud_sync(SyncAction::Push);
                                cx.notify();
                            })),
                    )
                    .into_any_element(),
                theme,
            )],
            theme,
        ));
    }

    let cfg_dir = AppSettings::config_dir()
        .map(|d| d.display().to_string())
        .unwrap_or_else(|| "~/Library/Application Support/Holossh".to_string());

    let mut col = col.child(settings_card(
        tr("settings.sync.card_local"),
        vec![
            setting_row(
                tr("settings.sync.config_dir"),
                Some(&cfg_dir),
                glass_pill_button(tr("settings.sync.open_finder"), false, theme)
                    .id("btn-sync-reveal-dir")
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.reveal_config_dir(cx);
                        cx.notify();
                    }))
                    .into_any_element(),
                theme,
            ),
            setting_row(
                tr("settings.sync.reset_default"),
                Some(tr("settings.sync.reset_sub")),
                glass_pill_button(tr("settings.sync.reset_btn"), false, theme)
                    .id("btn-reset-settings")
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.reset_settings_to_default();
                        cx.notify();
                    }))
                    .into_any_element(),
                theme,
            ),
        ],
        theme,
    ));
    if sync_in_progress {
        col = col.child(module_loading_overlay("sync-main", theme));
    }
    col
}

fn sync_provider_pill(
    id: &'static str,
    provider: SyncProvider,
    active: bool,
    theme: &ThemeTokens,
    cx: &mut Context<HoloSshApp>,
) -> impl IntoElement {
    div()
        .id(id)
        .px(px(11.0))
        .py(px(4.0))
        .rounded(LayoutMetrics::radius_pill())
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
        .text_size(LayoutMetrics::font_control())
        .text_color(if active {
            theme.text_primary
        } else {
            theme.text_secondary
        })
        .hover(|s| {
            s.bg(if active {
                theme.surface_active
            } else {
                theme.surface_pill
            })
        })
        .active(|s| s.opacity(0.76))
        .on_click(cx.listener(move |this, _, _, cx| {
            this.set_sync_provider(provider);
            cx.notify();
        }))
        .child(provider.label_zh())
}

#[allow(clippy::too_many_arguments)]
fn sync_text_input(
    id: &'static str,
    value: &str,
    placeholder: &str,
    focused: bool,
    target: ActiveInput,
    width_px: f32,
    theme: &ThemeTokens,
    cx: &mut Context<HoloSshApp>,
) -> impl IntoElement {
    div()
        .id(id)
        .w(px(width_px))
        .h(px(30.0))
        .px(px(10.0))
        .overflow_hidden()
        .flex()
        .items_center()
        .rounded(px(8.0))
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
            LayoutMetrics::font_control(),
            13.0,
            theme.text_primary,
            theme.text_tertiary,
            None,
            theme,
        ))
}

#[allow(clippy::too_many_arguments)]
fn sync_secret_input(
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
            LayoutMetrics::font_control(),
            13.0,
            theme.text_primary,
            theme.text_tertiary,
            None,
            theme,
        ));

    div()
        .id(id)
        .w(px(296.0))
        .h(px(30.0))
        .px(px(10.0))
        .flex()
        .items_center()
        .justify_between()
        .gap(px(6.0))
        .rounded(px(8.0))
        .cursor_text()
        .bg(theme.surface_input)
        .border_1()
        .border_color(if focused {
            theme.border_specular
        } else {
            theme.border_subtle
        })
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
                .id(SharedString::from(format!("{id}-eye")))
                .w(px(20.0))
                .h(px(20.0))
                .flex_none()
                .flex()
                .items_center()
                .justify_center()
                .cursor_pointer()
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, _: &MouseDownEvent, _, cx| {
                        cx.stop_propagation();
                        this.sync_show_secret = !this.sync_show_secret;
                        cx.notify();
                    }),
                )
                .child(icon_svg("icons/eye.svg", 13.5, theme.text_tertiary)),
        )
}

fn render_about_section(
    theme: &ThemeTokens,
    cx: &mut Context<HoloSshApp>,
) -> impl IntoElement {
    div()
        .w_full()
        .flex()
        .flex_col()
        .gap(px(22.0))
        .child(settings_card(
            tr("settings.about.card_info"),
            vec![
                div()
                    .w_full()
                    .px(px(16.0))
                    .py(px(12.0))
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap(px(12.0))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(10.0))
                            .child(
                                img(if theme.is_dark {
                                    "app-icon/dark.png"
                                } else {
                                    "app-icon/light.png"
                                })
                                .flex_none()
                                .w(px(48.0))
                                .h(px(48.0)),
                            )
                            .child(
                                div()
                                    .flex()
                                    .flex_col()
                                    .gap(px(2.0))
                                    .child(
                                        div()
                                            .text_size(LayoutMetrics::font_body())
                                            .text_color(theme.text_primary)
                                            .child("Holossh"),
                                    )
                                    .child(
                                        div()
                                            .text_size(LayoutMetrics::font_micro())
                                            .text_color(theme.text_tertiary)
                                            .child(tr("onboarding.subtitle")),
                                    ),
                            ),
                    )
                    .child(
                        div()
                            .text_size(LayoutMetrics::font_control())
                            .text_color(theme.text_secondary)
                            .child(concat!("v", env!("CARGO_PKG_VERSION"))),
                    ),
                setting_row(
                    tr("settings.sync.config_dir"),
                    None,
                    glass_pill_button(tr("settings.about.open_dir"), false, theme)
                        .id("btn-about-reveal-dir")
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.reveal_config_dir(cx);
                            cx.notify();
                        }))
                        .into_any_element(),
                    theme,
                ),
                setting_row(
                    tr("settings.about.update_version"),
                    None,
                    glass_pill_button(tr("settings.about.open_github"), false, theme)
                        .id("btn-about-open-update")
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.open_update_url(cx);
                            cx.notify();
                        }))
                        .into_any_element(),
                    theme,
                ),
            ],
            theme,
        ))
}

/// 可点击刻度滑块 + 精细 − / + 调节器
#[allow(clippy::too_many_arguments)]
fn render_interactive_slider(
    id_prefix: &'static str,
    steps: &[f32],
    current: f32,
    value_text: String,
    theme: &ThemeTokens,
    on_set: fn(&mut HoloSshApp, f32),
    on_dec: fn(&mut HoloSshApp),
    on_inc: fn(&mut HoloSshApp),
    cx: &mut Context<HoloSshApp>,
) -> Div {
    let mut track = div()
        .w(px(136.0))
        .h(px(16.0))
        .flex()
        .items_center()
        .gap(px(2.0));

    for (idx, &step_val) in steps.iter().enumerate() {
        let filled = current + 0.001 >= step_val;
        track = track.child(
            div()
                .id(SharedString::from(format!("{id_prefix}-step-{idx}")))
                .flex_1()
                .h(px(6.0))
                .rounded(px(2.0))
                .cursor_pointer()
                .bg(if filled {
                    theme.text_secondary
                } else {
                    theme.surface_pill
                })
                .hover(|s| s.bg(theme.text_primary))
                .active(|s| s.opacity(0.75))
                .on_click(cx.listener(move |this, _, _, cx| {
                    on_set(this, step_val);
                    cx.notify();
                })),
        );
    }

    div()
        .flex()
        .items_center()
        .gap(px(8.0))
        .child(
            div()
                .id(SharedString::from(format!("{id_prefix}-dec")))
                .w(px(20.0))
                .h(px(20.0))
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(6.0))
                .bg(theme.surface_pill)
                .cursor_pointer()
                .hover(|s| s.bg(theme.surface_active))
                .active(|s| s.opacity(0.72))
                .text_size(LayoutMetrics::font_control())
                .text_color(theme.text_secondary)
                .on_click(cx.listener(move |this, _, _, cx| {
                    on_dec(this);
                    cx.notify();
                }))
                .child("−"),
        )
        .child(track)
        .child(
            div()
                .id(SharedString::from(format!("{id_prefix}-inc")))
                .w(px(20.0))
                .h(px(20.0))
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(6.0))
                .bg(theme.surface_pill)
                .cursor_pointer()
                .hover(|s| s.bg(theme.surface_active))
                .active(|s| s.opacity(0.72))
                .text_size(LayoutMetrics::font_control())
                .text_color(theme.text_secondary)
                .on_click(cx.listener(move |this, _, _, cx| {
                    on_inc(this);
                    cx.notify();
                }))
                .child("+"),
        )
        .child(
            div()
                .w(px(44.0))
                .flex()
                .justify_end()
                .text_size(LayoutMetrics::font_control())
                .text_color(theme.text_secondary)
                .child(value_text),
        )
}

fn cursor_style_pill(
    id: &'static str,
    style: CursorStyle,
    active: bool,
    theme: &ThemeTokens,
    cx: &mut Context<HoloSshApp>,
) -> impl IntoElement {
    div()
        .id(id)
        .px(px(11.0))
        .py(px(4.0))
        .rounded(LayoutMetrics::radius_pill())
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
        .text_size(LayoutMetrics::font_control())
        .text_color(if active {
            theme.text_primary
        } else {
            theme.text_secondary
        })
        .hover(|s| {
            s.bg(if active {
                theme.surface_active
            } else {
                theme.surface_pill
            })
        })
        .active(|s| s.opacity(0.76))
        .on_click(cx.listener(move |this, _, _, cx| {
            this.set_cursor_style(style);
            cx.notify();
        }))
        .child(style.label_zh())
}

fn seg_pill(
    id: &'static str,
    label: &'static str,
    active: bool,
    mode: AppearanceMode,
    theme: &ThemeTokens,
    cx: &mut Context<HoloSshApp>,
) -> impl IntoElement {
    div()
        .id(id)
        .px(px(11.0))
        .py(px(4.0))
        .rounded(LayoutMetrics::radius_pill())
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
        .text_size(LayoutMetrics::font_control())
        .text_color(if active {
            theme.text_primary
        } else {
            theme.text_secondary
        })
        .hover(|s| {
            s.bg(if active {
                theme.surface_active
            } else {
                theme.surface_pill
            })
        })
        .active(|s| s.opacity(0.76))
        .on_click(cx.listener(move |this, _, _, cx| {
            this.set_appearance(mode);
            cx.notify();
        }))
        .child(label)
}

fn glass_scope_pill(
    id: &'static str,
    label: &'static str,
    active: bool,
    scope: GlassScope,
    theme: &ThemeTokens,
    cx: &mut Context<HoloSshApp>,
) -> impl IntoElement {
    div()
        .id(id)
        .px(px(11.0))
        .py(px(4.0))
        .rounded(LayoutMetrics::radius_pill())
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
        .text_size(LayoutMetrics::font_control())
        .text_color(if active {
            theme.text_primary
        } else {
            theme.text_secondary
        })
        .hover(|s| {
            s.bg(if active {
                theme.surface_active
            } else {
                theme.surface_pill
            })
        })
        .active(|s| s.opacity(0.76))
        .on_click(cx.listener(move |this, _, _, cx| {
            this.set_glass_scope(scope);
            cx.notify();
        }))
        .child(label)
}

fn sidebar_side_pill(
    id: &'static str,
    label: &'static str,
    active: bool,
    side: SidebarSide,
    theme: &ThemeTokens,
    cx: &mut Context<HoloSshApp>,
) -> impl IntoElement {
    div()
        .id(id)
        .px(px(11.0))
        .py(px(4.0))
        .rounded(LayoutMetrics::radius_pill())
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
        .text_size(LayoutMetrics::font_control())
        .text_color(if active {
            theme.text_primary
        } else {
            theme.text_secondary
        })
        .hover(|s| {
            s.bg(if active {
                theme.surface_active
            } else {
                theme.surface_pill
            })
        })
        .active(|s| s.opacity(0.76))
        .on_click(cx.listener(move |this, _, _, cx| {
            this.settings.sidebar_side = side;
            this.settings.save();
            cx.notify();
        }))
        .child(label)
}

fn nav_group(
    title: Option<&str>,
    items: &[SettingsSection],
    active: SettingsSection,
    lang: crate::state::settings::Language,
    theme: &ThemeTokens,
    cx: &mut Context<HoloSshApp>,
) -> Div {
    let mut col = div().w_full().flex().flex_col().gap(px(3.0));
    if let Some(t) = title {
        col = col.child(
            div()
                .px(px(8.0))
                .pb(px(3.0))
                .text_size(LayoutMetrics::font_caption())
                .text_color(theme.sidebar_text_tertiary)
                .child(t.to_string()),
        );
    }
    for (idx, &sec) in items.iter().enumerate() {
        let is_active = sec == active;
        col = col.child(
            div()
                .id(("settings-nav", (sec as usize) * 10 + idx))
                .w_full()
                .h(px(36.0))
                .px(px(10.0))
                .flex()
                .items_center()
                .gap(px(10.0))
                .rounded(px(9.0))
                .cursor_pointer()
                .bg(if is_active {
                    theme.surface_active
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
                .active(|s| s.opacity(0.76))
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.select_settings_section(sec);
                    cx.notify();
                }))
                .child(icon_svg(
                    sec.icon_path(),
                    15.0,
                    if is_active {
                        theme.sidebar_text_primary
                    } else {
                        theme.sidebar_text_secondary
                    },
                ))
                .child(
                    div()
                        .text_size(LayoutMetrics::font_body())
                        .text_color(if is_active {
                            theme.sidebar_text_primary
                        } else {
                            theme.sidebar_text_secondary
                        })
                        .child(sec.title(lang)),
                ),
        );
    }
    col
}

fn settings_card(title: &str, rows: Vec<Div>, theme: &ThemeTokens) -> Div {
    let mut card = div()
        .w_full()
        .overflow_hidden()
        .flex()
        .flex_col()
        .rounded(LayoutMetrics::radius_card())
        .bg(theme.card_bg)
        .border_1()
        .border_color(theme.border_subtle);

    let len = rows.len();
    for (i, row) in rows.into_iter().enumerate() {
        card = card.child(row);
        if i + 1 < len {
            card = card.child(div().w_full().px(px(16.0)).child(hairline_h(theme)));
        }
    }

    div()
        .w_full()
        .flex()
        .flex_col()
        .gap(px(8.0))
        .child(
            div()
                .text_size(LayoutMetrics::font_caption())
                .text_color(theme.text_tertiary)
                .child(title.to_string()),
        )
        .child(card)
}

/// macOS 系统设置风格二级跳转行（右侧带可选摘要文案与 `>` 箭头，整行可点击进入二级页）
fn setting_nav_row(
    id: &'static str,
    title: &str,
    subtitle: Option<&str>,
    summary: Option<&str>,
    target: SettingsSubPage,
    theme: &ThemeTokens,
    cx: &mut Context<HoloSshApp>,
) -> Div {
    let mut left = div().flex().flex_col().gap(px(2.0)).child(
        div()
            .text_size(LayoutMetrics::font_body())
            .text_color(theme.text_primary)
            .child(title.to_string()),
    );
    if let Some(sub) = subtitle {
        left = left.child(
            div()
                .text_size(LayoutMetrics::font_caption())
                .text_color(theme.text_tertiary)
                .child(sub.to_string()),
        );
    }

    let mut right = div().flex().items_center().gap(px(6.0));
    if let Some(s) = summary {
        right = right.child(
            div()
                .text_size(LayoutMetrics::font_control())
                .text_color(theme.text_tertiary)
                .child(s.to_string()),
        );
    }
    right = right.child(icon_svg("icons/chevron-right.svg", 13.0, theme.text_tertiary));

    div().w_full().child(
        div()
            .id(id)
            .w_full()
            .px(px(16.0))
            .py(px(12.0))
            .flex()
            .items_center()
            .justify_between()
            .gap(px(12.0))
            .cursor_pointer()
            .hover(|s| s.bg(theme.surface_pill))
            .active(|s| s.opacity(0.76))
            .on_click(cx.listener(move |this, _, _, cx| {
                this.open_settings_subpage(target);
                cx.notify();
            }))
            .child(left)
            .child(right),
    )
}

fn setting_row(
    title: &str,
    subtitle: Option<&str>,
    control: AnyElement,
    theme: &ThemeTokens,
) -> Div {
    let mut left = div().flex().flex_col().gap(px(2.0)).child(
        div()
            .text_size(LayoutMetrics::font_body())
            .text_color(theme.text_primary)
            .child(title.to_string()),
    );
    if let Some(sub) = subtitle {
        left = left.child(
            div()
                .text_size(LayoutMetrics::font_caption())
                .text_color(theme.text_tertiary)
                .child(sub.to_string()),
        );
    }

    div()
        .w_full()
        .px(px(16.0))
        .py(px(12.0))
        .flex()
        .items_center()
        .justify_between()
        .gap(px(12.0))
        .child(left)
        .child(control)
}
