//! 应用设置、多语言文案与持久化模型（对应 06 设置页与 07 首次引导页）

use crate::theme::AppearanceMode;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;

/// 支持的多语言选项（下拉框）
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum Language {
    ZhHans,
    ZhHant,
    #[default]
    English,
    Japanese,
    Korean,
}

impl Language {
    pub const ALL: [Self; 5] = [
        Self::ZhHans,
        Self::ZhHant,
        Self::English,
        Self::Japanese,
        Self::Korean,
    ];

    /// 将系统语言标签（如 `zh-Hans-CN`、`zh-Hant-TW`、`ja-JP`、`ko-KR`、`en-US`）映射至内置语言；
    /// 若为空或不在支持列表内，一律默认回退至 `English`。
    pub fn from_locale_tag(tag: &str) -> Self {
        let base = tag
            .split(['.', '@'])
            .next()
            .unwrap_or("")
            .trim()
            .to_ascii_lowercase()
            .replace('_', "-");
        if base.is_empty() || base == "c" || base == "posix" {
            return Self::English;
        }
        if base.starts_with("zh-hant")
            || base.starts_with("zh-tw")
            || base.starts_with("zh-hk")
            || base.starts_with("zh-mo")
        {
            return Self::ZhHant;
        }
        if base.starts_with("zh") {
            return Self::ZhHans;
        }
        if base.starts_with("ja") {
            return Self::Japanese;
        }
        if base.starts_with("ko") {
            return Self::Korean;
        }
        if base.starts_with("en") {
            return Self::English;
        }
        Self::English
    }

    /// 读取当前操作系统首选语言（macOS 优先读 `NSLocale.preferredLanguages`，取不到或不支持则默认 `English`）
    #[allow(unexpected_cfgs)]
    pub fn detect_system() -> Self {
        #[cfg(target_os = "macos")]
        {
            use objc::runtime::{Class, Object};
            use objc::{msg_send, sel, sel_impl};
            use std::ffi::CStr;
            use std::os::raw::c_char;

            unsafe {
                if let Some(cls) = Class::get("NSLocale") {
                    let langs: *mut Object = msg_send![cls, preferredLanguages];
                    if !langs.is_null() {
                        let count: usize = msg_send![langs, count];
                        if count > 0 {
                            let first: *mut Object = msg_send![langs, objectAtIndex: 0usize];
                            if !first.is_null() {
                                let c_ptr: *const c_char = msg_send![first, UTF8String];
                                if !c_ptr.is_null() {
                                    if let Ok(tag) = CStr::from_ptr(c_ptr).to_str() {
                                        return Self::from_locale_tag(tag);
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }

        for key in ["LC_ALL", "LC_MESSAGES", "LANG"] {
            if let Ok(val) = std::env::var(key) {
                if !val.trim().is_empty() {
                    return Self::from_locale_tag(&val);
                }
            }
        }
        Self::English
    }

    /// 对应 `assets/i18n/<code>.json` 配置文件代码
    pub fn code(self) -> &'static str {
        match self {
            Self::ZhHans => "zh-Hans",
            Self::ZhHant => "zh-Hant",
            Self::English => "en",
            Self::Japanese => "ja",
            Self::Korean => "ko",
        }
    }

    /// 从对应语言的 i18n 配置文件读取语言名称（`meta.language_name`）
    pub fn display_name(self) -> &'static str {
        crate::i18n::language_name(self)
    }

    /// 从对应语言的 i18n 配置文件读取翻译文案
    pub fn t(self, key: &str) -> &'static str {
        crate::i18n::t(self, key)
    }
}

/// 侧边栏位置
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum SidebarSide {
    #[default]
    Left,
    Right,
}

/// 磨砂玻璃生效区域（仅左侧 / 仅右侧 / 所有窗口）
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum GlassScope {
    #[default]
    LeftOnly,
    RightOnly,
    AllWindow,
}

/// 终端光标样式
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum CursorStyle {
    #[default]
    Block,
    Beam,
    Underline,
}

impl CursorStyle {
    pub fn label_zh(self) -> &'static str {
        match self {
            Self::Block => crate::i18n::tr("enum.cursor.block"),
            Self::Beam => crate::i18n::tr("enum.cursor.beam"),
            Self::Underline => crate::i18n::tr("enum.cursor.underline"),
        }
    }
}

/// 设置左侧导航模块（严格对应 06 画板：通用 / 连接 / 安全与同步 / 关于，无命令片段）
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SettingsSection {
    #[default]
    Appearance,
    Terminal,
    Shortcuts,
    Connections,
    Vault,
    Sync,
    About,
}

impl SettingsSection {
    pub fn title(self, lang: Language) -> &'static str {
        match self {
            Self::Appearance => lang.t("settings.sec.appearance"),
            Self::Terminal => lang.t("settings.sec.terminal"),
            Self::Shortcuts => lang.t("settings.sec.shortcuts"),
            Self::Connections => lang.t("settings.sec.connections"),
            Self::Vault => lang.t("settings.sec.vault"),
            Self::Sync => lang.t("settings.sec.sync"),
            Self::About => lang.t("settings.sec.about"),
        }
    }

    pub fn icon_path(self) -> &'static str {
        match self {
            Self::Appearance => "icons/sun.svg",
            Self::Terminal => "icons/terminal.svg",
            Self::Shortcuts => "icons/auth-interactive.svg",
            Self::Connections => "icons/connections.svg",
            Self::Vault => "icons/auth-key.svg",
            Self::Sync => "icons/sync.svg",
            Self::About => "icons/info.svg",
        }
    }
}

/// 设置页二级详情路由（macOS 系统设置风格：一级保持简洁，复杂表单与明细列表下沉至二级）
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettingsSubPage {
    VaultChangePassword,
    VaultHostSecrets,
    VaultLocalKeys,
    SyncGistConfig,
    SyncS3Config,
}

impl SettingsSubPage {
    pub fn title(self) -> &'static str {
        match self {
            Self::VaultChangePassword => crate::i18n::tr("settings.sub.vault_change_pwd"),
            Self::VaultHostSecrets => crate::i18n::tr("settings.sub.vault_host_secrets"),
            Self::VaultLocalKeys => crate::i18n::tr("settings.sub.vault_local_keys"),
            Self::SyncGistConfig => crate::i18n::tr("settings.sub.sync_gist"),
            Self::SyncS3Config => crate::i18n::tr("settings.sub.sync_s3"),
        }
    }

