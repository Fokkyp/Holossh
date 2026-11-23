//! macOS 原生窗口真实高斯磨砂模糊控制（CoreGraphics WindowServer API）
//!
//! 说明：GPUI 0.2.2 在 macOS 12+ 使用 `NSVisualEffectMaterial::Selection` 并移除背景图层，
//! 在 macOS 26/27 下会导致背景只透明不模糊；此处改为 `WindowBackgroundAppearance::Transparent`
//! 配合 CoreGraphics `CGSSetWindowBackgroundBlurRadius`，实现全窗口统一、可实时调节像素半径的真实硬件高斯模糊。

use gpui::{Window, WindowBackgroundAppearance};

#[cfg(target_os = "macos")]
#[link(name = "CoreGraphics", kind = "framework")]
unsafe extern "C" {
    fn CGSMainConnectionID() -> u32;
    fn CGSSetWindowBackgroundBlurRadius(cid: u32, wid: isize, radius: i64) -> i32;
}

#[cfg(target_os = "macos")]
#[link(name = "Carbon", kind = "framework")]
unsafe extern "C" {
    fn TISCopyCurrentASCIICapableKeyboardInputSource() -> *mut std::ffi::c_void;
    fn TISSelectInputSource(input_source: *mut std::ffi::c_void) -> i32;
}

#[cfg(target_os = "macos")]
#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    fn CFRelease(cf: *mut std::ffi::c_void);
}

/// 将 macOS 当前输入源切换至用户首选的 ASCII / 英文键盘布局（仅在焦点进入终端时触发一次，不阻止用户随后手动切换其他输入法）
pub fn switch_to_ascii_input_source() {
    #[cfg(target_os = "macos")]
    unsafe {
        let ascii_src = TISCopyCurrentASCIICapableKeyboardInputSource();
        if !ascii_src.is_null() {
            let _ = TISSelectInputSource(ascii_src);
            CFRelease(ascii_src);
        }
    }
}

/// 同步窗口磨砂开关与真实背景高斯模糊半径（px）
#[allow(unexpected_cfgs)]
pub fn apply_window_glass(window: &Window, glass_enabled: bool, blur_radius: f32) {
    if glass_enabled {
        window.set_background_appearance(WindowBackgroundAppearance::Transparent);
    } else {
        window.set_background_appearance(WindowBackgroundAppearance::Opaque);
    }

    #[cfg(target_os = "macos")]
    {
        use objc::runtime::Object;
        use objc::{msg_send, sel, sel_impl};
        use raw_window_handle::{HasWindowHandle, RawWindowHandle};

        let radius_px: i64 = if glass_enabled {
            blur_radius.round().clamp(1.0, 80.0) as i64
        } else {
            0
        };

        if let Ok(handle) = HasWindowHandle::window_handle(window) {
            if let RawWindowHandle::AppKit(appkit) = handle.as_raw() {
                unsafe {
                    let ns_view = appkit.ns_view.as_ptr() as *mut Object;
                    if !ns_view.is_null() {
                        let ns_window: *mut Object = msg_send![ns_view, window];
                        if !ns_window.is_null() {
                            let wid: isize = msg_send![ns_window, windowNumber];
                            if wid > 0 {
                                let cid = CGSMainConnectionID();
                                let _ = CGSSetWindowBackgroundBlurRadius(cid, wid, radius_px);
                            }
                        }
                    }
                }
            }
        }
    }
}

/// 同步 macOS Dock / 应用切换器（⌘Tab）图标，随应用深浅外观即时切换
#[allow(unexpected_cfgs)]
pub fn apply_app_icon(is_dark: bool) {
    #[cfg(target_os = "macos")]
    {
        use objc::runtime::{Class, Object};
        use objc::{msg_send, sel, sel_impl};

        let bytes = if is_dark {
            crate::assets::APP_ICON_DARK_PNG
        } else {
            crate::assets::APP_ICON_LIGHT_PNG
        };

        unsafe {
            let Some(ns_data_cls) = Class::get("NSData") else {
                return;
            };
            let Some(ns_image_cls) = Class::get("NSImage") else {
                return;
            };
            let Some(ns_app_cls) = Class::get("NSApplication") else {
                return;
            };

            let data: *mut Object = msg_send![
                ns_data_cls,
                dataWithBytes: bytes.as_ptr() as *const std::ffi::c_void
                length: bytes.len()
            ];
            if data.is_null() {
                return;
            }

            let image_alloc: *mut Object = msg_send![ns_image_cls, alloc];
            let image: *mut Object = msg_send![image_alloc, initWithData: data];
            if image.is_null() {
                return;
            }

            let app: *mut Object = msg_send![ns_app_cls, sharedApplication];
            if !app.is_null() {
                let _: () = msg_send![app, setApplicationIconImage: image];
            }
            let _: () = msg_send![image, release];
        }
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = is_dark;
    }
}
