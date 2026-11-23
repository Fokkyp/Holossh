//! 全局顶部居中浮层 Toast 通知系统
//!
//! 统一承载应用内所有报错、校验失败与状态反馈；支持多条叠加展示、各自独立定时自动消失，
//! 且每条 Toast 均提供手动关闭图标。严格遵循单色磨砂视觉体系。

use crate::app::HoloSshApp;
use crate::theme::{LayoutMetrics, ThemeTokens};
use crate::ui::components::icon_svg;
use gpui::{
    Animation, AnimationExt, Context, InteractiveElement, IntoElement, MouseButton,
    MouseDownEvent, ParentElement, Rgba, SharedString, StatefulInteractiveElement, Styled, div,
    ease_out_quint, px,
};
use std::time::{Duration, Instant};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ToastKind {
    Info,
    Success,
    Error,
}

#[derive(Clone, Debug)]
pub struct ToastItem {
    pub id: u64,
    pub message: String,
    pub kind: ToastKind,
    pub created_at: Instant,
    pub duration: Duration,
}

impl ToastItem {
    pub fn new(id: u64, message: impl Into<String>, kind: ToastKind) -> Self {
        let duration = match kind {
            ToastKind::Error => Duration::from_millis(4500),
            ToastKind::Success => Duration::from_millis(3200),
            ToastKind::Info => Duration::from_millis(3500),
        };
        Self {
            id,
            message: message.into(),
            kind,
            created_at: Instant::now(),
            duration,
        }
    }

    pub fn is_expired(&self) -> bool {
        self.created_at.elapsed() >= self.duration
    }
}

/// 渲染全局顶部居中堆叠 Toast（挂载于根视图最顶层，外层不拦截点击）
pub fn render_toast_stack(
    toasts: &[ToastItem],
    is_dark: bool,
    theme: &ThemeTokens,
    cx: &mut Context<HoloSshApp>,
) -> impl IntoElement {
    let error_dot: gpui::Hsla = if is_dark {
        Rgba {
            r: 0.96,
            g: 0.44,
            b: 0.44,
            a: 0.95,
        }
        .into()
    } else {
        Rgba {
            r: 0.80,
            g: 0.20,
            b: 0.20,
            a: 0.92,
        }
        .into()
    };

    let mut stack = div()
        .absolute()
        .top(px(16.0))
        .left_0()
        .right_0()
        .px(px(24.0))
        .flex()
        .flex_col()
        .items_center()
        .gap(px(8.0));

    for toast in toasts {
        let toast_id = toast.id;
        let dot_color = match toast.kind {
            ToastKind::Error => error_dot,
            ToastKind::Success => theme.text_primary,
            ToastKind::Info => theme.text_tertiary,
        };

        let card = div()
            .id(SharedString::from(format!("global-toast-card-{toast_id}")))
            .min_w(px(220.0))
            .max_w(px(460.0))
            .px(px(14.0))
            .py(px(9.0))
            .rounded(px(11.0))
            .bg(theme.sheet_bg)
            .border_1()
            .border_color(theme.border_specular)
            .shadow_lg()
            .flex()
            .items_center()
            .justify_between()
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
                    .items_center()
                    .gap(px(9.0))
                    .child(
                        div()
                            .w(px(6.5))
                            .h(px(6.5))
                            .flex_none()
                            .rounded_full()
                            .bg(dot_color),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_size(LayoutMetrics::font_control())
                            .text_color(theme.text_primary)
                            .child(toast.message.clone()),
                    ),
            )
            .child(
                div()
                    .id(SharedString::from(format!("btn-close-toast-{toast_id}")))
                    .w(px(20.0))
                    .h(px(20.0))
                    .flex_none()
                    .rounded(px(5.0))
                    .flex()
                    .items_center()
                    .justify_center()
                    .cursor_pointer()
                    .hover(|s| s.bg(theme.surface_pill))
                    .active(|s| s.opacity(0.70))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|_, _: &MouseDownEvent, _, cx| {
                            cx.stop_propagation();
                        }),
                    )
                    .on_click(cx.listener(move |this, _, _, cx| {
                        cx.stop_propagation();
                        this.dismiss_toast(toast_id);
                        cx.notify();
                    }))
                    .child(icon_svg("icons/close.svg", 10.0, theme.text_tertiary)),
            )
            .with_animation(
                SharedString::from(format!("global-toast-anim-{toast_id}")),
                Animation::new(Duration::from_millis(180)).with_easing(ease_out_quint()),
                |el, d| el.opacity(d).mt(px(-8.0 * (1.0 - d))),
            );

        stack = stack.child(card);
    }

    stack
}