    pub fn anim_key(self) -> usize {
        match self {
            Self::VaultChangePassword => 101,
            Self::VaultHostSecrets => 102,
            Self::VaultLocalKeys => 103,
            Self::SyncGistConfig => 104,
            Self::SyncS3Config => 105,
        }
    }
}

/// 密钥库密码使用与记住策略（默认不记住密码，连接前在终端验证主密码）
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum VaultRememberPolicy {
    #[default]
    Never,
    Days1,
    Days3,
    Days5,
    Days7,
    Days15,
    Days30,
}

impl VaultRememberPolicy {
    pub const ALL: [Self; 7] = [
        Self::Never,
        Self::Days1,
        Self::Days3,
        Self::Days5,
        Self::Days7,
        Self::Days15,
        Self::Days30,
    ];

    pub fn label_zh(self) -> &'static str {
        match self {
            Self::Never => crate::i18n::tr("enum.vault_policy.never"),
            Self::Days1 => crate::i18n::tr("enum.vault_policy.days1"),
            Self::Days3 => crate::i18n::tr("enum.vault_policy.days3"),
            Self::Days5 => crate::i18n::tr("enum.vault_policy.days5"),
            Self::Days7 => crate::i18n::tr("enum.vault_policy.days7"),
            Self::Days15 => crate::i18n::tr("enum.vault_policy.days15"),
            Self::Days30 => crate::i18n::tr("enum.vault_policy.days30"),
        }
    }

    pub fn duration_secs(self) -> Option<u64> {
        let days = match self {
            Self::Never => return None,
            Self::Days1 => 1,
            Self::Days3 => 3,
            Self::Days5 => 5,
            Self::Days7 => 7,
            Self::Days15 => 15,
            Self::Days30 => 30,
        };
        Some(days * 86_400)
    }
}

/// 云端同步后端类型（支持 GitHub Gist 与 S3 兼容对象存储）
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum SyncProvider {
    #[default]
    Disabled,
    GitHubGist,
    S3,
}

impl SyncProvider {
    pub fn label_zh(self) -> &'static str {
        match self {
            Self::Disabled => crate::i18n::tr("enum.sync.disabled"),
            Self::GitHubGist => crate::i18n::tr("enum.sync.gist"),
            Self::S3 => crate::i18n::tr("enum.sync.s3"),
        }
    }
}

