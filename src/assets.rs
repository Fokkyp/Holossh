//! 内嵌 SVG 图标资源源（GPUI `AssetSource`）
//!
//! 将 `assets/icons/*.svg` 编译进二进制，运行时零磁盘路径依赖，支持单色 SVG 随主题动态着色。

use gpui::{AssetSource, Result, SharedString};
use std::borrow::Cow;

pub struct HoloAssets;

/// 深色模式 macOS App 图标 PNG（512×512，供 Dock 图标与界面品牌展示共用）
pub const APP_ICON_DARK_PNG: &[u8] = include_bytes!("../assets/app-icon/AppIcon-Dark.png");
/// 浅色模式 macOS App 图标 PNG（512×512，供 Dock 图标与界面品牌展示共用）
pub const APP_ICON_LIGHT_PNG: &[u8] = include_bytes!("../assets/app-icon/AppIcon-Light.png");

const ALL_ICONS: &[(&str, &[u8])] = &[
    ("app-icon/dark.png", APP_ICON_DARK_PNG),
    ("app-icon/light.png", APP_ICON_LIGHT_PNG),
    (
        "app-icon/symbol-dark.png",
        include_bytes!("../assets/app-icon/holossh-symbol-dark.png"),
    ),
    (
        "app-icon/symbol-light.png",
        include_bytes!("../assets/app-icon/holossh-symbol-light.png"),
    ),
    (
        "icons/sidebar-toggle.svg",
        include_bytes!("../assets/icons/sidebar-toggle.svg"),
    ),
    ("icons/sun.svg", include_bytes!("../assets/icons/sun.svg")),
    ("icons/moon.svg", include_bytes!("../assets/icons/moon.svg")),
    ("icons/plus.svg", include_bytes!("../assets/icons/plus.svg")),
    (
        "icons/search.svg",
        include_bytes!("../assets/icons/search.svg"),
    ),
    ("icons/gear.svg", include_bytes!("../assets/icons/gear.svg")),
    (
        "icons/folder.svg",
        include_bytes!("../assets/icons/folder.svg"),
    ),
    (
        "icons/split-pane.svg",
        include_bytes!("../assets/icons/split-pane.svg"),
    ),
    (
        "icons/split-horizontal.svg",
        include_bytes!("../assets/icons/split-horizontal.svg"),
    ),
    (
        "icons/upload.svg",
        include_bytes!("../assets/icons/upload.svg"),
    ),
    (
        "icons/download.svg",
        include_bytes!("../assets/icons/download.svg"),
    ),
    (
        "icons/cloud-download.svg",
        include_bytes!("../assets/icons/cloud-download.svg"),
    ),
    ("icons/cloud.svg", include_bytes!("../assets/icons/cloud.svg")),
    (
        "icons/cloud-x.svg",
        include_bytes!("../assets/icons/cloud-x.svg"),
    ),
    (
        "icons/cloud-arrow-up.svg",
        include_bytes!("../assets/icons/cloud-arrow-up.svg"),
    ),
    (
        "icons/cloud-arrow-down.svg",
        include_bytes!("../assets/icons/cloud-arrow-down.svg"),
    ),
    (
        "icons/folder-plus.svg",
        include_bytes!("../assets/icons/folder-plus.svg"),
    ),
    (
        "icons/refresh.svg",
        include_bytes!("../assets/icons/refresh.svg"),
    ),
    (
        "icons/close.svg",
        include_bytes!("../assets/icons/close.svg"),
    ),
    (
        "icons/copy.svg",
        include_bytes!("../assets/icons/copy.svg"),
    ),
    (
        "icons/chevron-left.svg",
        include_bytes!("../assets/icons/chevron-left.svg"),
    ),
    (
        "icons/chevron-down.svg",
        include_bytes!("../assets/icons/chevron-down.svg"),
    ),
    (
        "icons/chevron-right.svg",
        include_bytes!("../assets/icons/chevron-right.svg"),
    ),
    ("icons/eye.svg", include_bytes!("../assets/icons/eye.svg")),
    (
        "icons/auth-auto.svg",
        include_bytes!("../assets/icons/auth-auto.svg"),
    ),
    (
        "icons/auth-password.svg",
        include_bytes!("../assets/icons/auth-password.svg"),
    ),
    (
        "icons/auth-key.svg",
        include_bytes!("../assets/icons/auth-key.svg"),
    ),
    (
        "icons/auth-agent.svg",
        include_bytes!("../assets/icons/auth-agent.svg"),
    ),
    (
        "icons/auth-interactive.svg",
        include_bytes!("../assets/icons/auth-interactive.svg"),
    ),
    (
        "icons/terminal.svg",
        include_bytes!("../assets/icons/terminal.svg"),
    ),
    (
        "icons/connections.svg",
        include_bytes!("../assets/icons/connections.svg"),
    ),
    ("icons/sync.svg", include_bytes!("../assets/icons/sync.svg")),
    (
        "icons/spinner.svg",
        include_bytes!("../assets/icons/spinner.svg"),
    ),
    ("icons/info.svg", include_bytes!("../assets/icons/info.svg")),
    (
        "icons/holossh-glyph.svg",
        include_bytes!("../assets/icons/holossh-glyph.svg"),
    ),
];

impl AssetSource for HoloAssets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        for (asset_path, bytes) in ALL_ICONS {
            if *asset_path == path {
                return Ok(Some(Cow::Borrowed(*bytes)));
            }
        }
        Ok(None)
    }

    fn list(&self, path: &str) -> Result<Vec<SharedString>> {
        Ok(ALL_ICONS
            .iter()
            .filter(|(asset_path, _)| asset_path.starts_with(path))
            .map(|(asset_path, _)| SharedString::from(*asset_path))
            .collect())
    }
}
