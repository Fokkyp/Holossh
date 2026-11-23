//! 1:1 对应 Figma `HoloSSH / Theme` 的双模式精确 Token 定义
//!
//! 禁止在此之外硬编码与 Figma 设计图不一致的色彩、圆角或边框数值。

use gpui::{Hsla, Pixels, Rgba, px};
use serde::{Deserialize, Serialize};

/// 外观模式（对应 06 设置页与 07 引导页的「自动 / 深色 / 浅色」）
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum AppearanceMode {
    System,
    #[default]
    Dark,
    Light,
}

/// 辅助构造：直接将 Figma 0.0–1.0 RGBA 数值转换为 GPUI `Hsla`
#[inline]
fn rgba(r: f32, g: f32, b: f32, a: f32) -> Hsla {
    Rgba { r, g, b, a }.into()
}

/// 对应 Figma `HoloSSH / Theme` 变量集合的运行时色彩表
#[derive(Debug, Clone, Copy)]
pub struct ThemeTokens {
    /// 当前是否处于深色外观（供双模式应用图标等资源即时切换）
    pub is_dark: bool,

    // 玻璃表面
    pub window_bg: Hsla,
    pub sidebar_bg: Hsla,
    pub terminal_bg: Hsla,
    pub sftp_bg: Hsla,
    pub surface_pill: Hsla,
    pub surface_active: Hsla,
    pub row_selected: Hsla,
    pub surface_input: Hsla,
    pub sheet_bg: Hsla,
    pub sheet_scrim: Hsla,
    pub card_bg: Hsla,

    // 描边与分割线
    pub border_specular: Hsla,
    pub border_subtle: Hsla,
    pub border_row_selected: Hsla,

    // 文字层级
    pub text_primary: Hsla,
    pub text_secondary: Hsla,
    pub text_tertiary: Hsla,
    // 左侧边栏文字层级（较通用层级再提亮一号）
    pub sidebar_text_primary: Hsla,
    pub sidebar_text_secondary: Hsla,
    pub sidebar_text_tertiary: Hsla,
    pub text_terminal: Hsla,
    #[allow(dead_code)]
    pub text_terminal_muted: Hsla,
    #[allow(dead_code)]
    pub text_prompt_host: Hsla,

    // 状态色
    pub status_connected: Hsla,
    pub status_idle: Hsla,
}

impl ThemeTokens {
    /// 深色模式（对应 Figma `01 / 02 / 04 / 05 / 06 / 07`）
    pub fn dark() -> Self {
        Self {
            is_dark: true,
            window_bg: rgba(0.080, 0.085, 0.100, 0.56),
            sidebar_bg: rgba(0.150, 0.160, 0.185, 0.54),
            terminal_bg: rgba(0.080, 0.085, 0.095, 0.34),
            sftp_bg: rgba(0.140, 0.150, 0.180, 0.44),
            surface_pill: rgba(1.000, 1.000, 1.000, 0.08),
            surface_active: rgba(1.000, 1.000, 1.000, 0.13),
            row_selected: rgba(1.000, 1.000, 1.000, 0.13),
            surface_input: rgba(1.000, 1.000, 1.000, 0.10),
            sheet_bg: rgba(0.130, 0.135, 0.155, 0.80),
            sheet_scrim: rgba(0.000, 0.000, 0.000, 0.34),
            card_bg: rgba(1.000, 1.000, 1.000, 0.045),

            border_specular: rgba(1.000, 1.000, 1.000, 0.14),
            border_subtle: rgba(1.000, 1.000, 1.000, 0.07),
            border_row_selected: rgba(1.000, 1.000, 1.000, 0.00),

            text_primary: rgba(0.960, 0.970, 0.980, 0.96),
            text_secondary: rgba(0.860, 0.880, 0.920, 0.62),
            text_tertiary: rgba(0.820, 0.850, 0.900, 0.40),
            sidebar_text_primary: rgba(1.000, 1.000, 1.000, 1.00),
            sidebar_text_secondary: rgba(0.940, 0.955, 0.980, 0.82),
            sidebar_text_tertiary: rgba(0.900, 0.925, 0.960, 0.60),
            text_terminal: rgba(0.910, 0.925, 0.945, 0.92),
            text_terminal_muted: rgba(0.720, 0.750, 0.800, 0.56),
            text_prompt_host: rgba(0.440, 0.900, 0.620, 0.95),

            status_connected: rgba(0.190, 0.820, 0.350, 1.00),
            status_idle: rgba(0.600, 0.630, 0.680, 0.55),
        }
    }