/// 支持用户自定义的全局/工作区快捷键动作枚举
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ShortcutAction {
    SearchHosts,
    OpenSettings,
    NewHost,
    NewTab,
    ClosePaneOrTab,
    FindInTerminal,
    SplitRight,
    SplitDown,
    ToggleSftp,
    ToggleSidebar,
    ZoomIn,
    ZoomOut,
    ZoomReset,
    Copy,
    Paste,
}

impl ShortcutAction {
    pub const ALL: [Self; 15] = [
        Self::SearchHosts,
        Self::OpenSettings,
        Self::NewHost,
        Self::NewTab,
        Self::ClosePaneOrTab,
        Self::FindInTerminal,
        Self::SplitRight,
        Self::SplitDown,
        Self::ToggleSftp,
        Self::ToggleSidebar,
        Self::ZoomIn,
        Self::ZoomOut,
        Self::ZoomReset,
        Self::Copy,
        Self::Paste,
    ];

    pub fn i18n_key(self) -> &'static str {
        match self {
            Self::SearchHosts => "settings.shortcuts.search",
            Self::OpenSettings => "settings.shortcuts.settings",
            Self::NewHost => "settings.shortcuts.new_host",
            Self::NewTab => "settings.shortcuts.new_tab",
            Self::ClosePaneOrTab => "settings.shortcuts.close_pane",
            Self::FindInTerminal => "settings.shortcuts.find",
            Self::SplitRight => "settings.shortcuts.split_h",
            Self::SplitDown => "settings.shortcuts.split_v",
            Self::ToggleSftp => "settings.shortcuts.sftp",
            Self::ToggleSidebar => "settings.shortcuts.sidebar",
            Self::ZoomIn => "settings.shortcuts.zoom_in",
            Self::ZoomOut => "settings.shortcuts.zoom_out",
            Self::ZoomReset => "settings.shortcuts.zoom_reset",
            Self::Copy => "settings.shortcuts.copy",
            Self::Paste => "settings.shortcuts.paste",
        }
    }

    pub fn id_slug(self) -> &'static str {
        match self {
            Self::SearchHosts => "search-hosts",
            Self::OpenSettings => "open-settings",
            Self::NewHost => "new-host",
            Self::NewTab => "new-tab",
            Self::ClosePaneOrTab => "close-pane",
            Self::FindInTerminal => "find-terminal",
            Self::SplitRight => "split-right",
            Self::SplitDown => "split-down",
            Self::ToggleSftp => "toggle-sftp",
            Self::ToggleSidebar => "toggle-sidebar",
            Self::ZoomIn => "zoom-in",
            Self::ZoomOut => "zoom-out",
            Self::ZoomReset => "zoom-reset",
            Self::Copy => "copy",
            Self::Paste => "paste",
        }
    }
}

/// 可持久化自定义快捷键映射（使用标准 GPUI 键位串如 `cmd-k`、`shift-cmd-d`）
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ShortcutBindings {
    pub search_hosts: String,
    pub open_settings: String,
    pub new_host: String,
    pub new_tab: String,
    pub close_pane_or_tab: String,
    pub find_in_terminal: String,
    pub split_right: String,
    pub split_down: String,
    pub toggle_sftp: String,
    pub toggle_sidebar: String,
    pub zoom_in: String,
    pub zoom_out: String,
    pub zoom_reset: String,
    pub copy: String,
    pub paste: String,
}

impl Default for ShortcutBindings {
    fn default() -> Self {
        Self {
            search_hosts: "cmd-k".into(),
            open_settings: "cmd-,".into(),
            new_host: "cmd-n".into(),
            new_tab: "cmd-t".into(),
            close_pane_or_tab: "cmd-w".into(),
            find_in_terminal: "cmd-f".into(),
            split_right: "cmd-d".into(),
            split_down: "shift-cmd-d".into(),
            toggle_sftp: "cmd-e".into(),
            toggle_sidebar: "cmd-b".into(),
            zoom_in: "cmd-=".into(),
            zoom_out: "cmd--".into(),
            zoom_reset: "cmd-0".into(),
            copy: "cmd-c".into(),
            paste: "cmd-v".into(),
        }
    }
}

