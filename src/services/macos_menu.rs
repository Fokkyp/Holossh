//! macOS 顶部原生系统菜单栏与 Dock 右键菜单（支持五语实时同步与系统快捷键显示）

use crate::state::settings::{Language, ShortcutBindings};
use gpui::{App, KeyBinding, Menu, MenuItem, SystemMenuType};

gpui::actions!(
    holossh,
    [
        MenuAbout,
        MenuSettings,
        MenuHide,
        MenuHideOthers,
        MenuShowAll,
        MenuQuit,
        MenuNewHost,
        MenuNewGroup,
        MenuNewTab,
        MenuClosePaneOrTab,
        MenuCut,
        MenuCopy,
        MenuPaste,
        MenuSelectAll,
        MenuSearchHosts,
        MenuFindInTerminal,
        MenuClearTerminal,
        MenuSplitRight,
        MenuSplitDown,
        MenuToggleSftp,
        MenuToggleSidebar,
        MenuToggleAppearance,
        MenuZoomIn,
        MenuZoomOut,
        MenuZoomReset,
        MenuToggleFullscreen,
        MenuMinimize,
        MenuZoomWindow,
    ]
);

/// 启动时注册一次应用级无窗口依赖的系统动作（退出/隐藏）
pub fn register_system_actions(cx: &mut App) {
    cx.on_action(|_: &MenuQuit, cx| cx.quit());
    cx.on_action(|_: &MenuHide, cx| cx.hide());
    cx.on_action(|_: &MenuHideOthers, cx| cx.hide_other_apps());
    cx.on_action(|_: &MenuShowAll, cx| cx.unhide_other_apps());
}

/// 录制自定义快捷键期间临时挂起应用快捷键，防止按下 `⌘,` / `⌘C` / `⌘V` 时被菜单动作截走
pub fn suspend_custom_keybindings(cx: &mut App) {
    cx.clear_key_bindings();
    cx.bind_keys([KeyBinding::new("cmd-q", MenuQuit, None)]);
}

/// 按当前用户自定义快捷键实时重建 GPUI 键位表（同步驱动顶部原生菜单栏右侧的 macOS 快捷键徽标）
pub fn rebind_keybindings(shortcuts: &ShortcutBindings, cx: &mut App) {
    cx.clear_key_bindings();

    let mut bindings: Vec<KeyBinding> = vec![
        // 系统标准快捷键
        KeyBinding::new("cmd-q", MenuQuit, None),
        KeyBinding::new("cmd-h", MenuHide, None),
        KeyBinding::new("cmd-alt-h", MenuHideOthers, None),
        KeyBinding::new("cmd-m", MenuMinimize, None),
        KeyBinding::new("ctrl-cmd-f", MenuToggleFullscreen, None),
        // 全局可自定义快捷键
        KeyBinding::new(&shortcuts.open_settings, MenuSettings, None),
        KeyBinding::new("cmd-x", MenuCut, None),
        KeyBinding::new(&shortcuts.copy, MenuCopy, None),
        KeyBinding::new(&shortcuts.paste, MenuPaste, None),
        KeyBinding::new("cmd-a", MenuSelectAll, None),
        // 工作区可自定义快捷键（顶部菜单栏自动展示对应键位符号）
        KeyBinding::new(&shortcuts.new_host, MenuNewHost, Some("Workspace")),
        KeyBinding::new(&shortcuts.new_tab, MenuNewTab, Some("Workspace")),
        KeyBinding::new(&shortcuts.close_pane_or_tab, MenuClosePaneOrTab, Some("Workspace")),
        KeyBinding::new(&shortcuts.search_hosts, MenuSearchHosts, Some("Workspace")),
        KeyBinding::new(&shortcuts.find_in_terminal, MenuFindInTerminal, Some("Workspace")),
        KeyBinding::new(&shortcuts.split_right, MenuSplitRight, Some("Workspace")),
        KeyBinding::new(&shortcuts.split_down, MenuSplitDown, Some("Workspace")),
        KeyBinding::new(&shortcuts.toggle_sftp, MenuToggleSftp, Some("Workspace")),
        KeyBinding::new(&shortcuts.toggle_sidebar, MenuToggleSidebar, Some("Workspace")),
        KeyBinding::new(&shortcuts.zoom_in, MenuZoomIn, Some("Workspace")),
        KeyBinding::new(&shortcuts.zoom_out, MenuZoomOut, Some("Workspace")),
        KeyBinding::new(&shortcuts.zoom_reset, MenuZoomReset, Some("Workspace")),
    ];

    // 当放大字号绑定为默认 `cmd-=` 时，同时兼容 `cmd-+`
    if shortcuts.zoom_in == "cmd-=" {
        bindings.push(KeyBinding::new("cmd-+", MenuZoomIn, Some("Workspace")));
    }

    cx.bind_keys(bindings);
}

