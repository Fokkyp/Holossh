//! Holossh — 原生 macOS 27 磨砂玻璃 SSH 客户端入口
//!
//! 严格基于 Rust GPUI 框架（https://gpui.rs/）启动原生 Metal + macOS Blurred 磨砂窗口。

mod app;
mod assets;
mod i18n;
mod services;
mod state;
mod theme;
mod ui;

use crate::app::HoloSshApp;
use crate::assets::HoloAssets;
use crate::theme::LayoutMetrics;
use gpui::{
    App, AppContext, Application, Bounds, TitlebarOptions, WindowBackgroundAppearance,
    WindowBounds, WindowOptions, point, px, size,
};

fn main() {
    Application::new()
        .with_assets(HoloAssets)
        .run(|cx: &mut App| {
            // 注册应用级退出/隐藏系统动作
            services::macos_menu::register_system_actions(cx);

            let bounds = Bounds::centered(
                None,
                size(
                    px(LayoutMetrics::WINDOW_WIDTH),
                    px(LayoutMetrics::WINDOW_HEIGHT),
                ),
                cx,
            );

            let options = WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                titlebar: Some(TitlebarOptions {
                    title: None,
                    appears_transparent: true,
                    // 对齐左侧 48px 标题栏中轴（y=24pt），与主题切换、侧栏展开图标及标签栏像素级垂直居中
                    traffic_light_position: Some(point(px(16.0), px(17.0))),
                }),
                // 启动即采用透明底 + CoreGraphics 真实高斯模糊（由 macos_glass 动态接管半径）
                window_background: WindowBackgroundAppearance::Transparent,
                ..Default::default()
            };

            cx.open_window(options, |window, cx| {
                // 启动时将已下载到应用私有目录的内置字体注册至 GPUI 进程内存（绝不安装到系统）
                let cached_fonts = services::fonts::load_all_downloaded_font_bytes();
                if !cached_fonts.is_empty() {
                    let _ = window.text_system().add_fonts(cached_fonts);
                }

                let entity = cx.new(|cx| HoloSshApp::new(cx));
                let (focus_handle, glass_enabled, blur_radius, initial_lang, initial_shortcuts) = {
                    let app = entity.read(cx);
                    (
                        app.focus_handle.clone(),
                        app.settings.glass_enabled,
                        app.settings.blur_radius,
                        app.settings.language,
                        app.settings.shortcuts.clone(),
                    )
                };
                services::macos_menu::rebind_keybindings(&initial_shortcuts, cx);
                services::macos_menu::apply_app_menus(initial_lang, cx);
                services::macos_glass::apply_window_glass(window, glass_enabled, blur_radius);
                window.focus(&focus_handle);
                entity
            })
            .expect("failed to open Holossh main window");

            cx.activate(true);
        });
}