impl ShortcutBindings {
    pub fn get(&self, action: ShortcutAction) -> &str {
        match action {
            ShortcutAction::SearchHosts => &self.search_hosts,
            ShortcutAction::OpenSettings => &self.open_settings,
            ShortcutAction::NewHost => &self.new_host,
            ShortcutAction::NewTab => &self.new_tab,
            ShortcutAction::ClosePaneOrTab => &self.close_pane_or_tab,
            ShortcutAction::FindInTerminal => &self.find_in_terminal,
            ShortcutAction::SplitRight => &self.split_right,
            ShortcutAction::SplitDown => &self.split_down,
            ShortcutAction::ToggleSftp => &self.toggle_sftp,
            ShortcutAction::ToggleSidebar => &self.toggle_sidebar,
            ShortcutAction::ZoomIn => &self.zoom_in,
            ShortcutAction::ZoomOut => &self.zoom_out,
            ShortcutAction::ZoomReset => &self.zoom_reset,
            ShortcutAction::Copy => &self.copy,
            ShortcutAction::Paste => &self.paste,
        }
    }

    pub fn set(&mut self, action: ShortcutAction, binding: String) {
        match action {
            ShortcutAction::SearchHosts => self.search_hosts = binding,
            ShortcutAction::OpenSettings => self.open_settings = binding,
            ShortcutAction::NewHost => self.new_host = binding,
            ShortcutAction::NewTab => self.new_tab = binding,
            ShortcutAction::ClosePaneOrTab => self.close_pane_or_tab = binding,
            ShortcutAction::FindInTerminal => self.find_in_terminal = binding,
            ShortcutAction::SplitRight => self.split_right = binding,
            ShortcutAction::SplitDown => self.split_down = binding,
            ShortcutAction::ToggleSftp => self.toggle_sftp = binding,
            ShortcutAction::ToggleSidebar => self.toggle_sidebar = binding,
            ShortcutAction::ZoomIn => self.zoom_in = binding,
            ShortcutAction::ZoomOut => self.zoom_out = binding,
            ShortcutAction::ZoomReset => self.zoom_reset = binding,
            ShortcutAction::Copy => self.copy = binding,
            ShortcutAction::Paste => self.paste = binding,
        }
    }

    /// 返回按 macOS 规范排布的快捷键展示文案（例如 `⇧⌘D`、`⌃⌥K`、`⌘+`）
    pub fn display_for(&self, action: ShortcutAction) -> String {
        format_shortcut_display(self.get(action))
    }

    /// 检查新录制的规范快捷键是否与其他动作冲突
    pub fn find_conflict(&self, target: ShortcutAction, canonical: &str) -> Option<ShortcutAction> {
        let norm = normalize_binding_string(canonical);
        for action in ShortcutAction::ALL {
            if action != target && normalize_binding_string(self.get(action)) == norm {
                return Some(action);
            }
        }
        None
    }

    /// 判断当前按键事件是否命中指定动作的自定义快捷键
    pub fn matches_keystroke(&self, action: ShortcutAction, ks: &gpui::Keystroke) -> bool {
        let Some(recorded) = keystroke_to_canonical_shortcut(ks) else {
            return false;
        };
        normalize_binding_string(&recorded) == normalize_binding_string(self.get(action))
    }
}

/// 判断是否为纯修饰键按下事件（录制快捷键时应忽略）
pub fn is_modifier_only_key(key: &str) -> bool {
    matches!(
        key.to_ascii_lowercase().as_str(),
        "shift"
            | "control"
            | "ctrl"
            | "alt"
            | "option"
            | "platform"
            | "command"
            | "cmd"
            | "meta"
            | "super"
            | "capslock"
            | "fn"
    )
}

fn is_function_key(key: &str) -> bool {
    matches!(
        key,
        "f1" | "f2" | "f3" | "f4" | "f5" | "f6" | "f7" | "f8" | "f9" | "f10" | "f11" | "f12"
    )
}