/// 按当前界面语言刷新 macOS 顶部原生菜单栏与 Dock 右键菜单
pub fn apply_app_menus(lang: Language, cx: &mut App) {
    let t = |key: &str| lang.t(key).to_string();

    cx.set_menus(vec![
        Menu {
            name: "Holossh".into(),
            items: vec![
                MenuItem::action(t("menu.about"), MenuAbout),
                MenuItem::separator(),
                MenuItem::action(t("menu.settings"), MenuSettings),
                MenuItem::separator(),
                MenuItem::os_submenu(t("menu.services"), SystemMenuType::Services),
                MenuItem::separator(),
                MenuItem::action(t("menu.hide"), MenuHide),
                MenuItem::action(t("menu.hide_others"), MenuHideOthers),
                MenuItem::action(t("menu.show_all"), MenuShowAll),
                MenuItem::separator(),
                MenuItem::action(t("menu.quit"), MenuQuit),
            ],
        },
        Menu {
            name: t("menu.file").into(),
            items: vec![
                MenuItem::action(t("menu.new_host"), MenuNewHost),
                MenuItem::action(t("menu.new_group"), MenuNewGroup),
                MenuItem::action(t("menu.new_tab"), MenuNewTab),
                MenuItem::separator(),
                MenuItem::action(t("menu.close_pane_or_tab"), MenuClosePaneOrTab),
            ],
        },
        Menu {
            name: t("menu.edit").into(),
            items: vec![
                MenuItem::action(t("menu.cut"), MenuCut),
                MenuItem::action(t("menu.copy"), MenuCopy),
                MenuItem::action(t("menu.paste"), MenuPaste),
                MenuItem::action(t("menu.select_all"), MenuSelectAll),
                MenuItem::separator(),
                MenuItem::action(t("menu.search_hosts"), MenuSearchHosts),
                MenuItem::action(t("menu.find_in_terminal"), MenuFindInTerminal),
                MenuItem::separator(),
                MenuItem::action(t("menu.clear_terminal"), MenuClearTerminal),
            ],
        },
        Menu {
            name: t("menu.session").into(),
            items: vec![
                MenuItem::action(t("menu.split_right"), MenuSplitRight),
                MenuItem::action(t("menu.split_down"), MenuSplitDown),
                MenuItem::separator(),
                MenuItem::action(t("menu.toggle_sftp"), MenuToggleSftp),
            ],
        },
        Menu {
            name: t("menu.view").into(),
            items: vec![
                MenuItem::action(t("menu.toggle_sidebar"), MenuToggleSidebar),
                MenuItem::action(t("menu.toggle_appearance"), MenuToggleAppearance),
                MenuItem::separator(),
                MenuItem::action(t("menu.zoom_in"), MenuZoomIn),
                MenuItem::action(t("menu.zoom_out"), MenuZoomOut),
                MenuItem::action(t("menu.zoom_reset"), MenuZoomReset),
                MenuItem::separator(),
                MenuItem::action(t("menu.toggle_fullscreen"), MenuToggleFullscreen),
            ],
        },
        Menu {
            name: t("menu.window").into(),
            items: vec![
                MenuItem::action(t("menu.minimize"), MenuMinimize),
                MenuItem::action(t("menu.zoom_window"), MenuZoomWindow),
            ],
        },
    ]);

    cx.set_dock_menu(vec![
        MenuItem::action(t("menu.new_host"), MenuNewHost),
        MenuItem::action(t("menu.settings"), MenuSettings),
    ]);
}