    /// 浅色模式（参考 macOS 原生系统设置：左侧磨砂浅灰侧栏 + 右侧清透纯白内容区 + 加深高对比文字）
    pub fn light() -> Self {
        Self {
            is_dark: false,
            window_bg: rgba(0.996, 0.997, 1.000, 0.88),
            sidebar_bg: rgba(0.905, 0.910, 0.922, 0.66),
            terminal_bg: rgba(0.996, 0.997, 1.000, 0.90),
            sftp_bg: rgba(0.935, 0.940, 0.950, 0.80),
            surface_pill: rgba(0.000, 0.000, 0.000, 0.045),
            surface_active: rgba(0.000, 0.000, 0.000, 0.090),
            row_selected: rgba(0.000, 0.000, 0.000, 0.090),
            surface_input: rgba(0.000, 0.000, 0.000, 0.055),
            sheet_bg: rgba(0.985, 0.988, 0.994, 0.92),
            sheet_scrim: rgba(0.000, 0.000, 0.000, 0.14),
            card_bg: rgba(0.948, 0.951, 0.960, 0.86),

            border_specular: rgba(0.000, 0.000, 0.000, 0.16),
            border_subtle: rgba(0.000, 0.000, 0.000, 0.095),
            border_row_selected: rgba(0.000, 0.000, 0.000, 0.00),

            text_primary: rgba(0.045, 0.050, 0.070, 0.96),
            text_secondary: rgba(0.105, 0.120, 0.155, 0.78),
            text_tertiary: rgba(0.155, 0.175, 0.215, 0.58),
            sidebar_text_primary: rgba(0.030, 0.035, 0.050, 0.98),
            sidebar_text_secondary: rgba(0.080, 0.092, 0.120, 0.85),
            sidebar_text_tertiary: rgba(0.130, 0.148, 0.185, 0.65),
            text_terminal: rgba(0.055, 0.065, 0.090, 0.96),
            text_terminal_muted: rgba(0.180, 0.200, 0.250, 0.72),
            text_prompt_host: rgba(0.060, 0.500, 0.240, 0.96),

            status_connected: rgba(0.140, 0.720, 0.300, 1.00),
            status_idle: rgba(0.320, 0.350, 0.400, 0.70),
        }
    }

    pub fn resolve(mode: AppearanceMode, system_is_dark: bool) -> Self {
        match mode {
            AppearanceMode::Dark => Self::dark(),
            AppearanceMode::Light => Self::light(),
            AppearanceMode::System => {
                if system_is_dark {
                    Self::dark()
                } else {
                    Self::light()
                }
            }
        }
    }
}

/// 与 Figma 7 张画板完全一致的固定尺寸规范
pub struct LayoutMetrics;

impl LayoutMetrics {
    pub const WINDOW_WIDTH: f32 = 1180.0;
    pub const WINDOW_HEIGHT: f32 = 744.0;
    pub const ONBOARDING_WIDTH: f32 = 468.0;
    #[allow(dead_code)]
    pub const ONBOARDING_HEIGHT: f32 = 445.0;

    pub const SIDEBAR_WIDTH: f32 = 264.0;
    pub const SFTP_PANEL_WIDTH: f32 = 416.0;
    pub const NEW_HOST_SHEET_WIDTH: f32 = 528.0;

    pub const TITLEBAR_HEIGHT: f32 = 48.0;
    pub const PANE_HEADER_HEIGHT: f32 = 28.0;
    pub const HOST_ROW_HEIGHT: f32 = 34.0;
    pub const TAB_HEIGHT: f32 = 28.0;
    pub const TOOLBAR_CAPSULE_WIDTH: f32 = 73.0;
    pub const TOOLBAR_CAPSULE_HEIGHT: f32 = 28.0;

    pub fn radius_window() -> Pixels {
        px(18.0)
    }

    pub fn radius_card() -> Pixels {
        px(12.0)
    }

    pub fn radius_row() -> Pixels {
        px(8.0)
    }

    pub fn radius_pill() -> Pixels {
        px(999.0)
    }

    // 统一非终端 UI 字号阶梯（主页 / 侧栏 / 设置页 / 弹窗 / SFTP / 引导页 / 菜单严格对齐）
    pub fn font_hero() -> Pixels {
        px(20.0)
    }

    pub fn font_title() -> Pixels {
        px(16.0)
    }

    pub fn font_body() -> Pixels {
        px(13.5)
    }

    pub fn font_control() -> Pixels {
        px(13.0)
    }

    pub fn font_caption() -> Pixels {
        px(12.5)
    }

    pub fn font_micro() -> Pixels {
        px(11.5)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dark_and_light_tokens_resolve_cleanly() {
        let dark = ThemeTokens::resolve(AppearanceMode::Dark, false);
        let light = ThemeTokens::resolve(AppearanceMode::Light, true);
        assert!(dark.text_primary.l > light.text_primary.l);
        assert!(light.sidebar_bg.a > 0.0 && light.sidebar_bg.a < 0.7);
    }
}