/// 将 GPUI `Keystroke` 转为规范的 GPUI 快捷键绑定串（`[ctrl-][alt-][shift-][cmd-]<key>`）
/// 要求至少携带一个主修饰键（⌘ / ⌃ / ⌥）或为 F1–F12 功能键，避免误绑单字母影响正常打字
pub fn keystroke_to_canonical_shortcut(ks: &gpui::Keystroke) -> Option<String> {
    let raw_key = ks.key.to_ascii_lowercase();
    if raw_key.is_empty() || is_modifier_only_key(&raw_key) || raw_key == "escape" {
        return None;
    }
    let ctrl = ks.modifiers.control;
    let alt = ks.modifiers.alt;
    let mut shift = ks.modifiers.shift;
    let cmd = ks.modifiers.platform;

    let key = if raw_key == "+" {
        shift = false;
        "=".to_string()
    } else {
        raw_key
    };

    if !(ctrl || alt || cmd || is_function_key(&key)) {
        return None;
    }

    let mut parts: Vec<&str> = Vec::with_capacity(5);
    if ctrl {
        parts.push("ctrl");
    }
    if alt {
        parts.push("alt");
    }
    if shift {
        parts.push("shift");
    }
    if cmd {
        parts.push("cmd");
    }
    parts.push(&key);
    Some(parts.join("-"))
}

/// 解析规范或任意顺序的 GPUI 快捷键串为 `(ctrl, alt, shift, cmd, key)`
pub fn parse_shortcut_binding(binding: &str) -> (bool, bool, bool, bool, String) {
    let s = binding.trim().to_ascii_lowercase();
    let (prefix, key) = if let Some(rest) = s.strip_suffix("--") {
        (rest, "-".to_string())
    } else if s == "-" {
        ("", "-".to_string())
    } else if let Some((left, right)) = s.rsplit_once('-') {
        (left, right.to_string())
    } else {
        ("", s)
    };
    let mut ctrl = false;
    let mut alt = false;
    let mut shift = false;
    let mut cmd = false;
    for tok in prefix.split('-').filter(|t| !t.is_empty()) {
        match tok {
            "ctrl" | "control" => ctrl = true,
            "alt" | "opt" | "option" => alt = true,
            "shift" => shift = true,
            "cmd" | "command" | "platform" | "super" => cmd = true,
            _ => {}
        }
    }
    (ctrl, alt, shift, cmd, key)
}

fn normalize_binding_string(binding: &str) -> String {
    let (ctrl, alt, shift, cmd, key) = parse_shortcut_binding(binding);
    let mut parts: Vec<&str> = Vec::with_capacity(5);
    if ctrl {
        parts.push("ctrl");
    }
    if alt {
        parts.push("alt");
    }
    if shift {
        parts.push("shift");
    }
    if cmd {
        parts.push("cmd");
    }
    parts.push(&key);
    parts.join("-")
}

/// 将 GPUI 快捷键绑定串格式化为 macOS 原生展示符号（`⌃⌥⇧⌘K`）
pub fn format_shortcut_display(binding: &str) -> String {
    let (ctrl, alt, shift, cmd, key) = parse_shortcut_binding(binding);
    let mut out = String::new();
    if ctrl {
        out.push('⌃');
    }
    if alt {
        out.push('⌥');
    }
    if shift {
        out.push('⇧');
    }
    if cmd {
        out.push('⌘');
    }
    let key_label: String = match key.as_str() {
        "=" => "+".into(),
        "-" => "-".into(),
        "space" => "Space".into(),
        "enter" | "return" => "↩".into(),
        "backspace" => "⌫".into(),
        "delete" => "⌦".into(),
        "tab" => "⇥".into(),
        "up" => "↑".into(),
        "down" => "↓".into(),
        "left" => "←".into(),
        "right" => "→".into(),
        "escape" => "Esc".into(),
        other => other.to_ascii_uppercase(),
    };
    out.push_str(&key_label);
    out
}

