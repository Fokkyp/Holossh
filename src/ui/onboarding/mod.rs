//! 首次打开极简磨砂引导页（严格对应 Figma 07 画板最终极简版）

use crate::app::{DropdownKind, HoloSshApp};
use crate::i18n::{tr, trf};
use crate::state::settings::AppSettings;
use crate::theme::{AppearanceMode, LayoutMetrics, ThemeTokens};
use crate::ui::components::{
    glass_pill_button, hairline_h, native_dropdown, native_toggle_with_id,
};
use gpui::{
    Animation, AnimationExt, Context, InteractiveElement, IntoElement, MouseButton, MouseDownEvent,
    ParentElement, StatefulInteractiveElement, Styled, div, ease_out_quint, img, px,
};
use std::time::Duration;

pub fn render_onboarding_view(
    settings: &AppSettings,
    detected_ssh_hosts: usize,
    import_enabled: bool,
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
        .child(onboard_mode_pill(
            "ob-auto",
            tr("onboarding.appearance_auto"),
            settings.appearance == AppearanceMode::System,
            AppearanceMode::System,
            theme,
            cx,
        ))
        .child(onboard_mode_pill(
            "ob-dark",
            tr("onboarding.appearance_dark"),
            settings.appearance == AppearanceMode::Dark,
            AppearanceMode::Dark,
            theme,
            cx,
        ))
        .child(onboard_mode_pill(
            "ob-light",
            tr("onboarding.appearance_light"),
            settings.appearance == AppearanceMode::Light,
            AppearanceMode::Light,
            theme,
            cx,
        ));

    let card = div()
        .w_full()
        .flex()
        .flex_col()
        .rounded(LayoutMetrics::radius_card())
        .bg(theme.card_bg)
        .border_1()
        .border_color(theme.border_subtle)
        // 行 1：语言下拉框
        .child(
            div()
                .w_full()
                .px(px(16.0))
                .py(px(12.0))
                .flex()
                .items_center()
                .justify_between()
                .child(
                    div()
                        .text_size(LayoutMetrics::font_body())
                        .text_color(theme.text_primary)
                        .child(settings.language.t("onboarding.language")),
                )
                .child(
                    native_dropdown(settings.language.display_name(), 132.0, theme)
                        .id("btn-ob-lang")
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
                        ),
                ),
        )
        .child(div().w_full().px(px(16.0)).child(hairline_h(theme)))
        // 行 2：外观
        .child(
            div()
                .w_full()
                .px(px(16.0))
                .py(px(12.0))
                .flex()
                .items_center()
                .justify_between()
                .child(
                    div()
                        .text_size(LayoutMetrics::font_body())
                        .text_color(theme.text_primary)
                        .child(tr("onboarding.appearance")),
                )
                .child(appearance_seg),
        )
        .child(div().w_full().px(px(16.0)).child(hairline_h(theme)))
        // 行 3：导入 ~/.ssh/config
        .child(
            div()
                .w_full()
                .px(px(16.0))
                .py(px(12.0))
                .flex()
                .items_center()
                .justify_between()
                .child(
                    div()
                        .text_size(LayoutMetrics::font_body())
                        .text_color(theme.text_primary)
                        .child(tr("onboarding.import_ssh_config")),
                )
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(10.0))
                        .child(
                            div()
                                .text_size(LayoutMetrics::font_caption())
                                .text_color(theme.text_tertiary)
                                .child(trf(
                                    "onboarding.detected_hosts",
                                    &[("count", &detected_ssh_hosts.to_string())],
                                )),
                        )
                        .child(
                            native_toggle_with_id("ob-import", import_enabled, theme)
                                .id("btn-ob-import")
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.import_ssh_config_on_start =
                                        !this.import_ssh_config_on_start;
                                    cx.notify();
                                })),
                        ),
                ),
        );

    div()
        .size_full()
        .flex()
        .items_center()
        .justify_center()
        .bg(theme.window_bg)
        .child(
            div()
                .w(px(LayoutMetrics::ONBOARDING_WIDTH))
                .px(px(34.0))
                .py(px(28.0))
                .flex()
                .flex_col()
                .items_center()
                .gap(px(24.0))
                // 图标与极简标题
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .items_center()
                        .gap(px(10.0))
                        .child(
                            img(if theme.is_dark {
                                "app-icon/dark.png"
                            } else {
                                "app-icon/light.png"
                            })
                            .w(px(78.0))
                            .h(px(78.0)),
                        )
                        .child(
                            div()
                                .text_size(LayoutMetrics::font_hero())
                                .text_color(theme.text_primary)
                                .child(settings.language.t("onboarding.welcome")),
                        ),
                )
                .child(card)
                .child(
                    glass_pill_button(settings.language.t("onboarding.start"), true, theme)
                        .id("btn-ob-start")
                        .w(px(168.0))
                        .h(px(34.0))
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.complete_onboarding();
                            cx.notify();
                        })),
                )
                .with_animation(
                    "onboarding-enter",
                    Animation::new(Duration::from_millis(240)).with_easing(ease_out_quint()),
                    |el, d| el.opacity(d).mt(px(12.0 * (1.0 - d))),
                ),
        )
}

fn onboard_mode_pill(
    id: &'static str,
    label: &'static str,
    active: bool,
    mode: AppearanceMode,
    theme: &ThemeTokens,
    cx: &mut Context<HoloSshApp>,
) -> impl IntoElement {
    div()
        .id(id)
        .px(px(10.0))
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
        .text_size(LayoutMetrics::font_caption())
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