/// 持久化应用配置（每一项均在对应运行模块中真实生效）
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct AppSettings {
    pub onboarding_completed: bool,
    pub appearance: AppearanceMode,
    pub language: Language,
    pub glass_enabled: bool,
    pub glass_scope: GlassScope,
    pub terminal_opacity: f32,
    pub blur_radius: f32,
    pub sidebar_side: SidebarSide,
    pub sidebar_width: f32,
    pub sftp_width: f32,
    pub compact_host_list: bool,
    pub show_groups: bool,
    // 终端配置
    pub terminal_font_family: String,
    pub terminal_font_size: f32,
    pub cursor_style: CursorStyle,
    pub scrollback_lines: usize,
    pub copy_on_select: bool,
    pub trim_paste_newlines: bool,
    // 快捷键配置
    pub shortcuts_enabled: bool,
    pub shortcuts: ShortcutBindings,
    // 连接配置
    pub keepalive_secs: u64,
    pub connect_timeout_secs: u64,
    pub default_username: String,
    pub default_port: u16,
    pub auto_accept_host_key: bool,
    // 密钥库主密码记住策略（默认不记住密码）
    pub vault_remember_policy: VaultRememberPolicy,
    // 密钥库用户手动导入的额外私钥路径
    pub extra_key_paths: Vec<String>,
    // 用户在侧边栏手动创建的自定义分组文件夹（即使暂未添加主机也持久保留）
    pub custom_groups: Vec<String>,
    /// 自定义分组创建时间/更新时间（UTC 毫秒，用于跨设备合并）
    pub group_updated_at: BTreeMap<String, u64>,
    /// 已删除主机墓碑（host_id -> deleted_at_ms，避免跨设备拉取后“诈尸”）
    pub deleted_hosts: BTreeMap<String, u64>,
    /// 已删除自定义分组墓碑（group_name -> deleted_at_ms）
    pub deleted_groups: BTreeMap<String, u64>,
    // 云端同步配置（Token 与 S3 Secret 直接保存在本机应用配置中，与密钥库解耦且绝不上传云端）
    pub sync_provider: SyncProvider,
    pub sync_auto_on_change: bool,
    pub sync_auto_pull_on_start: bool,
    pub sync_base_revision: u64,
    /// 当前本机已绑定的云端同步纪元 UUID（跨设备强制覆盖时会变更以触发分叉保护弹窗）
    pub sync_epoch: String,
    /// 当前本机已绑定的云端密钥库盐指纹（检测其他设备更换密钥库）
    pub sync_vault_salt: String,
    pub gist_id: String,
    pub gist_filename: String,
    pub sync_gist_token: String,
    pub s3_endpoint: String,
    pub s3_region: String,
    pub s3_bucket: String,
    pub s3_object_key: String,
    pub s3_access_key_id: String,
    pub sync_s3_secret: String,
    pub last_sync_at: Option<String>,
}

impl Default for AppSettings {
    fn default() -> Self {
        Self {
            onboarding_completed: false,
            appearance: AppearanceMode::Dark,
            language: Language::detect_system(),
            glass_enabled: true,
            glass_scope: GlassScope::LeftOnly,
            terminal_opacity: 0.68,
            blur_radius: 44.0,
            sidebar_side: SidebarSide::Left,
            sidebar_width: crate::theme::LayoutMetrics::SIDEBAR_WIDTH,
            sftp_width: crate::theme::LayoutMetrics::SFTP_PANEL_WIDTH,
            compact_host_list: false,
            show_groups: true,
            terminal_font_family: "系统默认".into(),
            terminal_font_size: 13.0,
            cursor_style: CursorStyle::Block,
            scrollback_lines: 4000,
            copy_on_select: true,
            trim_paste_newlines: false,
            shortcuts_enabled: true,
            shortcuts: ShortcutBindings::default(),
            keepalive_secs: 30,
            connect_timeout_secs: 12,
            default_username: "root".into(),
            default_port: 22,
            auto_accept_host_key: true,
            vault_remember_policy: VaultRememberPolicy::Never,
            extra_key_paths: Vec::new(),
            custom_groups: Vec::new(),
            group_updated_at: BTreeMap::new(),
            deleted_hosts: BTreeMap::new(),
            deleted_groups: BTreeMap::new(),
            sync_provider: SyncProvider::Disabled,
            sync_auto_on_change: false,
            sync_auto_pull_on_start: true,
            sync_base_revision: 0,
            sync_epoch: String::new(),
            sync_vault_salt: String::new(),
            gist_id: String::new(),
            gist_filename: "holossh-sync.json".into(),
            sync_gist_token: String::new(),
            s3_endpoint: "https://s3.amazonaws.com".into(),
            s3_region: "us-east-1".into(),
            s3_bucket: String::new(),
            s3_object_key: "holossh/sync.json".into(),
            s3_access_key_id: String::new(),
            sync_s3_secret: String::new(),
            last_sync_at: None,
        }
    }
}

impl AppSettings {
    pub fn config_path() -> Option<PathBuf> {
        dirs::config_dir().map(|dir| dir.join("Holossh").join("settings.json"))
    }

    pub fn config_dir() -> Option<PathBuf> {
        dirs::config_dir().map(|dir| dir.join("Holossh"))
    }

    pub fn load_or_default() -> Self {
        let Some(path) = Self::config_path() else {
            return Self::default();
        };
        if let Some(parsed) = fs::read_to_string(&path)
            .ok()
            .and_then(|raw| serde_json::from_str::<Self>(&raw).ok())
        {
            return parsed;
        }
        let bak_path = path.with_extension("json.bak");
        fs::read_to_string(&bak_path)
            .ok()
            .and_then(|raw| serde_json::from_str::<Self>(&raw).ok())
            .unwrap_or_default()
    }

    pub fn save(&self) {
        use std::io::Write;
        let Some(path) = Self::config_path() else {
            return;
        };
        if let Some(parent) = path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        let Ok(json) = serde_json::to_string_pretty(self) else {
            return;
        };
        let tmp_path = path.with_extension("json.tmp");
        let bak_path = path.with_extension("json.bak");
        if let Ok(mut f) = fs::File::create(&tmp_path) {
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let _ = fs::set_permissions(&tmp_path, fs::Permissions::from_mode(0o600));
            }
            if f.write_all(json.as_bytes()).is_ok() && f.sync_all().is_ok() {
                if path.exists() {
                    let _ = fs::copy(&path, &bak_path);
                    #[cfg(unix)]
                    {
                        use std::os::unix::fs::PermissionsExt;
                        let _ = fs::set_permissions(&bak_path, fs::Permissions::from_mode(0o600));
                    }
                }
                let _ = fs::rename(&tmp_path, &path);
            } else {
                let _ = fs::remove_file(&tmp_path);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_roundtrip_and_i18n_work() {
        let mut s = AppSettings::default();
        s.language = Language::English;
        s.cursor_style = CursorStyle::Beam;
        s.connect_timeout_secs = 20;
        let json = serde_json::to_string(&s).unwrap();
        let decoded: AppSettings = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded.language, Language::English);
        assert_eq!(decoded.cursor_style, CursorStyle::Beam);
        assert_eq!(decoded.connect_timeout_secs, 20);
        assert_eq!(SettingsSection::Vault.title(Language::English), "Vault");
        assert_eq!(SettingsSection::Vault.title(Language::ZhHans), "密钥库");
    }

    #[test]
    fn parses_system_locale_tags_with_english_fallback() {
        assert_eq!(Language::from_locale_tag("zh-Hans-CN"), Language::ZhHans);
        assert_eq!(Language::from_locale_tag("zh_CN.UTF-8"), Language::ZhHans);
        assert_eq!(Language::from_locale_tag("zh-Hant-TW"), Language::ZhHant);
        assert_eq!(Language::from_locale_tag("zh-HK"), Language::ZhHant);
        assert_eq!(Language::from_locale_tag("ja-JP"), Language::Japanese);
        assert_eq!(Language::from_locale_tag("ko_KR.UTF-8"), Language::Korean);
        assert_eq!(Language::from_locale_tag("en-US"), Language::English);
        assert_eq!(Language::from_locale_tag("fr-FR"), Language::English);
        assert_eq!(Language::from_locale_tag("de-DE"), Language::English);
        assert_eq!(Language::from_locale_tag("C"), Language::English);
        assert_eq!(Language::from_locale_tag(""), Language::English);
    }

    #[test]
    fn formats_and_detects_custom_shortcut_bindings() {
        let mut bindings = ShortcutBindings::default();
        assert_eq!(bindings.display_for(ShortcutAction::SearchHosts), "⌘K");
        assert_eq!(bindings.display_for(ShortcutAction::SplitDown), "⇧⌘D");
        assert_eq!(bindings.display_for(ShortcutAction::ZoomIn), "⌘+");
        assert_eq!(bindings.display_for(ShortcutAction::ZoomOut), "⌘-");
        assert_eq!(format_shortcut_display("ctrl-alt-shift-cmd-p"), "⌃⌥⇧⌘P");

        assert_eq!(
            bindings.find_conflict(ShortcutAction::SearchHosts, "cmd-t"),
            Some(ShortcutAction::NewTab)
        );
        assert_eq!(bindings.find_conflict(ShortcutAction::SearchHosts, "cmd-p"), None);

        bindings.set(ShortcutAction::SearchHosts, "alt-cmd-p".into());
        assert_eq!(bindings.display_for(ShortcutAction::SearchHosts), "⌥⌘P");
    }
}
