//! 顶层应用根视图与全量真实能力状态调度（纯 Rust + GPUI）
//!
//! 统一协调：
//! - 后台 Tokio SSHv2 / SFTP 会话池与 60Hz 事件泵
//! - 多标签、递归分屏独立 `TerminalEmulator` 实例
//! - 全局键盘输入路由（终端 ANSI 键位、搜索框实时过滤、新建主机表单与完整 macOS ⌘ 快捷键）
//! - 设置页 7 大模块全部真实业务逻辑（外观/系统深浅色/语言/磨砂材质、终端字体光标、快捷键开关、连接策略与配置导入、加密密钥库与私钥导入、云端同步与 JSON 导入导出）

use crate::i18n::{self, tr, trf};
use crate::services::fonts::{self, FONT_CATALOG, FontDownloadEvent};
use crate::services::local_pty::spawn_local_pty_session;
use crate::services::ssh_keys;
use crate::services::macos_glass;
use crate::services::macos_menu::{
    self, MenuAbout, MenuClearTerminal, MenuClosePaneOrTab, MenuCopy, MenuCut, MenuFindInTerminal,
    MenuMinimize, MenuNewGroup, MenuNewHost, MenuNewTab, MenuPaste, MenuSearchHosts,
    MenuSelectAll, MenuSettings, MenuSplitDown, MenuSplitRight, MenuToggleAppearance,
    MenuToggleFullscreen, MenuToggleSftp, MenuToggleSidebar, MenuZoomIn, MenuZoomOut,
    MenuZoomReset, MenuZoomWindow,
};
use crate::services::ssh::{PaneSessionHandle, ResolvedJumpHop, SshEvent, spawn_ssh_session};
use crate::services::ssh_config::load_local_ssh_config_hosts;
use crate::services::sync::{
    self, EncryptedSyncSnapshot, SyncAction, SyncCatalog, SyncConflictReason, SyncEvent,
    SyncRequest, spawn_sync_task,
};
use crate::services::terminal_emu::{TermSearchMatch, TerminalEmulator};
use crate::services::vault::{self, VaultData, VaultSecretEntry};
use crate::state::host::{
    AuthMethod, ConnectionKind, HostProfile, HostStatus, generate_host_uuid, now_unix_ms,
};
use crate::state::session::{SessionTab, SplitAxis, TerminalPaneState};
use crate::state::settings::{
    AppSettings, CursorStyle, GlassScope, Language, SettingsSection, SettingsSubPage,
    ShortcutAction, ShortcutBindings, SidebarSide, SyncProvider, VaultRememberPolicy,
    format_shortcut_display, is_modifier_only_key, keystroke_to_canonical_shortcut,
};
use crate::state::sftp::{SftpPanelState, TransferDirection, TransferTask};
use crate::theme::{AppearanceMode, ThemeTokens};
use crate::ui::components::{
    TextCursorState, TextKeyOutcome, ToastItem, ToastKind, circular_progress_ring,
    current_ime_caret_bounds, hairline_h, hit_test_text_input, icon_svg, render_toast_stack,
    reset_caret_blink_epoch, set_active_ime_marked_text, sync_active_text_input_render_state,
};
use crate::ui::modals::new_host_modal::render_new_host_modal;
use crate::ui::modals::sync_conflict_modal::render_sync_conflict_modal;
use crate::ui::modals::vault_modal::render_vault_modal;
use crate::ui::onboarding::render_onboarding_view;
use crate::ui::settings::render_settings_view;
use crate::ui::workspace::{
    empty_state::render_empty_workspace_pane,
    sftp_panel::render_sftp_panel,
    sidebar::render_sidebar,
    terminal_element::{HoveredLinkSpan, SharedPaneMetrics, SharedPaneShapes},
    terminal_pane::{TerminalRenderParams, render_split_tree},
    titlebar::render_titlebar,
};
use alacritty_terminal::index::Side;
use alacritty_terminal::selection::SelectionType;
use alacritty_terminal::term::TermMode;
use gpui::prelude::FluentBuilder;
use gpui::{
    Animation, AnimationExt, Bounds, ClipboardItem, Context, ElementInputHandler, EntityInputHandler,
    FocusHandle, InteractiveElement, IntoElement, KeyDownEvent, MouseButton, MouseDownEvent,
    MouseMoveEvent, MouseUpEvent, ParentElement, PathPromptOptions, Pixels, Point, Render,
    ScrollDelta, ScrollStrategy, ScrollWheelEvent, SharedString, StatefulInteractiveElement,
    Styled, UTF16Selection, UniformListScrollHandle, Window, WindowAppearance, canvas, div,
    ease_out_quint, point, px, size,
};
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::fs;
use std::ops::Range;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant};
use zeroize::Zeroize;

/// 当前主窗口所处的顶层视图
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppScreen {
    Onboarding,
    Workspace,
    Settings,
}

/// 当前键盘焦点所处的输入目标
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum ActiveInput {
    #[default]
    Terminal,
    TerminalSearch,
    SidebarSearch,
    SidebarNewGroup,
    SidebarRenameGroup,
    SftpNewDir,
    ModalName,
    ModalHost,
    ModalPort,
    ModalProxyHost,
    ModalProxyPort,
    ModalUser,
    ModalPassword,
    ModalKeyPath,
    SyncGistToken,
    SyncGistId,
    SyncGistFilename,
    SyncS3Endpoint,
    SyncS3Region,
    SyncS3Bucket,
    SyncS3Key,
    SyncS3AccessKey,
    SyncS3SecretKey,
    VaultModalPassword,
    VaultModalConfirm,
    VaultSetupPassword,
    VaultSetupConfirm,
    VaultUnlockPassword,
    VaultChangeOld,
    VaultChangeNew,
    VaultChangeConfirm,
    SyncConflictPassword,
}

/// 密钥库设置/解锁弹窗完成后自动衔接的后续动作
#[derive(Debug, Clone)]
pub enum VaultModalNextAction {
    OpenNewHost,
    OpenEditHost(String),
    TriggerSyncPush,
    TriggerSyncForcePush,
    ApplyPendingSyncPull,
}

/// 侧栏底部云同步状态图标展示态
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CloudSyncVisualState {
    Disabled,
    IdleSynced,
    Uploading,
    Downloading,
    Failed,
}

/// 侧栏主机长按拖拽的落点目标（置顶 / 插入指定行前后 / 移入指定文件夹）
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HostDropTarget {
    TopRoot,
    Folder(String),
    InsertBefore { host_id: String, group: String },
    InsertAfter { host_id: String, group: String },
}

/// 侧栏渲染阶段采集的拖拽命中热区
#[derive(Debug, Clone)]
pub enum HostDropZone {
    TopHeader,
    FolderHeader {
        group: String,
    },
    HostRow {
        host_id: String,
        group: String,
        index_in_group: usize,
    },
}

/// 精确跟随鼠标尖的绝对坐标插入横条几何信息
#[derive(Debug, Clone, Copy)]
pub struct DropInsertionLine {
    pub x: f32,
    pub y: f32,
    pub width: f32,
}

/// 侧栏主机长按拖拽状态
#[derive(Debug, Clone)]
pub struct HostDragState {
    pub host_id: String,
    pub host_name: String,
    pub host_ip: String,
    pub connected: bool,
    pub start_pos: Point<Pixels>,
    pub current_pos: Point<Pixels>,
    pub pressed_at: Instant,
    /// 长按超过 220ms（或按住移动）后激活正式拖拽态
    pub active: bool,
    pub hover_target: Option<HostDropTarget>,
    pub insertion_line: Option<DropInsertionLine>,
}

/// 同步冲突弹窗内嵌主密码验证后的执行动作
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyncConflictPasswordAction {
    OverwriteLocalFromCloud,
    MergeBoth,
}

/// 全局同步冲突 / 首次绑定决策弹窗状态（挂接于根视图，任意界面均可弹出）
#[derive(Debug, Clone)]
pub struct SyncConflictModalState {
    pub reason: SyncConflictReason,
    pub snapshot: EncryptedSyncSnapshot,
    pub same_vault_salt: bool,
    pub remote_time_display: String,
    pub local_host_count: usize,
    pub password_action: Option<SyncConflictPasswordAction>,
    pub password: String,
    pub show_password: bool,
    pub error: Option<String>,
}

/// 密钥库首次设置 / 解锁弹窗草稿
#[derive(Debug, Clone)]
pub struct VaultModalDraft {
    pub is_setup: bool,
    pub next_action: VaultModalNextAction,
    pub password: String,
    pub confirm_password: String,
    pub show_password: bool,
}

/// 设置页「密钥库」表单草稿（首次设密 / 解锁 / 修改密码）
#[derive(Debug, Clone, Default)]
pub struct VaultSettingsForm {
    pub setup_password: String,
    pub setup_confirm: String,
    pub unlock_password: String,
    pub change_old: String,
    pub change_new: String,
    pub change_confirm: String,
    pub show_passwords: bool,
}

/// 不记住密码策略下，在终端中等待用户输入密钥库密码的状态
#[derive(Debug, Clone)]
struct PendingVaultConnect {
    host: HostProfile,
    input_buf: String,
    started_at: Instant,
    timeout_secs: u64,
}

/// SSH 认证阶段（2FA/OTP、键盘交互、未存密码或私钥口令）在终端内等待用户输入的状态
#[derive(Debug, Clone)]
struct PendingAuthPrompt {
    prompt_id: u64,
    input_buf: String,
    echo: bool,
}

/// SSH 握手阶段等待用户在终端内确认未知主机指纹或变更告警的状态
#[derive(Debug, Clone)]
struct PendingHostKeyPrompt {
    prompt_id: u64,
    hostname: String,
    port: u16,
    input_buf: String,
}

/// 意外断线后的指数退避自动重连计划（2s -> 4s -> 8s，最多 3 次）
#[derive(Debug, Clone)]
struct PendingReconnect {
    host_id: String,
    next_at: Instant,
}

/// 主机行右键上下文菜单状态
#[derive(Debug, Clone)]
pub struct HostContextMenu {
    pub host_id: String,
    pub position: Point<Pixels>,
}

/// 文件夹（分组）右键上下文菜单状态
#[derive(Debug, Clone)]
pub struct GroupContextMenu {
    pub group_name: String,
    pub position: Point<Pixels>,
}

/// SFTP 文件行右键上下文菜单状态
#[derive(Debug, Clone)]
pub struct SftpContextMenu {
    pub entry_name: String,
    pub is_dir: bool,
    pub position: Point<Pixels>,
}

/// 终端内 `⌘F` 搜索浮条状态
#[derive(Debug, Clone)]
pub struct TerminalSearchState {
    pub pane_id: String,
    pub query: String,
    pub case_sensitive: bool,
    pub is_regex: bool,
    pub matches: Arc<Vec<TermSearchMatch>>,
    pub active_index: Option<usize>,
}

/// 多行粘贴安全确认条状态（非备用屏粘贴含换行文本时二次确认，防误触发生产命令）
#[derive(Debug, Clone)]
pub struct PendingPasteConfirm {
    pub pane_id: String,
    pub payload: String,
    pub line_count: usize,
    pub preview: String,
}

/// 单次鼠标拖选锚点（仅在真正移动跨越字符后才启动原生选区，确保点击单点绝不产生高亮条）
#[derive(Debug, Clone)]
struct MouseDragAnchor {
    pane_id: String,
    col: usize,
    row: usize,
    side: Side,
    ty: SelectionType,
    started_selection: bool,
}

/// 终端内右键上下文菜单（支持复制已选文本、粘贴与查找）
#[derive(Debug, Clone)]
pub struct TerminalContextMenu {
    pub pane_id: String,
    pub position: Point<Pixels>,
}

/// 分割线拖拽类型（侧栏中线 / SFTP 抽屉中线 / 递归分屏终端中线）
#[derive(Debug, Clone)]
pub enum DragKind {
    Sidebar,
    Sftp,
    SplitPane { path: String, axis: SplitAxis },
}

/// 当前正在进行的鼠标拖拽状态
#[derive(Debug, Clone)]
pub struct ActiveDrag {
    pub kind: DragKind,
    pub start_pos: Point<Pixels>,
    pub start_value: f32,
}

/// 全局悬浮下拉菜单类型（覆盖设置页、新建/编辑主机弹窗与引导页全部下拉项）
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DropdownKind {
    Language,
    TerminalFont,
    ScrollbackLines,
    KeepAlive,
    ConnectTimeout,
    DefaultUsername,
    DefaultPort,
    VaultRememberPolicy,
    DraftGroup,
    DraftConnection,
    DraftJumpHost,
}

/// 当前展开的磨砂悬浮下拉菜单
#[derive(Debug, Clone)]
pub struct ActiveDropdown {
    pub kind: DropdownKind,
    pub position: Point<Pixels>,
    pub width: f32,
}

/// 新建/编辑主机 Sheet 草稿状态（对应 02 画板，跳板机/代理交互对齐 Tabby）
#[derive(Debug, Clone)]
pub struct NewHostDraft {
    pub name: String,
    pub group: String,
    pub connection: ConnectionKind,
    pub jump_host_id: Option<String>,
    pub proxy_host: String,
    pub proxy_port: String,
    pub hostname: String,
    pub port: String,
    pub username: String,
    pub auth_method: AuthMethod,
    pub password: String,
    pub show_password: bool,
    pub private_key_path: String,
}

impl NewHostDraft {
    pub fn from_settings(settings: &AppSettings) -> Self {
        let discovered_key = ssh_keys::discover_local_ssh_keys()
            .into_iter()
            .next()
            .map(|p| abbreviate_home_path(&p))
            .unwrap_or_default();
        Self {
            name: String::new(),
            group: String::new(),
            connection: ConnectionKind::Direct,
            jump_host_id: None,
            proxy_host: String::new(),
            proxy_port: String::new(),
            hostname: String::new(),
            port: settings.default_port.to_string(),
            username: settings.default_username.clone(),
            auth_method: AuthMethod::Auto,
            password: String::new(),
            show_password: false,
            private_key_path: discovered_key,
        }
    }
}

/// HoloSSH 根视图实体
pub struct HoloSshApp {
    pub focus_handle: FocusHandle,
    pub active_input: ActiveInput,
    pub search_query: String,
    pub creating_group: bool,
    pub new_group_name: String,
    pub renaming_group: Option<String>,
    pub rename_group_input: String,
    pub group_context_menu: Option<GroupContextMenu>,
    pub deleting_group_confirm: Option<String>,
    pub screen: AppScreen,
    pub settings: AppSettings,
    pub active_settings_section: SettingsSection,
    pub active_settings_subpage: Option<SettingsSubPage>,
    pub recording_shortcut: Option<ShortcutAction>,
    /// 全局顶部居中 Toast 队列（多条叠加，各自独立定时自动隐藏，支持手动关闭）
    pub toasts: Vec<ToastItem>,
    next_toast_id: u64,
    /// 跨平台加密密钥库内存解锁态（仅在已解锁时存在，锁定时 zeroize 清零）
    vault_key: Option<[u8; 32]>,
    vault_data: Option<VaultData>,
    pub vault_revealed_host_id: Option<String>,
    pub vault_settings_form: VaultSettingsForm,
    pub vault_modal: Option<VaultModalDraft>,
    pub sync_conflict_modal: Option<SyncConflictModalState>,
    pending_vault_connects: HashMap<String, PendingVaultConnect>,
    pending_auth_prompts: HashMap<String, PendingAuthPrompt>,
    pending_host_key_prompts: HashMap<String, PendingHostKeyPrompt>,
    pending_reconnects: HashMap<String, PendingReconnect>,
    pane_reconnect_attempts: HashMap<String, u8>,
    /// 密钥库页缓存：本机 `~/.ssh/` 发现的私钥路径
    pub vault_local_keys: Vec<PathBuf>,
    pub hosts: Vec<HostProfile>,
    pub active_host_id: String,
    pub expanded_groups: HashSet<String>,
    pub sidebar_collapsed: bool,
    pub tabs: Vec<SessionTab>,
    pub active_tab_id: String,
    pub sftp: SftpPanelState,
    sftp_pane_id: Option<String>,
    sftp_scroll_handle: UniformListScrollHandle,
    pub sftp_creating_dir: bool,
    pub sftp_new_dir_name: String,
    pub sftp_context_menu: Option<SftpContextMenu>,
    pub terminal_context_menu: Option<TerminalContextMenu>,
    terminal_drag_anchor: Option<MouseDragAnchor>,
    last_drag_mouse_pos: Option<Point<Pixels>>,
    last_edge_scroll_at: Option<Instant>,
    terminal_sgr_button: Option<(String, MouseButton)>,
    pub terminal_search: Option<TerminalSearchState>,
    pub pending_paste_confirm: Option<PendingPasteConfirm>,
    ime_marked_text: Option<String>,
    pub text_cursor: TextCursorState,
    hovered_link: Option<(String, HoveredLinkSpan, String)>,
    pane_metrics: SharedPaneMetrics,
    pane_shapes: SharedPaneShapes,
    last_pane_pty_sizes: HashMap<String, (usize, usize)>,
    pending_pty_resizes: HashMap<String, (usize, usize, Instant)>,
    pane_scroll_accum: HashMap<String, f32>,
    pub show_new_host_modal: bool,
    pub editing_host_id: Option<String>,
    pub host_context_menu: Option<HostContextMenu>,
    pub active_dropdown: Option<ActiveDropdown>,
    dropdown_anchors: Rc<RefCell<HashMap<DropdownKind, Bounds<Pixels>>>>,
    pub host_drag: Option<HostDragState>,
    host_drop_zones: Rc<RefCell<Vec<(HostDropZone, Bounds<Pixels>)>>>,
    suppress_next_host_click: bool,
    pub new_host_draft: NewHostDraft,
    pub detected_ssh_config_hosts: Vec<HostProfile>,
    pub import_ssh_config_on_start: bool,
    pub sync_show_secret: bool,
    pub sync_in_progress: bool,
    sync_active_action: Option<SyncAction>,
    sync_last_failed: bool,
    /// 仅当由冲突弹窗决策或首次配置检测触发时为 true，执行完本次后自动复位，后续自动同步全静默看云图标
    sync_toast_next_result: bool,
    sync_push_pending: bool,
    sync_push_after: Option<Instant>,
    pub pane_emulators: HashMap<String, TerminalEmulator>,
    pane_handles: HashMap<String, PaneSessionHandle>,
    ssh_event_tx: tokio::sync::mpsc::Sender<SshEvent>,
    ssh_event_rx: tokio::sync::mpsc::Receiver<SshEvent>,
    sync_event_tx: mpsc::Sender<SyncEvent>,
    sync_event_rx: mpsc::Receiver<SyncEvent>,
    font_event_tx: mpsc::Sender<FontDownloadEvent>,
    font_event_rx: mpsc::Receiver<FontDownloadEvent>,
    pub font_downloading: HashMap<String, f32>,
    pending_register_fonts: Vec<Vec<u8>>,
    next_id_seq: usize,
    last_applied_glass: Option<(bool, i32)>,
    last_applied_app_icon_dark: Option<bool>,
    last_applied_menu_lang: Option<Language>,
    last_applied_shortcuts: Option<ShortcutBindings>,
    last_shortcuts_suspended: bool,
    /// 记录上一帧终端是否处于活跃输入焦点（用于在焦点进入终端的上升沿自动切换至英文输入源）
    last_terminal_active: bool,
    last_viewport_size: (f32, f32),
    // 动效代次计数（仅在交互切换时递增，驱动 GPUI 原生 ease_out_quint 过渡）
    sidebar_anim_gen: usize,
    search_expanded: bool,
    search_anim_gen: usize,
    sftp_anim_gen: usize,
    sftp_anim_tab_id: Option<String>,
    modal_anim_gen: usize,
    dropdown_anim_gen: usize,
    ctx_menu_anim_gen: usize,
    tab_anim_gen: usize,
    active_drag: Option<ActiveDrag>,
}

impl HoloSshApp {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let focus_handle = cx.focus_handle();
        let settings = AppSettings::load_or_default();
        i18n::set_language(settings.language);
        let detected_ssh_config_hosts = load_local_ssh_config_hosts();
        let hosts = HostProfile::load_persisted();
        let next_id_seq = hosts
            .iter()
            .filter_map(|h| h.id.strip_prefix("host-").and_then(|s| s.parse::<usize>().ok()))
            .max()
            .unwrap_or(0);
        let (ssh_event_tx, ssh_event_rx) = tokio::sync::mpsc::channel::<SshEvent>(256);
        let (sync_event_tx, sync_event_rx) = mpsc::channel::<SyncEvent>();
        let (font_event_tx, font_event_rx) = mpsc::channel::<FontDownloadEvent>();

        // 初始状态默认不选中主机、不打开标签页，直接呈现 Figma 08「初始空状态」
        let tabs = Vec::new();
        let pane_emulators = HashMap::new();
        let active_host_id = String::new();
        let active_tab_id = String::new();

        let screen = if settings.onboarding_completed {
            AppScreen::Workspace
        } else {
            AppScreen::Onboarding
        };

        let new_host_draft = NewHostDraft::from_settings(&settings);
        let vault_local_keys = ssh_keys::discover_local_ssh_keys();

        // 若用户选择了记住密码（1~30天）且本地限时会话未过期，启动时自动恢复解密态；默认不记住密码则保持锁定
        let mut initial_vault_key = None;
        let mut initial_vault_data = None;
        if let Some(k) = vault::try_load_remembered_key(settings.vault_remember_policy) {
            if let Ok(data) = vault::unlock_vault_with_key(&k) {
                initial_vault_key = Some(k);
                initial_vault_data = Some(data);
            } else {
                vault::clear_remember_session();
            }
        }

        // 启动 120Hz 高刷非阻塞 SSH/SFTP 事件拉取协程（仅在有新事件时触发重绘，零空转）
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor()
                    .timer(Duration::from_millis(8))
                    .await;
                let alive = this.update(cx, |app, cx| {
                    if app.drain_ssh_events(cx) {
                        cx.notify();
                    }
                });
                if alive.is_err() {
                    break;
                }
            }
        })
        .detach();

        let mut app = Self {
            focus_handle,
            active_input: ActiveInput::Terminal,
            search_query: String::new(),
            creating_group: false,
            new_group_name: String::new(),
            renaming_group: None,
            rename_group_input: String::new(),
            group_context_menu: None,
            deleting_group_confirm: None,
            screen,
            settings,
            active_settings_section: SettingsSection::Appearance,
            active_settings_subpage: None,
            recording_shortcut: None,
            toasts: Vec::new(),
            next_toast_id: 1,
            vault_key: initial_vault_key,
            vault_data: initial_vault_data,
            vault_revealed_host_id: None,
            vault_settings_form: VaultSettingsForm::default(),
            vault_modal: None,
            sync_conflict_modal: None,
            pending_vault_connects: HashMap::new(),
            pending_auth_prompts: HashMap::new(),
            pending_host_key_prompts: HashMap::new(),
            pending_reconnects: HashMap::new(),
            pane_reconnect_attempts: HashMap::new(),
            vault_local_keys,
            hosts,
            active_host_id,
            expanded_groups: HashSet::new(),
            sidebar_collapsed: false,
            tabs,
            active_tab_id,
            sftp: SftpPanelState::default(),
            sftp_pane_id: None,
            sftp_scroll_handle: UniformListScrollHandle::new(),
            sftp_creating_dir: false,
            sftp_new_dir_name: String::new(),
            sftp_context_menu: None,
            terminal_context_menu: None,
            terminal_drag_anchor: None,
            last_drag_mouse_pos: None,
            last_edge_scroll_at: None,
            terminal_sgr_button: None,
            terminal_search: None,
            pending_paste_confirm: None,
            ime_marked_text: None,
            text_cursor: TextCursorState::default(),
            hovered_link: None,
            pane_metrics: Rc::new(RefCell::new(HashMap::new())),
            pane_shapes: Rc::new(RefCell::new(HashMap::new())),
            last_pane_pty_sizes: HashMap::new(),
            pending_pty_resizes: HashMap::new(),
            pane_scroll_accum: HashMap::new(),
            show_new_host_modal: false,
            editing_host_id: None,
            host_context_menu: None,
            active_dropdown: None,
            dropdown_anchors: Rc::new(RefCell::new(HashMap::new())),
            host_drag: None,
            host_drop_zones: Rc::new(RefCell::new(Vec::new())),
            suppress_next_host_click: false,
            new_host_draft,
            detected_ssh_config_hosts,
            import_ssh_config_on_start: true,
            sync_show_secret: false,
            sync_in_progress: false,
            sync_active_action: None,
            sync_last_failed: false,
            sync_toast_next_result: false,
            sync_push_pending: false,
            sync_push_after: None,
            pane_emulators,
            pane_handles: HashMap::new(),
            ssh_event_tx,
            ssh_event_rx,
            sync_event_tx,
            sync_event_rx,
            font_event_tx,
            font_event_rx,
            font_downloading: HashMap::new(),
            pending_register_fonts: Vec::new(),
            next_id_seq,
            last_applied_glass: None,
            last_applied_app_icon_dark: None,
            last_applied_menu_lang: None,
            last_applied_shortcuts: None,
            last_shortcuts_suspended: false,
            last_terminal_active: false,
            last_viewport_size: (
                crate::theme::LayoutMetrics::WINDOW_WIDTH,
                crate::theme::LayoutMetrics::WINDOW_HEIGHT,
            ),
            sidebar_anim_gen: 0,
            search_expanded: false,
            search_anim_gen: 0,
            sftp_anim_gen: 0,
            sftp_anim_tab_id: None,
            modal_anim_gen: 0,
            dropdown_anim_gen: 0,
            ctx_menu_anim_gen: 0,
            tab_anim_gen: 0,
            active_drag: None,
        };
        if app.is_vault_unlocked() {
            app.try_apply_pending_pull_with_unlocked_vault();
        }
        if app.settings.sync_provider != SyncProvider::Disabled
            && app.settings.sync_auto_pull_on_start
            && app.has_sync_credentials()
        {
            app.trigger_cloud_sync(SyncAction::AutoPull);
        }
        app
    }

    /// 排空后台 Tokio 引擎回传的所有 SSH/SFTP 事件（含背压帧预算、PtyWrite 自动回包、OSC 52/OSC 7、响铃、指纹确认与自动重连）
    fn drain_ssh_events(&mut self, cx: &mut Context<Self>) -> bool {
        const MAX_OUTPUT_BYTES_PER_FRAME: usize = 256 * 1024;
        const MAX_FRAME_BUDGET: Duration = Duration::from_millis(6);
        let mut dirty = false;

        // 同步 TerminalElement prepaint 产出的精确 GPU 行列尺寸到本地仿真器，并对远端 PTY 执行 50ms 尾部防抖
        if self.sync_measured_pane_pty_sizes() {
            dirty = true;
        }
        self.flush_debounced_pty_resizes();

        // 拖选触及终端画布上下边沿时自动滚屏扩选
        if self.tick_terminal_drag_auto_scroll() {
            dirty = true;
        }

        // 到期的意外断线指数退避自动重连
        if self.tick_pending_reconnects() {
            dirty = true;
        }

        let frame_start = Instant::now();
        let mut output_bytes_this_frame: usize = 0;

        while let Ok(ev) = self.ssh_event_rx.try_recv() {
            dirty = true;
            match ev {
                SshEvent::Connecting {
                    pane_id,
                    host_id,
                    message,
                } => {
                    if let Some(emu) = self.pane_emulators.get_mut(&pane_id) {
                        emu.write_notice(&message);
                    }
                    let _ = host_id;
                }
                SshEvent::AuthNotice { pane_id, message } => {
                    if let Some(emu) = self.pane_emulators.get_mut(&pane_id) {
                        emu.write_notice(&message);
                    }
                }
                SshEvent::AuthPrompt {
                    pane_id,
                    prompt_id,
                    prompt,
                    echo,
                } => {
                    if let Some(emu) = self.pane_emulators.get_mut(&pane_id) {
                        emu.feed_bytes(prompt.as_bytes());
                    }
                    self.pending_auth_prompts.insert(
                        pane_id,
                        PendingAuthPrompt {
                            prompt_id,
                            input_buf: String::new(),
                            echo,
                        },
                    );
                }
                SshEvent::HostKeyPrompt {
                    pane_id,
                    prompt_id,
                    hostname,
                    port,
                    key_type,
                    fingerprint,
                    is_key_changed,
                } => {
                    if let Some(emu) = self.pane_emulators.get_mut(&pane_id) {
                        let port_str = port.to_string();
                        if is_key_changed {
                            let warn = trf(
                                "term.hostkey_changed_warning",
                                &[("host", &hostname), ("port", &port_str)],
                            );
                            emu.feed_bytes(format!("\r\n\x1b[1;31m{warn}\x1b[0m\r\n").as_bytes());
                        } else {
                            let hdr = trf(
                                "term.hostkey_new_header",
                                &[("host", &hostname), ("port", &port_str)],
                            );
                            emu.write_notice(&hdr);
                        }
                        let fp_line = trf(
                            "term.hostkey_fingerprint_line",
                            &[("key_type", &key_type), ("fp", &fingerprint)],
                        );
                        emu.feed_bytes(format!("{fp_line}\r\n").as_bytes());
                        emu.feed_bytes(tr("term.hostkey_confirm_prompt").as_bytes());
                    }
                    self.pending_host_key_prompts.insert(
                        pane_id,
                        PendingHostKeyPrompt {
                            prompt_id,
                            hostname,
                            port,
                            input_buf: String::new(),
                        },
                    );
                }
                SshEvent::Connected { pane_id, host_id } => {
                    self.pending_auth_prompts.remove(&pane_id);
                    self.pending_host_key_prompts.remove(&pane_id);
                    self.pending_reconnects.remove(&pane_id);
                    self.pane_reconnect_attempts.remove(&pane_id);
                    self.pending_pty_resizes.remove(&pane_id);
                    if let Some(host) = self.hosts.iter_mut().find(|h| h.id == host_id) {
                        host.status = HostStatus::Connected;
                    }
                    // 连接成功后立即清屏并清空连接前的本地提示与历史回滚，干净承接远端 Shell 输出
                    if let Some(emu) = self.pane_emulators.get_mut(&pane_id) {
                        emu.clear_screen_and_history();
                    }
                    // 连接成功后立即把精确网格尺寸补发一次，规避 Tabby 初始 resize 竞态
                    if let Some(m) = self.pane_metrics.borrow().get(&pane_id).copied() {
                        if let Some(emu) = self.pane_emulators.get_mut(&pane_id) {
                            emu.resize(m.cols, m.rows);
                        }
                        if let Some(handle) = self.pane_handles.get(&pane_id) {
                            handle.resize(m.cols as u32, m.rows as u32);
                        }
                        self.last_pane_pty_sizes.insert(pane_id.clone(), (m.cols, m.rows));
                    }
                    let mut should_list_sftp = None;
                    for tab in &mut self.tabs {
                        let mut matched = false;
                        let mut cur_dir = "~".to_string();
                        tab.root_pane.update_pane(&pane_id, &mut |pane| {
                            pane.connected = true;
                            cur_dir = pane.current_dir.clone();
                            matched = true;
                        });
                        if matched {
                            tab.connected = true;
                            if tab.sftp_open && tab.focused_pane_id == pane_id {
                                should_list_sftp = Some(cur_dir);
                            }
                        }
                    }
                    if let Some(dir) = should_list_sftp {
                        self.request_sftp_list_for_pane(&pane_id, &dir);
                    }
                }
                SshEvent::Output { pane_id, data } => {
                    output_bytes_this_frame = output_bytes_this_frame.saturating_add(data.len());
                    let mut pty_writes = Vec::new();
                    let mut clipboards = Vec::new();
                    let mut bell_count = 0u64;
                    let mut cwd_update = None;
                    let mut title_dir_fallback = None;
                    if let Some(emu) = self.pane_emulators.get_mut(&pane_id) {
                        emu.feed_bytes(&data);
                        pty_writes = emu.drain_pty_writes();
                        clipboards = emu.drain_clipboard_stores();
                        bell_count = emu.take_bell_count();
                        cwd_update = emu.take_cwd_update();
                        if cwd_update.is_none() {
                            if let Some(title) = emu.detected_title() {
                                title_dir_fallback = extract_dir_from_osc_title(&title);
                            }
                        }
                    }
                    // 非当前活跃标签页触发终端响铃（\x07）时，点亮标签页未读提醒圆点
                    if bell_count > 0 {
                        for tab in &mut self.tabs {
                            if tab.id != self.active_tab_id
                                && tab.root_pane.find_pane(&pane_id).is_some()
                            {
                                tab.has_bell = true;
                                break;
                            }
                        }
                    }
                    // 立即将 DSR (\x1b[6n]) / DA1 / OSC 10-11 应答写回远端 PTY（根治 nvim/starship/fish 卡顿）
                    if !pty_writes.is_empty() {
                        if let Some(handle) = self.pane_handles.get(&pane_id) {
                            for resp in pty_writes {
                                handle.send_bytes(resp.into_bytes());
                            }
                        }
                    }
                    // OSC 52 远端复制直接同步至本机剪贴板
                    if let Some(last_clip) = clipboards.into_iter().last() {
                        if !last_clip.is_empty() {
                            cx.write_to_clipboard(ClipboardItem::new_string(last_clip));
                            self.toast_info(tr("toast.osc52_copied"));
                        }
                    }
                    // OSC 7 / OSC 1337 实时更新窗格标题当前目录，并在 SFTP 打开时自动跟随目录
                    let new_dir = cwd_update.clone().or(title_dir_fallback);
                    if let Some(dir) = new_dir {
                        let mut changed_for_sftp = false;
                        for tab in &mut self.tabs {
                            tab.root_pane.update_pane(&pane_id, &mut |pane| {
                                if pane.current_dir != dir {
                                    pane.current_dir = dir.clone();
                                    if tab.sftp_open
                                        && tab.focused_pane_id == pane_id
                                        && cwd_update.is_some()
                                    {
                                        changed_for_sftp = true;
                                    }
                                }
                            });
                        }
                        if changed_for_sftp && dir.starts_with('/') {
                            self.request_sftp_list_for_pane(&pane_id, &dir);
                        }
                    }
                    // 若当前窗格开着 ⌘F 搜索，随输出实时刷新命中集
                    if self
                        .terminal_search
                        .as_ref()
                        .is_some_and(|s| s.pane_id == pane_id && !s.query.is_empty())
                    {
                        self.refresh_terminal_search_matches(false);
                    }
                }
                SshEvent::Error {
                    pane_id,
                    host_id,
                    message,
                } => {
                    self.pending_auth_prompts.remove(&pane_id);
                    self.pending_host_key_prompts.remove(&pane_id);
                    self.pane_handles.remove(&pane_id);
                    if let Some(emu) = self.pane_emulators.get_mut(&pane_id) {
                        // 断线自动重置远端残留私有模式（防止鼠标追踪/括弧粘贴卡死）
                        emu.reset_terminal_modes();
                        emu.write_notice(&format!("✗ {message}"));
                    }
                    self.mark_pane_disconnected(&pane_id, &host_id);
                    let in_auto_retry = self
                        .pane_reconnect_attempts
                        .get(&pane_id)
                        .copied()
                        .unwrap_or(0)
                        > 0;
                    if !in_auto_retry || !self.schedule_auto_reconnect(&pane_id, &host_id) {
                        self.pane_reconnect_attempts.remove(&pane_id);
                        if let Some(emu) = self.pane_emulators.get_mut(&pane_id) {
                            emu.write_notice(tr("workspace.conn_retry_hint"));
                        }
                    }
                }
                SshEvent::Disconnected {
                    pane_id,
                    host_id,
                    reason,
                    unexpected,
                } => {
                    self.pending_auth_prompts.remove(&pane_id);
                    self.pending_host_key_prompts.remove(&pane_id);
                    self.pane_handles.remove(&pane_id);
                    if let Some(emu) = self.pane_emulators.get_mut(&pane_id) {
                        emu.reset_terminal_modes();
                        emu.write_notice(&trf(
                            "workspace.conn_closed_retry",
                            &[("reason", &reason)],
                        ));
                    }
                    self.mark_pane_disconnected(&pane_id, &host_id);
                    if unexpected {
                        self.schedule_auto_reconnect(&pane_id, &host_id);
                    } else {
                        self.pending_reconnects.remove(&pane_id);
                        self.pane_reconnect_attempts.remove(&pane_id);
                    }
                }
                SshEvent::SftpListed {
                    pane_id,
                    path,
                    entries,
                } => {
                    if self.active_focused_pane_id().as_deref() == Some(pane_id.as_str()) {
                        let path_changed = self.sftp.current_path != path;
                        self.sftp.loading = false;
                        self.sftp.set_remote_path(&path);
                        self.sftp.entries = entries;
                        self.sftp.selected_name = self.sftp.entries.first().map(|e| e.name.clone());
                        if path_changed {
                            self.sftp_scroll_handle.scroll_to_item(0, ScrollStrategy::Top);
                        }
                    }
                }
                SshEvent::SftpTransferProgress {
                    pane_id,
                    transfer_id,
                    direction,
                    file_name,
                    transferred_mb,
                    total_mb,
                    progress,
                    done,
                } => {
                    if self.active_focused_pane_id().as_deref() == Some(pane_id.as_str()) {
                        self.sftp.upsert_transfer(TransferTask {
                            id: transfer_id,
                            direction,
                            file_name,
                            transferred_mb,
                            total_mb,
                            progress,
                            done,
                        });
                    }
                }
                SshEvent::SftpTransferCancelled {
                    pane_id,
                    transfer_id,
                } => {
                    if self.active_focused_pane_id().as_deref() == Some(pane_id.as_str()) {
                        self.sftp.remove_transfer(transfer_id);
                    }
                }
                SshEvent::SftpError { pane_id, message } => {
                    if self.active_focused_pane_id().as_deref() == Some(pane_id.as_str()) {
                        self.sftp.loading = false;
                        self.toast_error(message);
                    }
                }
            }
            // 每帧最多消费 256 KiB 或 6ms 输出，确保高频刷屏时 UI 与 Ctrl+C 保持 120Hz 丝滑响应
            if output_bytes_this_frame >= MAX_OUTPUT_BYTES_PER_FRAME
                || frame_start.elapsed() >= MAX_FRAME_BUDGET
            {
                break;
            }
        }
        // 长按主机行超过 220ms 自动激活侧栏拖拽态（支持拖入其他文件夹或任意行间/顶部插入）
        if let Some(drag) = self.host_drag.as_mut() {
            if !drag.active && drag.pressed_at.elapsed() >= Duration::from_millis(220) {
                drag.active = true;
                let pos = drag.current_pos;
                let (target, line) = self.hit_test_host_drop_target(pos);
                if let Some(d) = self.host_drag.as_mut() {
                    d.hover_target = target;
                    d.insertion_line = line;
                }
                dirty = true;
            }
        }
        // 防抖排队推送：即使上次同步仍在进行中，变更也不会丢弃；若存在未决同步冲突弹窗则暂停自动推送
        if self.sync_push_pending
            && !self.sync_in_progress
            && self.sync_conflict_modal.is_none()
            && self
                .sync_push_after
                .is_some_and(|deadline| Instant::now() >= deadline)
        {
            self.sync_push_pending = false;
            self.sync_push_after = None;
            self.trigger_cloud_sync(SyncAction::Push);
            dirty = true;
        }
        while let Ok(sync_ev) = self.sync_event_rx.try_recv() {
            dirty = true;
            let completed_action = self.sync_active_action.take();
            self.sync_in_progress = false;
            match sync_ev {
                SyncEvent::TestOk { message } => {
                    self.sync_last_failed = false;
                    self.sync_toast_next_result = false;
                    self.toast_success(message);
                }
                SyncEvent::ProbeEmpty => {
                    self.sync_last_failed = false;
                    // 首次配置探测发现云端为空：上传初始版本并在本次完成/失败时弹一次提示
                    self.sync_toast_next_result = true;
                    self.toast_info(tr("toast.sync_probe_empty_uploading"));
                    self.trigger_cloud_sync(SyncAction::ForcePush);
                }
                SyncEvent::ConflictPrompt {
                    reason,
                    snapshot,
                    same_vault_salt,
                } => {
                    self.sync_toast_next_result = false;
                    // 暂停待推队列，立即在当前任意页面顶层弹出同步决策弹窗
                    self.sync_push_pending = false;
                    self.sync_push_after = None;
                    let remote_time_display =
                        sync::format_epoch_ms_local(snapshot.updated_at_ms);
                    self.modal_anim_gen += 1;
                    self.sync_conflict_modal = Some(SyncConflictModalState {
                        reason,
                        snapshot,
                        same_vault_salt,
                        remote_time_display,
                        local_host_count: self.hosts.len(),
                        password_action: None,
                        password: String::new(),
                        show_password: false,
                        error: None,
                    });
                }
                SyncEvent::PushOk {
                    message,
                    created_gist_id,
                    revision,
                    sync_epoch,
                    vault_salt,
                    synced_at,
                    merged_catalog,
                    merged_vault_data,
                    merged_vault_file,
                } => {
                    self.sync_last_failed = false;
                    let should_toast = std::mem::take(&mut self.sync_toast_next_result);
                    if let Some(gid) = created_gist_id {
                        self.settings.gist_id = gid;
                    }
                    self.settings.last_sync_at = Some(synced_at);
                    self.settings.sync_base_revision = revision;
                    if !sync_epoch.is_empty() {
                        self.settings.sync_epoch = sync_epoch;
                    }
                    if !vault_salt.is_empty() {
                        self.settings.sync_vault_salt = vault_salt;
                    }
                    if let Some(cat) = merged_catalog {
                        self.apply_merged_catalog(cat, revision);
                    } else {
                        self.settings.save();
                    }
                    self.apply_merged_vault_from_sync(merged_vault_data, merged_vault_file);
                    sync::clear_pending_pull();
                    if should_toast {
                        self.toast_success(message);
                    }
                }
                SyncEvent::PullMerged {
                    revision,
                    sync_epoch,
                    vault_salt,
                    synced_at,
                    merged_catalog,
                    merged_vault_data,
                    merged_vault_file,
                } => {
                    self.sync_last_failed = false;
                    self.sync_toast_next_result = false;
                    self.settings.last_sync_at = Some(synced_at);
                    if !sync_epoch.is_empty() {
                        self.settings.sync_epoch = sync_epoch;
                    }
                    if !vault_salt.is_empty() {
                        self.settings.sync_vault_salt = vault_salt;
                    }
                    self.apply_merged_catalog(merged_catalog, revision);
                    self.apply_merged_vault_from_sync(
                        Some(merged_vault_data),
                        Some(merged_vault_file),
                    );
                    sync::clear_pending_pull();
                }
                SyncEvent::PullNeedsUnlock {
                    message,
                    snapshot,
                    synced_at,
                    is_auto_pull,
                } => {
                    self.sync_last_failed = false;
                    self.sync_toast_next_result = false;
                    sync::save_pending_pull(&snapshot);
                    self.settings.last_sync_at = Some(synced_at);
                    self.settings.save();
                    if is_auto_pull {
                        self.toast_info(message);
                    } else {
                        self.modal_anim_gen += 1;
                        self.vault_modal = Some(VaultModalDraft {
                            is_setup: false,
                            next_action: VaultModalNextAction::ApplyPendingSyncPull,
                            password: String::new(),
                            confirm_password: String::new(),
                            show_password: false,
                        });
                        self.active_input = ActiveInput::VaultModalPassword;
                    }
                }
                SyncEvent::PullUpToDate => {
                    self.sync_last_failed = false;
                    self.sync_toast_next_result = false;
                }
                SyncEvent::Error { message } => {
                    self.sync_last_failed = true;
                    let from_conflict_or_probe = std::mem::take(&mut self.sync_toast_next_result);
                    let from_explicit_test = matches!(
                        completed_action,
                        Some(SyncAction::TestConnection | SyncAction::ProbeRemote)
                    );
                    if from_conflict_or_probe || from_explicit_test {
                        self.toast_error(message);
                    }
                }
            }
        }
        while let Ok(font_ev) = self.font_event_rx.try_recv() {
            dirty = true;
            match font_ev {
                FontDownloadEvent::Progress { font_id, progress } => {
                    self.font_downloading.insert(font_id, progress);
                }
                FontDownloadEvent::Completed {
                    font_id,
                    gpui_family,
                    font_bytes,
                } => {
                    self.font_downloading.remove(&font_id);
                    self.pending_register_fonts.extend(font_bytes);
                    self.settings.terminal_font_family = gpui_family.clone();
                    self.settings.save();
                    self.sync_active_tab_pty_sizes();
                    self.toast_success(trf("toast.font_installed", &[("family", &gpui_family)]));
                }
                FontDownloadEvent::Failed { font_id, error } => {
                    self.font_downloading.remove(&font_id);
                    self.toast_error(error);
                }
            }
        }

        // 检查「不记住密码」模式下终端内等待输入密钥库密码是否超时
        let timed_out_panes: Vec<String> = self
            .pending_vault_connects
            .iter()
            .filter(|(_, p)| p.started_at.elapsed().as_secs() >= p.timeout_secs)
            .map(|(pid, _)| pid.clone())
            .collect();
        for pid in timed_out_panes {
            if let Some(pending) = self.pending_vault_connects.remove(&pid) {
                dirty = true;
                if let Some(emu) = self.pane_emulators.get_mut(&pid) {
                    emu.feed_bytes(b"\r\n");
                    emu.write_notice(tr("workspace.vault_timeout"));
                }
                self.spawn_real_ssh_for_pane(&pid, &pending.host, None, None, None);
            }
        }

        // 清理已到期的全局 Toast（多条叠加时各自按独立倒计时自动隐藏）
        if !self.toasts.is_empty() {
            let before = self.toasts.len();
            self.toasts.retain(|t| !t.is_expired());
            if self.toasts.len() != before {
                dirty = true;
            }
        }
        dirty
    }

    pub fn push_toast(&mut self, message: impl Into<String>, kind: ToastKind) {
        let id = self.next_toast_id;
        self.next_toast_id = self.next_toast_id.wrapping_add(1).max(1);
        self.toasts.push(ToastItem::new(id, message, kind));
        if self.toasts.len() > 5 {
            let excess = self.toasts.len() - 5;
            self.toasts.drain(0..excess);
        }
    }

    pub fn toast_info(&mut self, message: impl Into<String>) {
        self.push_toast(message, ToastKind::Info);
    }

    pub fn toast_success(&mut self, message: impl Into<String>) {
        self.push_toast(message, ToastKind::Success);
    }

    pub fn toast_error(&mut self, message: impl Into<String>) {
        self.push_toast(message, ToastKind::Error);
    }

    pub fn dismiss_toast(&mut self, toast_id: u64) {
        self.toasts.retain(|t| t.id != toast_id);
    }

    fn mark_pane_disconnected(&mut self, pane_id: &str, host_id: &str) {
        let mut any_connected_for_host = false;
        for tab in &mut self.tabs {
            tab.root_pane.update_pane(pane_id, &mut |pane| {
                pane.connected = false;
            });
            let mut ids = Vec::new();
            tab.root_pane.collect_pane_ids(&mut ids);
            let tab_still_up = ids.iter().any(|id| {
                tab.root_pane
                    .find_pane(id)
                    .map(|p| p.connected)
                    .unwrap_or(false)
            });
            tab.connected = tab_still_up;
            if tab.host_id == host_id && tab_still_up {
                any_connected_for_host = true;
            }
        }
        if !any_connected_for_host {
            if let Some(host) = self.hosts.iter_mut().find(|h| h.id == host_id) {
                host.status = HostStatus::Idle;
            }
        }
    }

    /// 结合 macOS 实时系统深浅色与用户的磨砂不透明度，生成全应用统一的单层磨砂主题 Token
    fn current_theme(&self, window: &Window) -> (ThemeTokens, bool) {
        let system_is_dark = matches!(
            window.appearance(),
            WindowAppearance::Dark | WindowAppearance::VibrantDark
        );
        let is_dark = match self.settings.appearance {
            AppearanceMode::System => system_is_dark,
            AppearanceMode::Dark => true,
            AppearanceMode::Light => false,
        };
        let mut theme = ThemeTokens::resolve(self.settings.appearance, is_dark);

        if self.settings.glass_enabled {
            // 深色模式：右侧主画布 = terminal_opacity，左侧略厚润（+0.06）；
            // 浅色模式：复刻 macOS 系统设置层级——左侧保持通透浅灰磨砂，右侧内容区更明亮清透纯白，拉开左右明暗对比；
            // 支持「仅左侧 / 仅右侧 / 所有窗口」磨砂区域，未启用磨砂的一侧自动呈 100% 纯净实底
            let base_alpha = self.settings.terminal_opacity.clamp(0.40, 0.95);
            let (mut win_alpha, mut sidebar_alpha, mut term_alpha, mut sftp_alpha) = if is_dark {
                (
                    (base_alpha + 0.04).clamp(0.44, 0.96),
                    (base_alpha + 0.06).clamp(0.46, 0.97),
                    base_alpha,
                    (base_alpha + 0.04).clamp(0.44, 0.96),
                )
            } else {
                (
                    (base_alpha + 0.18).clamp(0.76, 0.96),
                    (base_alpha - 0.02).clamp(0.56, 0.86),
                    (base_alpha + 0.20).clamp(0.80, 0.97),
                    (base_alpha + 0.08).clamp(0.64, 0.92),
                )
            };

            let sidebar_is_left = self.screen == AppScreen::Settings
                || self.settings.sidebar_side == SidebarSide::Left;
            let (sidebar_glass, content_glass) = match self.settings.glass_scope {
                GlassScope::AllWindow => (true, true),
                GlassScope::LeftOnly => (sidebar_is_left, !sidebar_is_left),
                GlassScope::RightOnly => (!sidebar_is_left, sidebar_is_left),
            };
            if !sidebar_glass {
                sidebar_alpha = 1.0;
            }
            if !content_glass {
                win_alpha = 1.0;
                term_alpha = 1.0;
                sftp_alpha = 1.0;
            }
            if self.settings.glass_scope != GlassScope::AllWindow {
                let mut sheet_rgba: gpui::Rgba = theme.sheet_bg.into();
                sheet_rgba.a = 1.0;
                theme.sheet_bg = sheet_rgba.into();
            }

            let mut win_rgba: gpui::Rgba = theme.window_bg.into();
            win_rgba.a = win_alpha;
            theme.window_bg = win_rgba.into();

            let mut sb_rgba: gpui::Rgba = theme.sidebar_bg.into();
            sb_rgba.a = sidebar_alpha;
            theme.sidebar_bg = sb_rgba.into();

            let mut term_rgba: gpui::Rgba = theme.terminal_bg.into();
            term_rgba.a = term_alpha;
            theme.terminal_bg = term_rgba.into();

            let mut sftp_rgba: gpui::Rgba = theme.sftp_bg.into();
            sftp_rgba.a = sftp_alpha;
            theme.sftp_bg = sftp_rgba.into();
        } else {
            // 关闭磨砂玻璃时全部主表面统一切换为纯色不透明
            for slot in [
                &mut theme.window_bg,
                &mut theme.sidebar_bg,
                &mut theme.terminal_bg,
                &mut theme.sftp_bg,
                &mut theme.sheet_bg,
            ] {
                let mut c: gpui::Rgba = (*slot).into();
                c.a = 1.0;
                *slot = c.into();
            }
        }
        (theme, is_dark)
    }

    // ====================== 设置页真实业务逻辑 ======================

    pub fn set_appearance(&mut self, mode: AppearanceMode) {
        self.settings.appearance = mode;
        self.settings.save();
    }

    pub fn set_glass_scope(&mut self, scope: GlassScope) {
        self.settings.glass_scope = scope;
        self.settings.save();
    }

    pub fn toggle_appearance(&mut self) {
        let next = match self.settings.appearance {
            AppearanceMode::Light => AppearanceMode::Dark,
            AppearanceMode::Dark | AppearanceMode::System => AppearanceMode::Light,
        };
        self.set_appearance(next);
    }

    pub fn toggle_dropdown(&mut self, kind: DropdownKind, position: Point<Pixels>, width: f32) {
        self.host_context_menu = None;
        if self
            .active_dropdown
            .as_ref()
            .map(|d| d.kind == kind)
            .unwrap_or(false)
        {
            self.active_dropdown = None;
        } else {
            self.dropdown_anim_gen += 1;
            self.active_dropdown = Some(ActiveDropdown {
                kind,
                position,
                width,
            });
        }
    }

    pub fn close_dropdown(&mut self) {
        self.active_dropdown = None;
    }

    fn dropdown_options(&self, kind: DropdownKind) -> Vec<(String, bool)> {
        match kind {
            DropdownKind::Language => Language::ALL
                .iter()
                .map(|&l| (l.display_name().to_string(), l == self.settings.language))
                .collect(),
            DropdownKind::TerminalFont => {
                let current_id =
                    fonts::find_spec_by_stored_name(&self.settings.terminal_font_family).id;
                FONT_CATALOG
                    .iter()
                    .map(|spec| {
                        let label = if spec.is_system {
                            tr("common.system_default").to_string()
                        } else {
                            spec.display_name.to_string()
                        };
                        (label, spec.id == current_id)
                    })
                    .collect()
            }
            DropdownKind::ScrollbackLines => [2000usize, 4000, 10000, 20000]
                .iter()
                .map(|&n| {
                    (
                        trf("settings.terminal.lines_unit", &[("n", &n.to_string())]),
                        n == self.settings.scrollback_lines,
                    )
                })
                .collect(),
            DropdownKind::KeepAlive => [0u64, 15, 30, 60, 120]
                .iter()
                .map(|&s| {
                    let label = if s == 0 {
                        tr("common.off").to_string()
                    } else {
                        trf("settings.conn.every_secs", &[("s", &s.to_string())])
                    };
                    (label, s == self.settings.keepalive_secs)
                })
                .collect(),
            DropdownKind::ConnectTimeout => [5u64, 10, 12, 20, 30]
                .iter()
                .map(|&s| {
                    (
                        trf("settings.conn.secs_unit", &[("s", &s.to_string())]),
                        s == self.settings.connect_timeout_secs,
                    )
                })
                .collect(),
            DropdownKind::DefaultUsername => {
                let local_user = std::env::var("USER").unwrap_or_else(|_| "admin".into());
                let mut list = vec!["root".to_string()];
                for u in [local_user, "ubuntu".to_string(), "admin".to_string()] {
                    if !list.contains(&u) {
                        list.push(u);
                    }
                }
                list.into_iter()
                    .map(|u| {
                        let sel = u == self.settings.default_username;
                        (u, sel)
                    })
                    .collect()
            }
            DropdownKind::DefaultPort => [22u16, 2222, 22022]
                .iter()
                .map(|&p| (p.to_string(), p == self.settings.default_port))
                .collect(),
            DropdownKind::VaultRememberPolicy => VaultRememberPolicy::ALL
                .iter()
                .map(|&p| (p.label_zh().to_string(), p == self.settings.vault_remember_policy))
                .collect(),
            DropdownKind::DraftGroup => {
                let none_label = tr("common.none").to_string();
                let mut groups = vec![none_label.clone()];
                for g in &self.settings.custom_groups {
                    if !g.trim().is_empty() && !groups.contains(g) {
                        groups.push(g.clone());
                    }
                }
                for h in &self.hosts {
                    if !h.group.trim().is_empty() && !groups.contains(&h.group) {
                        groups.push(h.group.clone());
                    }
                }
                groups
                    .into_iter()
                    .enumerate()
                    .map(|(i, g)| {
                        let sel = if i == 0 {
                            self.new_host_draft.group.trim().is_empty()
                        } else {
                            g == self.new_host_draft.group
                        };
                        (g, sel)
                    })
                    .collect()
            }
            DropdownKind::DraftConnection => ConnectionKind::ALL
                .iter()
                .map(|&c| (c.label_zh().to_string(), c == self.new_host_draft.connection))
                .collect(),
            DropdownKind::DraftJumpHost => {
                let candidates = self.jump_host_candidates();
                if candidates.is_empty() {
                    vec![(tr("modal.host.jump_host_empty").to_string(), false)]
                } else {
                    let mut opts = Vec::with_capacity(candidates.len() + 1);
                    opts.push((
                        tr("modal.host.jump_host_select").to_string(),
                        self.new_host_draft.jump_host_id.is_none(),
                    ));
                    for h in candidates {
                        let sel = self.new_host_draft.jump_host_id.as_deref() == Some(h.id.as_str());
                        opts.push((h.jump_display_label(), sel));
                    }
                    opts
                }
            }
        }
    }

    /// 返回当前新建/编辑弹窗可选的跳板主机列表（排除自身与本地终端，按「分组 / 名称」排序，与 Tabby 一致）
    fn jump_host_candidates(&self) -> Vec<&HostProfile> {
        let mut list: Vec<&HostProfile> = self
            .hosts
            .iter()
            .filter(|h| !h.is_local() && self.editing_host_id.as_deref() != Some(h.id.as_str()))
            .collect();
        list.sort_by(|a, b| {
            a.jump_display_label()
                .to_lowercase()
                .cmp(&b.jump_display_label().to_lowercase())
        });
        list
    }

    fn selected_jump_host_label(&self) -> Option<String> {
        let id = self.new_host_draft.jump_host_id.as_deref()?;
        self.hosts
            .iter()
            .find(|h| h.id == id)
            .map(|h| h.jump_display_label())
    }

    /// 检测 `start_host_id -> candidate_jump_id -> ...` 是否形成环路
    fn has_jump_cycle(&self, self_id: Option<&str>, candidate_jump_id: &str) -> bool {
        let mut visited: HashSet<&str> = HashSet::new();
        if let Some(sid) = self_id {
            visited.insert(sid);
        }
        let mut cur = Some(candidate_jump_id);
        while let Some(id) = cur {
            if !visited.insert(id) {
                return true;
            }
            cur = self
                .hosts
                .iter()
                .find(|h| h.id == id && h.connection == ConnectionKind::JumpHost)
                .and_then(|h| h.jump_host_id.as_deref());
        }
        false
    }

    pub fn select_dropdown_index(&mut self, kind: DropdownKind, idx: usize) {
        match kind {
            DropdownKind::Language => {
                if let Some(&lang) = Language::ALL.get(idx) {
                    self.settings.language = lang;
                    i18n::set_language(lang);
                    self.settings.save();
                }
            }
            DropdownKind::TerminalFont => {
                if let Some(spec) = FONT_CATALOG.get(idx) {
                    if spec.is_system || fonts::is_font_downloaded(spec) {
                        self.settings.terminal_font_family = spec.display_name.to_string();
                        self.settings.save();
                        self.sync_active_tab_pty_sizes();
                        self.active_dropdown = None;
                    } else if !self.font_downloading.contains_key(spec.id) {
                        self.font_downloading.insert(spec.id.to_string(), 0.04);
                        fonts::spawn_font_download(spec, self.font_event_tx.clone());
                    }
                }
                return;
            }
            DropdownKind::ScrollbackLines => {
                let vals = [2000usize, 4000, 10000, 20000];
                if let Some(&n) = vals.get(idx) {
                    self.settings.scrollback_lines = n;
                    self.settings.save();
                }
            }
            DropdownKind::KeepAlive => {
                let vals = [0u64, 15, 30, 60, 120];
                if let Some(&s) = vals.get(idx) {
                    self.settings.keepalive_secs = s;
                    self.settings.save();
                }
            }
            DropdownKind::ConnectTimeout => {
                let vals = [5u64, 10, 12, 20, 30];
                if let Some(&s) = vals.get(idx) {
                    self.settings.connect_timeout_secs = s;
                    self.settings.save();
                }
            }
            DropdownKind::DefaultUsername => {
                let opts = self.dropdown_options(DropdownKind::DefaultUsername);
                if let Some((u, _)) = opts.get(idx) {
                    self.settings.default_username = u.clone();
                    self.settings.save();
                }
            }
            DropdownKind::DefaultPort => {
                let vals = [22u16, 2222, 22022];
                if let Some(&p) = vals.get(idx) {
                    self.settings.default_port = p;
                    self.settings.save();
                }
            }
            DropdownKind::VaultRememberPolicy => {
                if let Some(&policy) = VaultRememberPolicy::ALL.get(idx) {
                    self.settings.vault_remember_policy = policy;
                    self.settings.save();
                    if let Some(key) = self.vault_key.as_ref() {
                        vault::apply_remember_session(key, policy);
                    } else if policy == VaultRememberPolicy::Never {
                        vault::clear_remember_session();
                    }
                    self.toast_success(trf(
                        "toast.vault_policy_updated",
                        &[("policy", policy.label_zh())],
                    ));
                }
            }
            DropdownKind::DraftGroup => {
                let opts = self.dropdown_options(DropdownKind::DraftGroup);
                if let Some((g, _)) = opts.get(idx) {
                    self.new_host_draft.group = if idx == 0 {
                        String::new()
                    } else {
                        g.clone()
                    };
                }
            }
            DropdownKind::DraftConnection => {
                if let Some(&c) = ConnectionKind::ALL.get(idx) {
                    let prev = self.new_host_draft.connection;
                    self.new_host_draft.connection = c;
                    if prev != c {
                        match c {
                            ConnectionKind::Local => {
                                if matches!(
                                    self.active_input,
                                    ActiveInput::ModalHost
                                        | ActiveInput::ModalPort
                                        | ActiveInput::ModalProxyHost
                                        | ActiveInput::ModalProxyPort
                                        | ActiveInput::ModalUser
                                        | ActiveInput::ModalPassword
                                        | ActiveInput::ModalKeyPath
                                ) {
                                    self.active_input = ActiveInput::ModalName;
                                }
                            }
                            ConnectionKind::Socks5 => {
                                if self.new_host_draft.proxy_port.trim().is_empty()
                                    || self.new_host_draft.proxy_port.trim() == "8080"
                                {
                                    self.new_host_draft.proxy_port = "1080".to_string();
                                }
                            }
                            ConnectionKind::HttpProxy => {
                                if self.new_host_draft.proxy_port.trim().is_empty()
                                    || self.new_host_draft.proxy_port.trim() == "1080"
                                {
                                    self.new_host_draft.proxy_port = "8080".to_string();
                                }
                            }
                            _ => {}
                        }
                    }
                }
            }
            DropdownKind::DraftJumpHost => {
                let ids: Vec<String> = self
                    .jump_host_candidates()
                    .into_iter()
                    .map(|h| h.id.clone())
                    .collect();
                if !ids.is_empty() {
                    if idx == 0 {
                        self.new_host_draft.jump_host_id = None;
                    } else if let Some(chosen_id) = ids.get(idx - 1) {
                        self.new_host_draft.jump_host_id = Some(chosen_id.clone());
                    }
                }
            }
        }
        self.active_dropdown = None;
    }

    pub fn set_terminal_opacity(&mut self, val: f32) {
        self.settings.terminal_opacity = val.clamp(0.40, 0.95);
        self.settings.save();
    }

    pub fn adjust_terminal_opacity(&mut self, delta: f32) {
        self.set_terminal_opacity(self.settings.terminal_opacity + delta);
    }

    pub fn set_blur_radius(&mut self, val: f32) {
        self.settings.blur_radius = val.clamp(16.0, 60.0);
        self.settings.save();
    }

    pub fn adjust_blur_radius(&mut self, delta: f32) {
        self.set_blur_radius(self.settings.blur_radius + delta);
    }

    pub fn adjust_font_size(&mut self, delta: f32) {
        self.settings.terminal_font_size =
            (self.settings.terminal_font_size + delta).clamp(11.0, 18.0);
        self.settings.save();
        self.sync_active_tab_pty_sizes();
    }

    pub fn set_font_size(&mut self, size: f32) {
        self.settings.terminal_font_size = size.clamp(11.0, 18.0);
        self.settings.save();
        self.sync_active_tab_pty_sizes();
    }

    pub fn set_cursor_style(&mut self, style: CursorStyle) {
        self.settings.cursor_style = style;
        self.settings.save();
    }

    /// 立即扫描 ~/.ssh/config（只读）并合并新主机到 hosts.json
    pub fn import_ssh_config_now(&mut self) {
        let parsed = load_local_ssh_config_hosts();
        self.detected_ssh_config_hosts = parsed.clone();
        let mut added = 0usize;
        for imported in parsed {
            if !self.hosts.iter().any(|h| {
                h.hostname == imported.hostname
                    && h.port == imported.port
                    && h.username == imported.username
            }) {
                self.hosts.push(imported);
                added += 1;
            }
        }
        if added > 0 {
            HostProfile::save_all(&self.hosts);
            self.maybe_auto_push_sync();
            self.toast_success(trf(
                "toast.ssh_config_imported",
                &[
                    ("added", &added.to_string()),
                    ("total", &self.hosts.len().to_string()),
                ],
            ));
        } else {
            self.toast_info(trf(
                "toast.ssh_config_up_to_date",
                &[("scanned", &self.detected_ssh_config_hosts.len().to_string())],
            ));
        }
    }

    pub fn is_vault_unlocked(&self) -> bool {
        self.vault_key.is_some() && self.vault_data.is_some()
    }

    fn try_auto_unlock_remembered(&mut self) -> bool {
        if self.is_vault_unlocked() {
            return true;
        }
        if let Some(k) = vault::try_load_remembered_key(self.settings.vault_remember_policy) {
            if let Ok(data) = vault::unlock_vault_with_key(&k) {
                self.vault_key = Some(k);
                self.vault_data = Some(data);
                self.try_apply_pending_pull_with_unlocked_vault();
                return true;
            }
            vault::clear_remember_session();
        }
        false
    }

    fn vault_entries_sorted(&self) -> Vec<VaultSecretEntry> {
        let Some(data) = self.vault_data.as_ref() else {
            return Vec::new();
        };
        let mut list: Vec<VaultSecretEntry> = data
            .host_secrets
            .values()
            .cloned()
            .map(|mut e| {
                if let Some(h) = self.hosts.iter().find(|h| h.id == e.host_id) {
                    e.host_name = h.name.clone();
                    e.hostname = h.hostname.clone();
                }
                e
            })
            .collect();
        list.sort_by(|a, b| a.host_name.to_lowercase().cmp(&b.host_name.to_lowercase()));
        list
    }

    /// 刷新密钥库页：扫描本机 ~/.ssh/ 私钥并按记住策略自动恢复会话
    pub fn refresh_vault_state(&mut self) {
        self.vault_local_keys = ssh_keys::discover_local_ssh_keys();
        self.try_auto_unlock_remembered();
    }

    /// 立即锁定密钥库并清零内存中派生密钥
    pub fn lock_vault_now(&mut self) {
        if let Some(mut k) = self.vault_key.take() {
            k.zeroize();
        }
        self.vault_data = None;
        self.vault_revealed_host_id = None;
        self.vault_settings_form = VaultSettingsForm::default();
        if matches!(
            self.active_settings_subpage,
            Some(
                SettingsSubPage::VaultChangePassword
                    | SettingsSubPage::VaultHostSecrets
                    | SettingsSubPage::VaultLocalKeys
            )
        ) {
            self.active_settings_subpage = None;
        }
        vault::clear_remember_session();
    }

    pub fn toggle_reveal_vault_host(&mut self, host_id: &str) {
        if self.vault_revealed_host_id.as_deref() == Some(host_id) {
            self.vault_revealed_host_id = None;
        } else {
            self.vault_revealed_host_id = Some(host_id.to_string());
        }
    }

    /// 设置页首次创建密钥库主密码
    pub fn submit_settings_vault_setup(&mut self) {
        let p1 = self.vault_settings_form.setup_password.clone();
        let p2 = self.vault_settings_form.setup_confirm.clone();
        if p1.len() < 4 {
            self.toast_error(tr("toast.vault_pwd_too_short"));
            return;
        }
        if p1 != p2 {
            self.toast_error(tr("toast.vault_pwd_mismatch"));
            return;
        }
        match vault::create_new_vault(&p1) {
            Ok((key, data)) => {
                vault::apply_remember_session(&key, self.settings.vault_remember_policy);
                self.vault_key = Some(key);
                self.vault_data = Some(data);
                self.vault_settings_form.setup_password.clear();
                self.vault_settings_form.setup_confirm.clear();
                self.active_input = ActiveInput::Terminal;
                self.refresh_vault_state();
                self.toast_success(tr("toast.vault_created_unlocked"));
            }
            Err(err) => {
                self.toast_error(err);
            }
        }
    }

    /// 设置页输入主密码解锁密钥库
    pub fn submit_settings_vault_unlock(&mut self) {
        let pwd = self.vault_settings_form.unlock_password.clone();
        if pwd.is_empty() {
            self.toast_error(tr("toast.vault_enter_pwd"));
            return;
        }
        match vault::unlock_vault(&pwd) {
            Ok((key, data)) => {
                vault::apply_remember_session(&key, self.settings.vault_remember_policy);
                self.vault_key = Some(key);
                self.vault_data = Some(data);
                self.vault_settings_form.unlock_password.clear();
                self.active_input = ActiveInput::Terminal;
                self.try_apply_pending_pull_with_unlocked_vault();
                self.toast_success(tr("toast.vault_unlocked"));
            }
            Err(err) => {
                self.toast_error(err);
            }
        }
    }

    /// 设置页修改密钥库主密码（原密码 + 新密码两次确认）
    pub fn submit_change_vault_password(&mut self) {
        let old_p = self.vault_settings_form.change_old.clone();
        let new_p = self.vault_settings_form.change_new.clone();
        let confirm_p = self.vault_settings_form.change_confirm.clone();
        if old_p.is_empty() {
            self.toast_error(tr("toast.vault_enter_old_pwd"));
            return;
        }
        if new_p.len() < 4 {
            self.toast_error(tr("toast.vault_new_pwd_short"));
            return;
        }
        if new_p != confirm_p {
            self.toast_error(tr("toast.vault_new_pwd_mismatch"));
            return;
        }
        match vault::change_master_password(&old_p, &new_p) {
            Ok((new_key, data)) => {
                if let Some(mut old_k) = self.vault_key.replace(new_key) {
                    old_k.zeroize();
                }
                self.vault_data = Some(data);
                vault::apply_remember_session(&new_key, self.settings.vault_remember_policy);
                self.vault_settings_form.change_old.clear();
                self.vault_settings_form.change_new.clear();
                self.vault_settings_form.change_confirm.clear();
                self.active_input = ActiveInput::Terminal;
                self.active_settings_subpage = None;
                self.maybe_auto_push_sync();
                self.toast_success(tr("toast.vault_pwd_changed"));
            }
            Err(err) => {
                self.toast_error(err);
            }
        }
    }

    /// 从跨平台加密密钥库删除指定主机的关联凭据
    pub fn delete_host_vault_secret(&mut self, host_id: &str, host_name: &str) {
        let (Some(key), Some(data)) = (self.vault_key.as_ref(), self.vault_data.as_mut()) else {
            self.toast_error(tr("toast.vault_unlock_first"));
            return;
        };
        data.host_secrets.remove(host_id);
        match vault::save_vault_with_key(key, data) {
            Ok(_) => {
                if self.vault_revealed_host_id.as_deref() == Some(host_id) {
                    self.vault_revealed_host_id = None;
                }
                self.maybe_auto_push_sync();
                self.toast_success(trf("toast.vault_secret_deleted", &[("host", host_name)]));
            }
            Err(err) => {
                self.toast_error(err);
            }
        }
    }

    pub fn close_vault_modal(&mut self) {
        self.vault_modal = None;
        self.active_input = ActiveInput::Terminal;
    }

    /// 提交首页弹窗中的密钥库设密 / 解锁表单，成功后自动衔接打开新建或编辑主机弹窗
    pub fn submit_vault_modal(&mut self) {
        let Some(draft) = self.vault_modal.clone() else {
            return;
        };
        if draft.is_setup {
            if draft.password.len() < 4 {
                self.toast_error(tr("toast.vault_pwd_too_short"));
                return;
            }
            if draft.password != draft.confirm_password {
                self.toast_error(tr("toast.vault_pwd_mismatch"));
                return;
            }
            match vault::create_new_vault(&draft.password) {
                Ok((key, data)) => {
                    vault::apply_remember_session(&key, self.settings.vault_remember_policy);
                    self.vault_key = Some(key);
                    self.vault_data = Some(data);
                    self.vault_modal = None;
                    self.toast_success(tr("toast.vault_created"));
                    match draft.next_action {
                        VaultModalNextAction::OpenNewHost => self.open_new_host_modal_unlocked(),
                        VaultModalNextAction::OpenEditHost(hid) => {
                            self.open_edit_host_modal_unlocked(&hid)
                        }
                        VaultModalNextAction::TriggerSyncPush => {
                            self.trigger_cloud_sync(SyncAction::Push);
                        }
                        VaultModalNextAction::TriggerSyncForcePush => {
                            self.trigger_cloud_sync(SyncAction::ForcePush);
                        }
                        VaultModalNextAction::ApplyPendingSyncPull => {}
                    }
                }
                Err(err) => {
                    self.toast_error(err);
                }
            }
        } else {
            if draft.password.is_empty() {
                self.toast_error(tr("toast.vault_enter_pwd"));
                return;
            }
            if matches!(draft.next_action, VaultModalNextAction::ApplyPendingSyncPull) {
                let Some(snapshot) = sync::load_pending_pull() else {
                    self.vault_modal = None;
                    self.active_input = ActiveInput::Terminal;
                    return;
                };
                let local_cat = self.build_sync_catalog();
                let local_vfile = vault::load_encrypted_file();
                match sync::unlock_and_merge_snapshot(
                    &draft.password,
                    &snapshot,
                    &local_cat,
                    local_vfile.as_ref(),
                ) {
                    Ok((key, merged_vdata, _vfile, merged_cat)) => {
                        let count_s = merged_cat.hosts.len().to_string();
                        let rev_s = snapshot.revision.to_string();
                        vault::apply_remember_session(&key, self.settings.vault_remember_policy);
                        self.vault_key = Some(key);
                        self.vault_data = Some(merged_vdata);
                        if !snapshot.sync_epoch.is_empty() {
                            self.settings.sync_epoch = snapshot.sync_epoch.clone();
                        }
                        self.settings.sync_vault_salt = snapshot.vault.salt_hex.clone();
                        self.apply_merged_catalog(merged_cat, snapshot.revision);
                        sync::clear_pending_pull();
                        self.vault_modal = None;
                        self.active_input = ActiveInput::Terminal;
                        if self.settings.vault_remember_policy == VaultRememberPolicy::Never {
                            self.lock_vault_now();
                        }
                        self.toast_success(trf(
                            "svc.sync.pull_merged_ok",
                            &[("count", &count_s), ("rev", &rev_s)],
                        ));
                    }
                    Err(err) => {
                        self.toast_error(err);
                    }
                }
                return;
            }
            match vault::unlock_vault(&draft.password) {
                Ok((key, data)) => {
                    vault::apply_remember_session(&key, self.settings.vault_remember_policy);
                    self.vault_key = Some(key);
                    self.vault_data = Some(data);
                    self.try_apply_pending_pull_with_unlocked_vault();
                    self.vault_modal = None;
                    match draft.next_action {
                        VaultModalNextAction::OpenNewHost => self.open_new_host_modal_unlocked(),
                        VaultModalNextAction::OpenEditHost(hid) => {
                            self.open_edit_host_modal_unlocked(&hid)
                        }
                        VaultModalNextAction::TriggerSyncPush => {
                            self.trigger_cloud_sync(SyncAction::Push);
                        }
                        VaultModalNextAction::TriggerSyncForcePush => {
                            self.trigger_cloud_sync(SyncAction::ForcePush);
                        }
                        VaultModalNextAction::ApplyPendingSyncPull => {}
                    }
                }
                Err(err) => {
                    self.toast_error(err);
                }
            }
        }
    }

    /// 在密钥库页通过原生文件选择器添加额外私钥路径
    pub fn prompt_import_vault_key(&mut self, cx: &mut Context<Self>) {
        let rx = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some(tr("settings.vault.import_key_prompt").into()),
        });
        cx.spawn(async move |this, cx| {
            if let Ok(Ok(Some(paths))) = rx.await {
                if let Some(first) = paths.into_iter().next() {
                    let display = abbreviate_home_path(&first);
                    let _ = this.update(cx, |app, cx| {
                        if !app.settings.extra_key_paths.contains(&display) {
                            app.settings.extra_key_paths.push(display.clone());
                            app.settings.save();
                            app.toast_success(trf("toast.vault_key_added", &[("path", &display)]));
                        } else {
                            app.toast_info(trf("toast.vault_key_exists", &[("path", &display)]));
                        }
                        cx.notify();
                    });
                }
            }
        })
        .detach();
    }

    pub fn remove_extra_vault_key(&mut self, key_path: &str) {
        self.settings.extra_key_paths.retain(|p| p != key_path);
        self.settings.save();
        self.toast_info(trf("toast.vault_key_removed", &[("path", key_path)]));
    }

    /// 在 macOS Finder 中显示 ~/.ssh 目录
    pub fn reveal_ssh_dir(&mut self, cx: &mut Context<Self>) {
        if let Some(home) = dirs::home_dir() {
            let ssh_dir = home.join(".ssh");
            if ssh_dir.exists() {
                cx.reveal_path(&ssh_dir);
                self.toast_info(tr("toast.finder_ssh_opened"));
            } else {
                cx.reveal_path(&home);
                self.toast_info(tr("toast.finder_home_opened"));
            }
        }
    }

    /// 在 macOS Finder 中打开 HoloSSH 本地配置目录
    pub fn reveal_config_dir(&mut self, cx: &mut Context<Self>) {
        if let Some(dir) = AppSettings::config_dir() {
            let _ = fs::create_dir_all(&dir);
            cx.reveal_path(&dir);
            let p = dir.display().to_string();
            self.toast_info(trf("toast.finder_dir_opened", &[("path", &p)]));
        }
    }

    pub fn open_update_url(&mut self, cx: &mut Context<Self>) {
        const REPO_URL: &str = "https://github.com/Fokkyp/Holossh";
        cx.open_url(REPO_URL);
        self.toast_info(trf("toast.link_opened", &[("url", REPO_URL)]));
    }

    pub fn set_sync_provider(&mut self, provider: SyncProvider) {
        self.settings.sync_provider = provider;
        self.settings.save();
        self.active_input = ActiveInput::Terminal;
        if matches!(
            self.active_settings_subpage,
            Some(SettingsSubPage::SyncGistConfig | SettingsSubPage::SyncS3Config)
        ) {
            self.active_settings_subpage = None;
        }
    }

    fn has_sync_credentials(&self) -> bool {
        match self.settings.sync_provider {
            SyncProvider::Disabled => false,
            SyncProvider::GitHubGist => !self.settings.sync_gist_token.trim().is_empty(),
            SyncProvider::S3 => {
                !self.settings.s3_endpoint.trim().is_empty()
                    && !self.settings.s3_bucket.trim().is_empty()
                    && !self.settings.s3_access_key_id.trim().is_empty()
                    && !self.settings.sync_s3_secret.trim().is_empty()
            }
        }
    }

    fn build_sync_catalog(&self) -> SyncCatalog {
        SyncCatalog {
            hosts: self.hosts.clone(),
            deleted_hosts: self.settings.deleted_hosts.clone(),
            custom_groups: self.settings.custom_groups.clone(),
            group_updated_at: self.settings.group_updated_at.clone(),
            deleted_groups: self.settings.deleted_groups.clone(),
        }
    }

    fn apply_merged_catalog(&mut self, incoming: SyncCatalog, revision: u64) {
        // 关键防竞态：始终与当前实时本地目录再做一次 CRDT 合并，
        // 防止后台 AutoPull / 上一次 Push 在途期间用户刚删除的文件夹或主机被过期快照“诈尸”覆盖
        let live_local = self.build_sync_catalog();
        let catalog = sync::merge_catalogs(&live_local, &incoming);

        // 保留活跃标签的连接在线状态，仅更新合并后的主机元数据
        let prev_status: HashMap<String, HostStatus> = self
            .hosts
            .iter()
            .map(|h| (h.id.clone(), h.status))
            .collect();
        self.hosts = catalog
            .hosts
            .into_iter()
            .map(|mut h| {
                if let Some(&st) = prev_status.get(&h.id) {
                    h.status = st;
                }
                h
            })
            .collect();
        for tab in &mut self.tabs {
            if let Some(h) = self.hosts.iter().find(|h| h.id == tab.host_id) {
                tab.title = h.name.clone();
                tab.root_pane.update_host_name(&h.id, &h.name);
            }
        }
        self.settings.deleted_hosts = catalog.deleted_hosts;
        self.settings.custom_groups = catalog.custom_groups;
        self.settings.group_updated_at = catalog.group_updated_at;
        self.settings.deleted_groups = catalog.deleted_groups;
        self.settings.sync_base_revision = self.settings.sync_base_revision.max(revision);
        for g in &self.settings.custom_groups {
            self.expanded_groups.insert(g.clone());
        }
        HostProfile::save_all(&self.hosts);
        self.settings.save();
    }

    /// 同步回包写入密钥库时同样与当前实时解锁态合并并尊重最新 `deleted_hosts` 墓碑，防止覆盖并发删除/修改
    fn apply_merged_vault_from_sync(
        &mut self,
        incoming_vdata: Option<VaultData>,
        incoming_vfile: Option<vault::EncryptedVaultFile>,
    ) {
        if let (Some(key), Some(live_vdata), Some(remote_vdata)) = (
            self.vault_key.as_ref(),
            self.vault_data.as_mut(),
            incoming_vdata,
        ) {
            vault::merge_vault_secrets(live_vdata, &remote_vdata, &self.settings.deleted_hosts);
            if let Some(template) = incoming_vfile.as_ref() {
                if let Ok(fresh) = vault::encrypt_vault_with_template(key, template, live_vdata) {
                    let _ = vault::save_encrypted_file(&fresh);
                    return;
                }
            }
            let _ = vault::save_vault_with_key(key, live_vdata);
        } else if let Some(vfile) = incoming_vfile {
            let _ = vault::save_encrypted_file(&vfile);
        }
    }

    fn try_apply_pending_pull_with_unlocked_vault(&mut self) {
        let Some(snapshot) = sync::load_pending_pull() else {
            return;
        };
        let (Some(key), Some(vdata)) = (self.vault_key.as_ref(), self.vault_data.as_ref()) else {
            return;
        };
        let local_cat = self.build_sync_catalog();
        if let Some((merged_vdata, _vfile, merged_cat)) =
            sync::try_merge_snapshot_with_unlocked_key(key, vdata, &local_cat, &snapshot)
        {
            self.vault_data = Some(merged_vdata);
            self.apply_merged_catalog(merged_cat, snapshot.revision);
            sync::clear_pending_pull();
        }
    }

    /// 触发全量端到端加密云端同步操作（Pull-Before-Push / 安全拉取 / 连接测试）
    pub fn trigger_cloud_sync(&mut self, action: SyncAction) {
        if self.sync_in_progress {
            if action == SyncAction::Push {
                self.sync_push_pending = true;
                self.sync_push_after = Some(Instant::now() + Duration::from_millis(800));
            }
            return;
        }
        if self.settings.sync_provider == SyncProvider::Disabled {
            if action != SyncAction::AutoPull {
                self.toast_error(tr("toast.sync_enable_first"));
            }
            return;
        }
        self.try_auto_unlock_remembered();
        self.settings.save();

        // 推送全量加密快照前，确保已存在带 X25519 写入公钥的密钥库
        if matches!(action, SyncAction::Push | SyncAction::ForcePush) {
            let next_act = if action == SyncAction::ForcePush {
                VaultModalNextAction::TriggerSyncForcePush
            } else {
                VaultModalNextAction::TriggerSyncPush
            };
            if !vault::vault_exists() {
                self.modal_anim_gen += 1;
                self.vault_modal = Some(VaultModalDraft {
                    is_setup: true,
                    next_action: next_act,
                    password: String::new(),
                    confirm_password: String::new(),
                    show_password: false,
                });
                self.active_input = ActiveInput::VaultModalPassword;
                return;
            }
            if !self.is_vault_unlocked() && !vault::vault_supports_sealed_write() {
                self.modal_anim_gen += 1;
                self.vault_modal = Some(VaultModalDraft {
                    is_setup: false,
                    next_action: next_act,
                    password: String::new(),
                    confirm_password: String::new(),
                    show_password: false,
                });
                self.active_input = ActiveInput::VaultModalPassword;
                return;
            }
        }

        self.sync_in_progress = true;
        self.sync_active_action = Some(action);

        let req = SyncRequest {
            action,
            settings: self.settings.clone(),
            local_catalog: self.build_sync_catalog(),
            local_vault_file: vault::load_encrypted_file(),
            unlocked_key: self.vault_key,
            unlocked_vault_data: self.vault_data.clone(),
        };
        spawn_sync_task(req, self.sync_event_tx.clone());
    }

    /// 变更自动推送（1.2s 防抖排队，并发编辑或同步中绝不丢弃变更；存在未决冲突弹窗时暂停）
    fn maybe_auto_push_sync(&mut self) {
        if self.settings.sync_auto_on_change
            && self.settings.sync_provider != SyncProvider::Disabled
            && self.has_sync_credentials()
            && self.sync_conflict_modal.is_none()
        {
            self.sync_push_pending = true;
            self.sync_push_after = Some(Instant::now() + Duration::from_millis(1200));
        }
    }

    /// 保存同步后端配置后立刻探测云端是否已有快照（有则弹窗选择拉取/合并/覆盖，无则自动上传本地为初始版本）
    pub fn save_and_probe_cloud_sync(&mut self, provider: SyncProvider) {
        self.settings.sync_provider = provider;
        self.settings.save();
        self.trigger_cloud_sync(SyncAction::ProbeRemote);
    }

    /// 使用云端快照完整覆盖本机主机与分组目录，并对齐云端 sync_epoch / vault_salt / revision
    fn apply_overwrite_catalog(
        &mut self,
        catalog: SyncCatalog,
        revision: u64,
        sync_epoch: String,
        vault_salt: String,
    ) {
        let prev_status: HashMap<String, HostStatus> = self
            .hosts
            .iter()
            .map(|h| (h.id.clone(), h.status))
            .collect();
        self.hosts = catalog
            .hosts
            .into_iter()
            .map(|mut h| {
                if let Some(&st) = prev_status.get(&h.id) {
                    h.status = st;
                }
                h
            })
            .collect();
        for tab in &mut self.tabs {
            if let Some(h) = self.hosts.iter().find(|h| h.id == tab.host_id) {
                tab.title = h.name.clone();
                tab.root_pane.update_host_name(&h.id, &h.name);
            }
        }
        self.settings.deleted_hosts = catalog.deleted_hosts;
        self.settings.custom_groups = catalog.custom_groups;
        self.settings.group_updated_at = catalog.group_updated_at;
        self.settings.deleted_groups = catalog.deleted_groups;
        self.settings.sync_base_revision = revision;
        if !sync_epoch.is_empty() {
            self.settings.sync_epoch = sync_epoch;
        }
        if !vault_salt.is_empty() {
            self.settings.sync_vault_salt = vault_salt;
        }
        self.settings.last_sync_at = Some(sync::format_epoch_ms_local(now_unix_ms()));
        for g in &self.settings.custom_groups {
            self.expanded_groups.insert(g.clone());
        }
        HostProfile::save_all(&self.hosts);
        self.settings.save();
    }

    /// 冲突弹窗选项 1：以云端为准（自动备份本地后覆盖本机）
    pub fn choose_sync_conflict_pull_overwrite(&mut self) {
        let Some(state) = self.sync_conflict_modal.as_ref() else {
            return;
        };
        let snapshot = state.snapshot.clone();
        self.try_auto_unlock_remembered();

        // 若本机已解锁且密钥可直接解开云端快照，一键完成备份与覆盖恢复
        if let Some(key) = self.vault_key {
            if let Some((remote_vdata, _vfile, remote_cat)) =
                sync::adopt_remote_snapshot_overwrite_with_unlocked_key(&key, &snapshot)
            {
                let count_s = remote_cat.hosts.len().to_string();
                let rev_s = snapshot.revision.to_string();
                self.vault_data = Some(remote_vdata);
                self.apply_overwrite_catalog(
                    remote_cat,
                    snapshot.revision,
                    snapshot.sync_epoch.clone(),
                    snapshot.vault.salt_hex.clone(),
                );
                sync::clear_pending_pull();
                self.sync_conflict_modal = None;
                self.active_input = ActiveInput::Terminal;
                self.toast_success(trf(
                    "toast.sync_conflict_pulled",
                    &[("count", &count_s), ("rev", &rev_s)],
                ));
                return;
            }
        }

        // 否则原地切入「输入云端密钥库主密码」步骤
        if let Some(st) = self.sync_conflict_modal.as_mut() {
            st.password_action = Some(SyncConflictPasswordAction::OverwriteLocalFromCloud);
            st.password.clear();
            st.error = None;
        }
        self.set_active_input(ActiveInput::SyncConflictPassword);
    }

    /// 冲突弹窗选项 2：同密钥库下无损双向合并并推送至云端
    pub fn choose_sync_conflict_merge(&mut self) {
        let Some(state) = self.sync_conflict_modal.as_ref() else {
            return;
        };
        let snapshot = state.snapshot.clone();
        self.try_auto_unlock_remembered();

        if let (Some(key), Some(vdata)) = (self.vault_key, self.vault_data.as_ref()) {
            let local_cat = self.build_sync_catalog();
            if let Some((merged_vdata, _vfile, merged_cat)) =
                sync::try_merge_snapshot_with_unlocked_key(&key, vdata, &local_cat, &snapshot)
            {
                self.vault_data = Some(merged_vdata);
                if !snapshot.sync_epoch.is_empty() {
                    self.settings.sync_epoch = snapshot.sync_epoch.clone();
                }
                self.settings.sync_vault_salt = snapshot.vault.salt_hex.clone();
                self.apply_merged_catalog(merged_cat, snapshot.revision);
                sync::clear_pending_pull();
                self.sync_conflict_modal = None;
                self.active_input = ActiveInput::Terminal;
                self.sync_toast_next_result = true;
                self.trigger_cloud_sync(SyncAction::Push);
                return;
            }
        }

        if let Some(st) = self.sync_conflict_modal.as_mut() {
            st.password_action = Some(SyncConflictPasswordAction::MergeBoth);
            st.password.clear();
            st.error = None;
        }
        self.set_active_input(ActiveInput::SyncConflictPassword);
    }

    /// 冲突弹窗选项 3：以本机为准强制覆盖云端（生成全新 sync_epoch，并在本次成功/失败时提示一次）
    pub fn choose_sync_conflict_push_overwrite(&mut self) {
        self.sync_conflict_modal = None;
        self.active_input = ActiveInput::Terminal;
        sync::clear_pending_pull();
        self.sync_toast_next_result = true;
        self.trigger_cloud_sync(SyncAction::ForcePush);
    }

    pub fn back_from_sync_conflict_password(&mut self) {
        if let Some(st) = self.sync_conflict_modal.as_mut() {
            st.password_action = None;
            st.password.clear();
            st.error = None;
        }
        self.active_input = ActiveInput::Terminal;
    }

    pub fn toggle_sync_conflict_password_visibility(&mut self) {
        if let Some(st) = self.sync_conflict_modal.as_mut() {
            st.show_password = !st.show_password;
        }
    }

    /// 关闭同步冲突弹窗（分叉场景下勾选暂停自动同步，避免反复弹窗打扰）
    pub fn dismiss_sync_conflict_modal(&mut self, pause_auto_sync: bool) {
        self.sync_conflict_modal = None;
        self.active_input = ActiveInput::Terminal;
        self.sync_push_pending = false;
        self.sync_push_after = None;
        if pause_auto_sync {
            self.settings.sync_auto_on_change = false;
            self.settings.sync_auto_pull_on_start = false;
            self.settings.save();
            self.toast_info(tr("toast.sync_conflict_paused"));
        }
    }

    /// 提交同步冲突弹窗内嵌的云端密钥库主密码
    pub fn submit_sync_conflict_password(&mut self) {
        let Some(state) = self.sync_conflict_modal.clone() else {
            return;
        };
        let Some(action) = state.password_action else {
            return;
        };
        if state.password.is_empty() {
            if let Some(st) = self.sync_conflict_modal.as_mut() {
                st.error = Some(tr("toast.vault_enter_pwd").to_string());
            }
            return;
        }

        match action {
            SyncConflictPasswordAction::OverwriteLocalFromCloud => {
                match sync::adopt_remote_snapshot_overwrite(&state.password, &state.snapshot) {
                    Ok((key, remote_vdata, _vfile, remote_cat)) => {
                        let count_s = remote_cat.hosts.len().to_string();
                        let rev_s = state.snapshot.revision.to_string();
                        vault::apply_remember_session(&key, self.settings.vault_remember_policy);
                        self.vault_key = Some(key);
                        self.vault_data = Some(remote_vdata);
                        self.apply_overwrite_catalog(
                            remote_cat,
                            state.snapshot.revision,
                            state.snapshot.sync_epoch.clone(),
                            state.snapshot.vault.salt_hex.clone(),
                        );
                        sync::clear_pending_pull();
                        self.sync_conflict_modal = None;
                        self.active_input = ActiveInput::Terminal;
                        if self.settings.vault_remember_policy == VaultRememberPolicy::Never {
                            self.lock_vault_now();
                        }
                        self.toast_success(trf(
                            "toast.sync_conflict_pulled",
                            &[("count", &count_s), ("rev", &rev_s)],
                        ));
                    }
                    Err(err) => {
                        if let Some(st) = self.sync_conflict_modal.as_mut() {
                            st.error = Some(err);
                        }
                    }
                }
            }
            SyncConflictPasswordAction::MergeBoth => {
                let local_cat = self.build_sync_catalog();
                let local_vfile = vault::load_encrypted_file();
                match sync::unlock_and_merge_snapshot(
                    &state.password,
                    &state.snapshot,
                    &local_cat,
                    local_vfile.as_ref(),
                ) {
                    Ok((key, merged_vdata, _vfile, merged_cat)) => {
                        vault::apply_remember_session(&key, self.settings.vault_remember_policy);
                        self.vault_key = Some(key);
                        self.vault_data = Some(merged_vdata);
                        if !state.snapshot.sync_epoch.is_empty() {
                            self.settings.sync_epoch = state.snapshot.sync_epoch.clone();
                        }
                        self.settings.sync_vault_salt = state.snapshot.vault.salt_hex.clone();
                        self.apply_merged_catalog(merged_cat, state.snapshot.revision);
                        sync::clear_pending_pull();
                        self.sync_conflict_modal = None;
                        self.active_input = ActiveInput::Terminal;
                        self.sync_toast_next_result = true;
                        self.trigger_cloud_sync(SyncAction::Push);
                    }
                    Err(err) => {
                        if let Some(st) = self.sync_conflict_modal.as_mut() {
                            st.error = Some(err);
                        }
                    }
                }
            }
        }
    }

    /// 恢复全部应用设置为出厂默认值（保留已完成首次引导状态与自定义文件夹）
    pub fn reset_settings_to_default(&mut self) {
        let preserved_groups = self.settings.custom_groups.clone();
        let mut defaults = AppSettings::default();
        defaults.onboarding_completed = true;
        defaults.custom_groups = preserved_groups;
        self.settings = defaults;
        i18n::set_language(self.settings.language);
        self.settings.save();
        self.sync_active_tab_pty_sizes();
        self.toast_success(tr("toast.settings_reset"));
    }

    pub fn select_settings_section(&mut self, section: SettingsSection) {
        self.active_dropdown = None;
        self.recording_shortcut = None;
        self.active_input = ActiveInput::Terminal;
        self.active_settings_section = section;
        self.active_settings_subpage = None;
        if section == SettingsSection::Vault {
            self.refresh_vault_state();
        }
    }

    pub fn toggle_recording_shortcut(&mut self, action: ShortcutAction) {
        self.active_dropdown = None;
        if self.recording_shortcut == Some(action) {
            self.recording_shortcut = None;
        } else {
            self.recording_shortcut = Some(action);
        }
    }

    pub fn reset_shortcuts_to_default(&mut self) {
        self.recording_shortcut = None;
        self.settings.shortcuts = ShortcutBindings::default();
        self.settings.save();
        self.toast_success(tr("toast.shortcuts_reset"));
    }

    pub fn open_settings_subpage(&mut self, subpage: SettingsSubPage) {
        self.active_dropdown = None;
        self.recording_shortcut = None;
        self.vault_revealed_host_id = None;
        self.active_settings_subpage = Some(subpage);
        self.active_input = match subpage {
            SettingsSubPage::VaultChangePassword => ActiveInput::VaultChangeOld,
            _ => ActiveInput::Terminal,
        };
        if matches!(subpage, SettingsSubPage::VaultLocalKeys) {
            self.vault_local_keys = ssh_keys::discover_local_ssh_keys();
        }
    }

    pub fn pop_settings_subpage(&mut self) {
        self.active_dropdown = None;
        self.active_input = ActiveInput::Terminal;
        self.vault_revealed_host_id = None;
        self.active_settings_subpage = None;
    }

    pub fn toggle_sidebar(&mut self) {
        self.sidebar_collapsed = !self.sidebar_collapsed;
        self.sidebar_anim_gen += 1;
        self.sync_active_tab_pty_sizes();
    }

    /// 开始拖拽侧边栏中线（支持左右两侧布局与设置页侧栏）
    pub fn start_drag_sidebar(&mut self, pos: Point<Pixels>) {
        self.active_drag = Some(ActiveDrag {
            kind: DragKind::Sidebar,
            start_pos: pos,
            start_value: self.settings.sidebar_width.clamp(190.0, 440.0),
        });
    }

    /// 开始拖拽右侧 SFTP 面板中线
    pub fn start_drag_sftp(&mut self, pos: Point<Pixels>) {
        self.active_drag = Some(ActiveDrag {
            kind: DragKind::Sftp,
            start_pos: pos,
            start_value: self.settings.sftp_width.clamp(280.0, 680.0),
        });
    }

    /// 开始拖拽分屏终端窗格中线（水平左右或垂直上下）
    pub fn start_drag_split(&mut self, path: String, axis: SplitAxis, pos: Point<Pixels>) {
        let current_ratio = self
            .tabs
            .iter()
            .find(|t| t.id == self.active_tab_id)
            .and_then(|t| t.root_pane.ratio_at_path(&path))
            .unwrap_or(0.5);
        self.active_drag = Some(ActiveDrag {
            kind: DragKind::SplitPane { path, axis },
            start_pos: pos,
            start_value: current_ratio,
        });
    }

    pub fn begin_host_press(&mut self, host_id: &str, pos: Point<Pixels>) {
        let Some(host) = self.hosts.iter().find(|h| h.id == host_id) else {
            return;
        };
        self.suppress_next_host_click = false;
        self.host_drag = Some(HostDragState {
            host_id: host.id.clone(),
            host_name: host.name.clone(),
            host_ip: host.hostname.clone(),
            connected: host.status == HostStatus::Connected,
            start_pos: pos,
            current_pos: pos,
            pressed_at: Instant::now(),
            active: false,
            hover_target: None,
            insertion_line: None,
        });
    }

    pub fn handle_host_click(&mut self, host_id: &str) {
        if std::mem::take(&mut self.suppress_next_host_click) {
            return;
        }
        self.select_host(host_id);
    }

    fn hit_test_host_drop_target(
        &self,
        pos: Point<Pixels>,
    ) -> (Option<HostDropTarget>, Option<DropInsertionLine>) {
        let zones = self.host_drop_zones.borrow();
        if zones.is_empty() {
            return (None, None);
        }
        let px_x: f32 = pos.x.into();
        let px_y: f32 = pos.y.into();

        // 计算侧栏内容区水平范围，避免光标拖到右侧终端区时仍误触发侧栏落点
        let mut min_x = f32::INFINITY;
        let mut max_x = f32::NEG_INFINITY;
        for (_, b) in zones.iter() {
            let bx: f32 = b.origin.x.into();
            let bw: f32 = b.size.width.into();
            min_x = min_x.min(bx);
            max_x = max_x.max(bx + bw);
        }
        if px_x < min_x - 20.0 || px_x > max_x + 20.0 {
            return (None, None);
        }

        let dragged_id = self.host_drag.as_ref().map(|d| d.host_id.as_str());

        // 1. 若光标尖落在某个文件夹标题行中部（上下各留 25% 给行间横条），高亮该文件夹以移入
        for (zone, b) in zones.iter() {
            if let HostDropZone::FolderHeader { group } = zone {
                let top: f32 = b.origin.y.into();
                let h: f32 = b.size.height.into();
                if px_y >= top + h * 0.25 && px_y <= top + h * 0.80 {
                    return (Some(HostDropTarget::Folder(group.clone())), None);
                }
            }
        }

        // 2. 收集所有行间插入槽位（列表项上方/中间缝隙/下方 + 首个文件夹上方），吸附到距光标尖最近的槽位
        let mut slots: Vec<(f32, f32, f32, HostDropTarget)> = Vec::new();
        let has_ungrouped = zones.iter().any(|(z, _)| {
            matches!(z, HostDropZone::HostRow { group, .. } if group.is_empty())
        });

        // 若无无分组主机，则首个文件夹正上方（或「主机」标题正下方）即顶部置顶/移出文件夹插入槽
        if !has_ungrouped {
            if let Some((_, b)) = zones
                .iter()
                .find(|(z, _)| matches!(z, HostDropZone::FolderHeader { .. }))
            {
                let bx: f32 = b.origin.x.into();
                let bw: f32 = b.size.width.into();
                let by: f32 = b.origin.y.into();
                slots.push((by - 2.0, bx, bw, HostDropTarget::TopRoot));
            } else if let Some((_, b)) = zones
                .iter()
                .find(|(z, _)| matches!(z, HostDropZone::TopHeader))
            {
                let bx: f32 = b.origin.x.into();
                let bw: f32 = b.size.width.into();
                let by: f32 = b.origin.y.into();
                let bh: f32 = b.size.height.into();
                slots.push((by + bh + 4.0, bx, bw, HostDropTarget::TopRoot));
            }
        }

        // 每个主机行的上边（行间缝隙中点）与下边均为有效插入槽位
        for (zone, b) in zones.iter() {
            if let HostDropZone::HostRow {
                host_id,
                group,
                index_in_group,
            } = zone
            {
                let bx: f32 = b.origin.x.into();
                let bw: f32 = b.size.width.into();
                let top: f32 = b.origin.y.into();
                let bottom: f32 = top + Into::<f32>::into(b.size.height);
                if dragged_id != Some(host_id.as_str()) {
                    let target_before = if group.is_empty() && *index_in_group == 0 {
                        HostDropTarget::TopRoot
                    } else {
                        HostDropTarget::InsertBefore {
                            host_id: host_id.clone(),
                            group: group.clone(),
                        }
                    };
                    slots.push((top - 1.0, bx, bw, target_before));
                    slots.push((
                        bottom + 1.0,
                        bx,
                        bw,
                        HostDropTarget::InsertAfter {
                            host_id: host_id.clone(),
                            group: group.clone(),
                        },
                    ));
                }
            }
        }

        // 选取垂直距离鼠标尖最近的槽位（允许 38px 吸附范围，覆盖行间缝隙与标题下方）
        let mut best: Option<(f32, f32, f32, f32, HostDropTarget)> = None;
        for (sy, sx, sw, target) in slots {
            let dist = (px_y - sy).abs();
            if dist <= 38.0 {
                if best.as_ref().is_none_or(|b| dist < b.0) {
                    best = Some((dist, sy, sx, sw, target));
                }
            }
        }

        if let Some((_, sy, sx, sw, target)) = best {
            (
                Some(target),
                Some(DropInsertionLine {
                    x: sx,
                    y: sy,
                    width: sw,
                }),
            )
        } else {
            (None, None)
        }
    }

    /// 将主机插入到指定分组内某台主机的上方或下方（自动重排该分组内 created_at_ms 保持精确顺序）
    pub fn move_host_relative(
        &mut self,
        drag_host_id: &str,
        anchor_host_id: &str,
        target_group: &str,
        insert_before: bool,
    ) {
        if drag_host_id == anchor_host_id {
            return;
        }
        let target = target_group.trim().to_string();
        // 按当前倒序收集目标分组内除被拖主机外的有序 ID 列表
        let mut ordered_ids: Vec<(String, u64)> = self
            .hosts
            .iter()
            .filter(|h| h.id != drag_host_id && h.group.trim() == target.as_str())
            .map(|h| (h.id.clone(), h.effective_created_at_ms()))
            .collect();
        ordered_ids.sort_by(|a, b| b.1.cmp(&a.1));

        let mut ids: Vec<String> = ordered_ids.into_iter().map(|(id, _)| id).collect();
        let insert_idx = if let Some(pos) = ids.iter().position(|id| id == anchor_host_id) {
            if insert_before { pos } else { pos + 1 }
        } else {
            0
        };
        ids.insert(insert_idx, drag_host_id.to_string());

        let now_ms = now_unix_ms();
        let max_existing = self
            .hosts
            .iter()
            .map(|h| h.effective_created_at_ms())
            .max()
            .unwrap_or(0);
        let base_ts = now_ms.max(max_existing.saturating_add(ids.len() as u64 + 1));

        for (i, id) in ids.iter().enumerate() {
            if let Some(h) = self.hosts.iter_mut().find(|h| &h.id == id) {
                h.created_at_ms = base_ts.saturating_sub(i as u64);
                if id == drag_host_id {
                    h.group = target.clone();
                }
                h.updated_at_ms = now_ms;
            }
        }
        if !target.is_empty() {
            self.expanded_groups.insert(target);
        }
        HostProfile::save_all(&self.hosts);
        self.maybe_auto_push_sync();
    }

    /// 将主机取消分组并置顶（更新 created_at_ms 为最大值 + 1，持久且自动云同步）
    pub fn move_host_to_top(&mut self, host_id: &str) {
        let max_created = self
            .hosts
            .iter()
            .map(|h| h.effective_created_at_ms())
            .max()
            .unwrap_or(0);
        let now_ms = now_unix_ms();
        let top_ts = now_ms.max(max_created.saturating_add(1));
        let Some(host) = self.hosts.iter_mut().find(|h| h.id == host_id) else {
            return;
        };
        host.group.clear();
        host.created_at_ms = top_ts;
        host.updated_at_ms = now_ms;
        HostProfile::save_all(&self.hosts);
        self.maybe_auto_push_sync();
    }

    /// 将主机拖入指定文件夹（自动展开目标文件夹，持久且自动云同步）
    pub fn move_host_to_group(&mut self, host_id: &str, target_group: &str) {
        let target = target_group.trim();
        if target.is_empty() {
            self.move_host_to_top(host_id);
            return;
        }
        let max_in_group = self
            .hosts
            .iter()
            .filter(|h| h.group.trim() == target)
            .map(|h| h.effective_created_at_ms())
            .max()
            .unwrap_or(0);
        let now_ms = now_unix_ms();
        let Some(host) = self.hosts.iter_mut().find(|h| h.id == host_id) else {
            return;
        };
        if host.group.trim() == target {
            return;
        }
        host.group = target.to_string();
        host.created_at_ms = now_ms.max(max_in_group.saturating_add(1));
        host.updated_at_ms = now_ms;
        self.expanded_groups.insert(target.to_string());
        HostProfile::save_all(&self.hosts);
        self.maybe_auto_push_sync();
    }

    fn finish_host_drag(&mut self) -> bool {
        let Some(drag) = self.host_drag.take() else {
            return false;
        };
        if !drag.active {
            return false;
        }
        self.suppress_next_host_click = true;
        match drag.hover_target {
            Some(HostDropTarget::TopRoot) => {
                self.move_host_to_top(&drag.host_id);
            }
            Some(HostDropTarget::Folder(group)) => {
                self.move_host_to_group(&drag.host_id, &group);
            }
            Some(HostDropTarget::InsertBefore { host_id, group }) => {
                self.move_host_relative(&drag.host_id, &host_id, &group, true);
            }
            Some(HostDropTarget::InsertAfter { host_id, group }) => {
                self.move_host_relative(&drag.host_id, &host_id, &group, false);
            }
            None => {}
        }
        true
    }

    fn handle_mouse_move(
        &mut self,
        event: &MouseMoveEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.text_cursor.dragging {
            if !event.dragging() {
                self.text_cursor.dragging = false;
                return;
            }
            let target = self.active_input;
            if target != ActiveInput::Terminal {
                if let Some(hit_idx) = hit_test_text_input(target, event.position) {
                    if let Some(text) = self.text_for_input(target) {
                        self.text_cursor.extend_to(hit_idx, &text);
                        cx.notify();
                    }
                }
            }
            return;
        }

        if self.host_drag.is_some() {
            if !event.dragging() {
                if self.finish_host_drag() {
                    cx.notify();
                }
                return;
            }
            let cur = event.position;
            let mut should_notify = false;
            let mut is_now_active = false;
            if let Some(drag) = self.host_drag.as_mut() {
                drag.current_pos = cur;
                if !drag.active {
                    let sx: f32 = drag.start_pos.x.into();
                    let sy: f32 = drag.start_pos.y.into();
                    let cx_f: f32 = cur.x.into();
                    let cy_f: f32 = cur.y.into();
                    let dist = ((cx_f - sx).powi(2) + (cy_f - sy).powi(2)).sqrt();
                    if drag.pressed_at.elapsed() >= Duration::from_millis(220)
                        || (drag.pressed_at.elapsed() >= Duration::from_millis(140) && dist >= 6.0)
                    {
                        drag.active = true;
                        should_notify = true;
                    }
                }
                is_now_active = drag.active;
            }
            if is_now_active {
                let (target, line) = self.hit_test_host_drop_target(cur);
                if let Some(drag) = self.host_drag.as_mut() {
                    drag.hover_target = target;
                    drag.insertion_line = line;
                }
                should_notify = true;
            }
            if should_notify {
                cx.notify();
            }
            if is_now_active {
                return;
            }
        }

        // 记录终端跨屏拖选期间的全局鼠标位置（供鼠标移出终端画布上下边沿时自动滚屏扩选）
        if self.terminal_drag_anchor.is_some() {
            if event.dragging() {
                self.last_drag_mouse_pos = Some(event.position);
            } else {
                self.last_drag_mouse_pos = None;
            }
        }

        let Some(drag) = self.active_drag.clone() else {
            return;
        };
        if !event.dragging() {
            self.finish_active_drag();
            return;
        }
        let vp = window.viewport_size();
        let win_w: f32 = vp.width.into();
        let win_h: f32 = vp.height.into();
        let cur_x: f32 = event.position.x.into();
        let cur_y: f32 = event.position.y.into();
        let start_x: f32 = drag.start_pos.x.into();
        let start_y: f32 = drag.start_pos.y.into();
        let dx = cur_x - start_x;
        let dy = cur_y - start_y;

        match drag.kind {
            DragKind::Sidebar => {
                let signed_dx = if self.screen == AppScreen::Workspace
                    && self.settings.sidebar_side == SidebarSide::Right
                {
                    -dx
                } else {
                    dx
                };
                let max_sb = (win_w * 0.45).clamp(240.0, 480.0);
                self.settings.sidebar_width = (drag.start_value + signed_dx).clamp(190.0, max_sb);
                cx.notify();
            }
            DragKind::Sftp => {
                let sb_w = if self.sidebar_collapsed {
                    0.0
                } else {
                    self.settings.sidebar_width
                };
                let max_sftp = ((win_w - sb_w) * 0.72).clamp(300.0, 740.0);
                // SFTP 位于右侧，往左拖（dx < 0）变宽、往右拖（dx > 0）变窄
                self.settings.sftp_width = (drag.start_value - dx).clamp(260.0, max_sftp);
                cx.notify();
            }
            DragKind::SplitPane { path, axis } => {
                let sb_w = if self.sidebar_collapsed {
                    0.0
                } else {
                    self.settings.sidebar_width
                };
                let sftp_w = self
                    .tabs
                    .iter()
                    .find(|t| t.id == self.active_tab_id)
                    .filter(|t| t.sftp_open)
                    .map(|_| self.settings.sftp_width)
                    .unwrap_or(0.0);
                let root_span = match axis {
                    SplitAxis::Horizontal => (win_w - sb_w - sftp_w).max(240.0),
                    SplitAxis::Vertical => {
                        (win_h - crate::theme::LayoutMetrics::TITLEBAR_HEIGHT).max(200.0)
                    }
                };
                if let Some(tab) = self.tabs.iter_mut().find(|t| t.id == self.active_tab_id) {
                    let frac = tab.root_pane.span_fraction_at_path(&path, axis).max(0.20);
                    let span = (root_span * frac).max(160.0);
                    let delta_ratio = match axis {
                        SplitAxis::Horizontal => dx / span,
                        SplitAxis::Vertical => dy / span,
                    };
                    tab.root_pane
                        .set_ratio_at_path(&path, drag.start_value + delta_ratio);
                }
                cx.notify();
            }
        }
    }

    fn handle_mouse_up(
        &mut self,
        _event: &MouseUpEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let mut dirty = false;
        if self.text_cursor.dragging {
            self.text_cursor.dragging = false;
            dirty = true;
        }
        if self.finish_host_drag() {
            dirty = true;
        }
        if self.active_drag.is_some() {
            self.finish_active_drag();
            dirty = true;
        }
        if let Some(anchor) = self.terminal_drag_anchor.take() {
            self.last_drag_mouse_pos = None;
            self.last_edge_scroll_at = None;
            if anchor.started_selection && self.settings.copy_on_select {
                self.copy_terminal_selection(cx);
            }
            dirty = true;
        }
        if dirty {
            cx.notify();
        }
    }

    fn finish_active_drag(&mut self) {
        if self.active_drag.take().is_some() {
            self.settings.save();
            self.sync_active_tab_pty_sizes();
        }
    }

    /// 读取 `TerminalElement::prepaint` 产出的精确 GPU 行列数：本地仿真器立即更新，远端 PTY `window_change` 做 50ms 尾部防抖
    fn sync_measured_pane_pty_sizes(&mut self) -> bool {
        let snapshot: Vec<(String, usize, usize)> = self
            .pane_metrics
            .borrow()
            .iter()
            .map(|(pid, m)| (pid.clone(), m.cols, m.rows))
            .collect();
        let mut changed = false;
        let deadline = Instant::now() + Duration::from_millis(50);
        for (pid, cols, rows) in snapshot {
            if self.last_pane_pty_sizes.get(&pid).copied() == Some((cols, rows)) {
                continue;
            }
            self.last_pane_pty_sizes.insert(pid.clone(), (cols, rows));
            if let Some(emu) = self.pane_emulators.get_mut(&pid) {
                emu.resize(cols, rows);
                changed = true;
            }
            if self.pane_handles.contains_key(&pid) {
                self.pending_pty_resizes.insert(pid, (cols, rows, deadline));
            }
        }
        changed
    }

    /// 冲刷已过 50ms 尾部防抖期的远端 PTY `window_change` 缩放请求
    fn flush_debounced_pty_resizes(&mut self) {
        if self.pending_pty_resizes.is_empty() {
            return;
        }
        let now = Instant::now();
        let ready: Vec<(String, usize, usize)> = self
            .pending_pty_resizes
            .iter()
            .filter_map(|(pid, &(cols, rows, due))| (now >= due).then(|| (pid.clone(), cols, rows)))
            .collect();
        for (pid, cols, rows) in ready {
            self.pending_pty_resizes.remove(&pid);
            if let Some(handle) = self.pane_handles.get(&pid) {
                handle.resize(cols as u32, rows as u32);
            }
        }
    }

    /// 鼠标在终端画布外上下边沿拖选时自动滚动历史回滚缓冲并实时扩选
    fn tick_terminal_drag_auto_scroll(&mut self) -> bool {
        let (Some(anchor), Some(pos)) = (self.terminal_drag_anchor.as_mut(), self.last_drag_mouse_pos)
        else {
            return false;
        };
        if self
            .last_edge_scroll_at
            .is_some_and(|t| t.elapsed() < Duration::from_millis(40))
        {
            return false;
        }
        let Some(m) = self.pane_metrics.borrow().get(&anchor.pane_id).copied() else {
            return false;
        };
        let top: f32 = m.bounds.origin.y.into();
        let height: f32 = m.bounds.size.height.into();
        let bottom = top + height;
        let py: f32 = pos.y.into();
        let delta: i32 = if py < top {
            (((top - py) / m.line_height.max(12.0)).ceil() as i32).clamp(1, 3)
        } else if py > bottom {
            -((((py - bottom) / m.line_height.max(12.0)).ceil() as i32).clamp(1, 3))
        } else {
            return false;
        };

        let left: f32 = m.bounds.origin.x.into();
        let px_f: f32 = pos.x.into();
        let rel_x = (px_f - left).max(0.0);
        let col = ((rel_x / m.cell_width.max(4.0)).floor() as usize).min(m.cols.saturating_sub(1));
        let edge_row = if delta > 0 {
            0
        } else {
            m.rows.saturating_sub(1)
        };
        let side = if delta > 0 { Side::Left } else { Side::Right };

        let pane_id = anchor.pane_id.clone();
        let (ac, ar, asd, ty) = (anchor.col, anchor.row, anchor.side, anchor.ty);
        let started = anchor.started_selection;
        let Some(emu) = self.pane_emulators.get_mut(&pane_id) else {
            return false;
        };
        if !emu.scroll_display(delta) {
            return false;
        }
        if !started {
            anchor.started_selection = true;
            emu.start_selection(ac, ar, ty, asd);
        }
        emu.update_selection(col, edge_row, side);
        self.last_edge_scroll_at = Some(Instant::now());
        true
    }

    /// 意外断线后按 2s → 4s → 8s 指数退避排期自动重连（最多 3 次）
    fn schedule_auto_reconnect(&mut self, pane_id: &str, host_id: &str) -> bool {
        const MAX_AUTO_RECONNECT_ATTEMPTS: u8 = 3;
        if !self.hosts.iter().any(|h| h.id == host_id) {
            return false;
        }
        let next_attempt = self
            .pane_reconnect_attempts
            .get(pane_id)
            .copied()
            .unwrap_or(0)
            .saturating_add(1);
        if next_attempt > MAX_AUTO_RECONNECT_ATTEMPTS {
            return false;
        }
        let delay_secs = 1u64 << next_attempt; // 2, 4, 8 秒
        self.pane_reconnect_attempts
            .insert(pane_id.to_string(), next_attempt);
        self.pending_reconnects.insert(
            pane_id.to_string(),
            PendingReconnect {
                host_id: host_id.to_string(),
                next_at: Instant::now() + Duration::from_secs(delay_secs),
            },
        );
        if let Some(emu) = self.pane_emulators.get_mut(pane_id) {
            emu.write_notice(&trf(
                "term.auto_reconnect_notice",
                &[
                    ("secs", &delay_secs.to_string()),
                    ("attempt", &next_attempt.to_string()),
                    ("max", &MAX_AUTO_RECONNECT_ATTEMPTS.to_string()),
                ],
            ));
        }
        true
    }

    /// 检查并触发到期的自动重连任务
    fn tick_pending_reconnects(&mut self) -> bool {
        if self.pending_reconnects.is_empty() {
            return false;
        }
        let now = Instant::now();
        let due: Vec<(String, PendingReconnect)> = self
            .pending_reconnects
            .iter()
            .filter_map(|(pid, pr)| (now >= pr.next_at).then(|| (pid.clone(), pr.clone())))
            .collect();
        if due.is_empty() {
            return false;
        }
        let mut fired = false;
        for (pane_id, pr) in due {
            self.pending_reconnects.remove(&pane_id);
            let pane_exists = self
                .tabs
                .iter()
                .any(|t| t.root_pane.find_pane(&pane_id).is_some());
            if !pane_exists || self.pane_handles.contains_key(&pane_id) {
                continue;
            }
            if let Some(host) = self.hosts.iter().find(|h| h.id == pr.host_id).cloned() {
                // 保留退避计数以支持连续失败时推进到下一次退避（2s -> 4s -> 8s）
                self.connect_pane_inner(&pane_id, &host, None, None, true);
                fired = true;
            }
        }
        fired
    }

    /// 按当前真实窗口尺寸与布局（优先使用 GPU 实测 `pane_metrics`）动态同步本地仿真器与远程 SSH PTY 行列
    fn sync_active_tab_pty_sizes(&mut self) {
        let Some(tab) = self.tabs.iter().find(|t| t.id == self.active_tab_id) else {
            return;
        };
        let (win_w, win_h) = self.last_viewport_size;
        let mut avail_w = win_w.max(640.0);
        if !self.sidebar_collapsed {
            avail_w -= self.settings.sidebar_width;
        }
        if tab.sftp_open {
            avail_w -= self.settings.sftp_width;
        }
        let pane_count = tab.root_pane.pane_count().max(1);
        let pane_w = if pane_count > 1 {
            (avail_w / 2.0) - 32.0
        } else {
            avail_w - 40.0
        };
        let pane_h = if pane_count > 2 {
            (win_h.max(480.0) - 120.0) / 2.0
        } else {
            win_h.max(480.0) - 96.0
        };

        let char_w = (self.settings.terminal_font_size * 0.62).max(6.5);
        let line_h = (self.settings.terminal_font_size * 1.44).max(15.0);
        let est_cols = (pane_w / char_w).floor().clamp(36.0, 220.0) as usize;
        let est_rows = (pane_h / line_h).floor().clamp(12.0, 80.0) as usize;

        let mut ids = Vec::new();
        tab.root_pane.collect_pane_ids(&mut ids);
        let metrics = self.pane_metrics.borrow();
        let deadline = Instant::now() + Duration::from_millis(50);
        for pid in ids {
            let (cols, rows) = metrics
                .get(&pid)
                .map(|m| (m.cols, m.rows))
                .unwrap_or((est_cols, est_rows));
            if self.last_pane_pty_sizes.get(&pid).copied() == Some((cols, rows)) {
                continue;
            }
            self.last_pane_pty_sizes.insert(pid.clone(), (cols, rows));
            if let Some(emu) = self.pane_emulators.get_mut(&pid) {
                emu.resize(cols, rows);
            }
            if self.pane_handles.contains_key(&pid) {
                self.pending_pty_resizes.insert(pid, (cols, rows, deadline));
            }
        }
    }

    pub fn open_settings(&mut self) {
        self.active_input = ActiveInput::Terminal;
        self.active_settings_subpage = None;
        if self.active_settings_section == SettingsSection::Vault {
            self.refresh_vault_state();
        }
        self.screen = AppScreen::Settings;
    }

    pub fn close_settings(&mut self) {
        self.active_dropdown = None;
        self.recording_shortcut = None;
        self.active_settings_subpage = None;
        self.vault_revealed_host_id = None;
        if self.settings.vault_remember_policy == VaultRememberPolicy::Never {
            self.lock_vault_now();
        }
        self.screen = AppScreen::Workspace;
    }

    pub fn complete_onboarding(&mut self) {
        if self.import_ssh_config_on_start && !self.detected_ssh_config_hosts.is_empty() {
            let mut added = 0usize;
            for imported in self.detected_ssh_config_hosts.clone() {
                if !self.hosts.iter().any(|h| {
                    h.hostname == imported.hostname
                        && h.port == imported.port
                        && h.username == imported.username
                }) {
                    self.hosts.push(imported);
                    added += 1;
                }
            }
            if added > 0 {
                HostProfile::save_all(&self.hosts);
                self.maybe_auto_push_sync();
            }
        }
        self.settings.onboarding_completed = true;
        self.settings.save();
        self.screen = AppScreen::Workspace;
    }

    pub fn open_new_host_modal(&mut self) {
        self.host_context_menu = None;
        self.group_context_menu = None;
        self.sftp_context_menu = None;
        self.terminal_context_menu = None;
        self.active_dropdown = None;
        if !vault::vault_exists() {
            self.modal_anim_gen += 1;
            self.vault_modal = Some(VaultModalDraft {
                is_setup: true,
                next_action: VaultModalNextAction::OpenNewHost,
                password: String::new(),
                confirm_password: String::new(),
                show_password: false,
            });
            self.active_input = ActiveInput::VaultModalPassword;
            return;
        }
        // 规则：写入不弹解锁窗，读取时才弹！若密钥库已具备 X25519 写入公钥，直接打开新建主机弹窗
        let _ = self.try_auto_unlock_remembered();
        if !self.is_vault_unlocked() && !vault::vault_supports_sealed_write() {
            self.modal_anim_gen += 1;
            self.vault_modal = Some(VaultModalDraft {
                is_setup: false,
                next_action: VaultModalNextAction::OpenNewHost,
                password: String::new(),
                confirm_password: String::new(),
                show_password: false,
            });
            self.active_input = ActiveInput::VaultModalPassword;
            return;
        }
        self.open_new_host_modal_unlocked();
    }

    fn open_new_host_modal_unlocked(&mut self) {
        self.editing_host_id = None;
        self.new_host_draft = NewHostDraft::from_settings(&self.settings);
        self.modal_anim_gen += 1;
        self.show_new_host_modal = true;
        self.active_input = ActiveInput::ModalName;
    }

    pub fn close_new_host_modal(&mut self) {
        self.active_dropdown = None;
        self.show_new_host_modal = false;
        self.editing_host_id = None;
        self.active_input = ActiveInput::Terminal;
        if self.settings.vault_remember_policy == VaultRememberPolicy::Never {
            self.lock_vault_now();
        }
    }

    pub fn open_host_context_menu(&mut self, host_id: &str, position: Point<Pixels>) {
        self.active_dropdown = None;
        self.group_context_menu = None;
        self.ctx_menu_anim_gen += 1;
        self.host_context_menu = Some(HostContextMenu {
            host_id: host_id.to_string(),
            position,
        });
    }

    pub fn close_host_context_menu(&mut self) {
        self.host_context_menu = None;
    }

    pub fn open_group_context_menu(&mut self, group_name: &str, position: Point<Pixels>) {
        self.active_dropdown = None;
        self.host_context_menu = None;
        self.ctx_menu_anim_gen += 1;
        self.group_context_menu = Some(GroupContextMenu {
            group_name: group_name.to_string(),
            position,
        });
    }

    pub fn close_group_context_menu(&mut self) {
        self.group_context_menu = None;
    }

    /// 文件夹右键「修改」：原地切换为内联输入框重命名文件夹
    pub fn start_renaming_group(&mut self, group_name: &str) {
        self.group_context_menu = None;
        self.creating_group = false;
        self.renaming_group = Some(group_name.to_string());
        self.rename_group_input = group_name.to_string();
        self.active_input = ActiveInput::SidebarRenameGroup;
    }

    pub fn cancel_renaming_group(&mut self) {
        self.renaming_group = None;
        self.rename_group_input.clear();
        if self.active_input == ActiveInput::SidebarRenameGroup {
            self.active_input = ActiveInput::Terminal;
        }
    }

    pub fn commit_renaming_group(&mut self) {
        let Some(old_name) = self.renaming_group.take() else {
            return;
        };
        let new_name = self.rename_group_input.trim().to_string();
        self.rename_group_input.clear();
        self.active_input = ActiveInput::Terminal;
        if new_name.is_empty() || new_name == old_name {
            return;
        }
        let now_ms = now_unix_ms();
        for h in &mut self.hosts {
            if h.group == old_name {
                h.group = new_name.clone();
                h.updated_at_ms = now_ms;
            }
        }
        HostProfile::save_all(&self.hosts);

        for g in &mut self.settings.custom_groups {
            if g == &old_name {
                *g = new_name.clone();
            }
        }
        if !self.settings.custom_groups.contains(&new_name) {
            self.settings.custom_groups.push(new_name.clone());
        }
        self.settings.custom_groups.dedup();
        self.settings.deleted_groups.insert(old_name.clone(), now_ms);
        self.settings.deleted_groups.remove(&new_name);
        self.settings.group_updated_at.insert(new_name.clone(), now_ms);
        self.settings.save();
        if self.expanded_groups.remove(&old_name) {
            self.expanded_groups.insert(new_name);
        }
        self.maybe_auto_push_sync();
    }

    /// 文件夹右键「删除」：弹出确认弹窗告知下属主机将移至无文件夹状态
    pub fn request_delete_group(&mut self, group_name: &str) {
        self.group_context_menu = None;
        self.modal_anim_gen += 1;
        self.deleting_group_confirm = Some(group_name.to_string());
    }

    pub fn cancel_delete_group(&mut self) {
        self.deleting_group_confirm = None;
    }

    /// 确认删除文件夹：将该文件夹下所有主机置为无文件夹（group = ""）并移除文件夹本身
    pub fn confirm_delete_group(&mut self) {
        let Some(target) = self.deleting_group_confirm.take() else {
            return;
        };
        let target_trimmed = target.trim().to_string();
        let now_ms = now_unix_ms();
        let prev_updated = self
            .settings
            .group_updated_at
            .get(&target_trimmed)
            .copied()
            .unwrap_or(0);
        let del_ts = now_ms.max(prev_updated.saturating_add(1));

        for h in &mut self.hosts {
            if h.group.trim() == target_trimmed {
                h.group.clear();
                h.updated_at_ms = del_ts.max(h.updated_at_ms.saturating_add(1));
            }
        }
        HostProfile::save_all(&self.hosts);
        self.settings
            .custom_groups
            .retain(|g| g.trim() != target_trimmed);
        self.settings.group_updated_at.remove(&target_trimmed);
        self.settings
            .deleted_groups
            .insert(target_trimmed.clone(), del_ts);
        self.settings.save();
        self.expanded_groups.remove(&target_trimmed);
        self.maybe_auto_push_sync();
    }

    /// 右键「编辑」：锁定态直接打开编辑弹窗（写入不弹解锁窗；若输入新密码/私钥则公钥加密覆盖）
    pub fn open_edit_host_modal(&mut self, host_id: &str) {
        self.host_context_menu = None;
        self.active_dropdown = None;
        // 本地终端不依赖密钥库凭据，直接进入编辑弹窗
        if self.hosts.iter().any(|h| h.id == host_id && h.is_local()) {
            self.open_edit_host_modal_unlocked(host_id);
            return;
        }
        if !vault::vault_exists() {
            self.modal_anim_gen += 1;
            self.vault_modal = Some(VaultModalDraft {
                is_setup: true,
                next_action: VaultModalNextAction::OpenEditHost(host_id.to_string()),
                password: String::new(),
                confirm_password: String::new(),
                show_password: false,
            });
            self.active_input = ActiveInput::VaultModalPassword;
            return;
        }
        let _ = self.try_auto_unlock_remembered();
        if !self.is_vault_unlocked() && !vault::vault_supports_sealed_write() {
            self.modal_anim_gen += 1;
            self.vault_modal = Some(VaultModalDraft {
                is_setup: false,
                next_action: VaultModalNextAction::OpenEditHost(host_id.to_string()),
                password: String::new(),
                confirm_password: String::new(),
                show_password: false,
            });
            self.active_input = ActiveInput::VaultModalPassword;
            return;
        }
        self.open_edit_host_modal_unlocked(host_id);
    }

    fn open_edit_host_modal_unlocked(&mut self, host_id: &str) {
        let Some(host) = self.hosts.iter().find(|h| h.id == host_id).cloned() else {
            return;
        };
        let saved_pwd = self
            .vault_data
            .as_ref()
            .and_then(|d| d.host_secrets.get(&host.id))
            .and_then(|e| e.password.clone())
            .unwrap_or_default();
        let (proxy_host, proxy_port) = match host.connection {
            ConnectionKind::Socks5 | ConnectionKind::HttpProxy => host
                .effective_proxy_host_port()
                .map(|(h, p)| (h, p.to_string()))
                .unwrap_or_else(|| {
                    (
                        String::new(),
                        if host.connection == ConnectionKind::Socks5 {
                            "1080".to_string()
                        } else {
                            "8080".to_string()
                        },
                    )
                }),
            _ => (String::new(), String::new()),
        };
        self.new_host_draft = NewHostDraft {
            name: host.name,
            group: host.group,
            connection: host.connection,
            jump_host_id: host.jump_host_id,
            proxy_host,
            proxy_port,
            hostname: host.hostname,
            port: host.port.to_string(),
            username: host.username,
            auth_method: host.auth_method,
            password: saved_pwd,
            show_password: false,
            private_key_path: host.private_key_path.unwrap_or_default(),
        };
        self.editing_host_id = Some(host.id);
        self.modal_anim_gen += 1;
        self.show_new_host_modal = true;
        self.active_input = ActiveInput::ModalName;
    }

    /// 右键「删除」：断开该主机活跃会话、从加密密钥库清除关联凭据并持久化 hosts.json
    pub fn delete_host(&mut self, host_id: &str) {
        self.host_context_menu = None;
        let tab_ids: Vec<String> = self
            .tabs
            .iter()
            .filter(|t| t.host_id == host_id)
            .map(|t| t.id.clone())
            .collect();
        for tid in tab_ids {
            self.close_tab(&tid);
        }
        if let (Some(key), Some(data)) = (self.vault_key.as_ref(), self.vault_data.as_mut()) {
            if data.host_secrets.remove(host_id).is_some() {
                let _ = vault::save_vault_with_key(key, data);
            }
        } else if vault::vault_exists() {
            vault::mark_deleted_in_locked_vault(host_id);
        }
        let now_ms = now_unix_ms();
        let prev_host_updated = self
            .hosts
            .iter()
            .find(|h| h.id == host_id)
            .map(|h| h.updated_at_ms)
            .unwrap_or(0);
        let del_ts = now_ms.max(prev_host_updated.saturating_add(1));
        self.hosts.retain(|h| h.id != host_id);
        for h in &mut self.hosts {
            if h.jump_host_id.as_deref() == Some(host_id) {
                h.jump_host_id = None;
                h.updated_at_ms = del_ts.max(h.updated_at_ms.saturating_add(1));
            }
        }
        HostProfile::save_all(&self.hosts);
        self.settings
            .deleted_hosts
            .insert(host_id.to_string(), del_ts);
        self.settings.save();
        if self.active_host_id == host_id {
            self.active_host_id.clear();
        }
        self.maybe_auto_push_sync();
    }

    pub fn set_active_input(&mut self, target: ActiveInput) {
        let changed = self.active_input != target;
        if changed {
            self.ime_marked_text = None;
        }
        let next_expanded = target == ActiveInput::SidebarSearch;
        if self.search_expanded != next_expanded {
            self.search_expanded = next_expanded;
            self.search_anim_gen += 1;
        }
        self.active_dropdown = None;
        self.active_input = target;
        if changed {
            self.move_text_cursor_to_end();
        }
        reset_caret_blink_epoch();
    }

    /// 鼠标在任意单行输入框内按下：聚焦该输入框并按像素坐标落位光标 / 双击选词 / 三击全选 / 拖拽扩选
    pub fn begin_text_input_mouse_down(
        &mut self,
        target: ActiveInput,
        event: &MouseDownEvent,
        window: &mut Window,
    ) {
        window.focus(&self.focus_handle);
        let was_same = self.active_input == target;
        if !was_same {
            self.ime_marked_text = None;
        }
        let next_expanded = target == ActiveInput::SidebarSearch;
        if self.search_expanded != next_expanded {
            self.search_expanded = next_expanded;
            self.search_anim_gen += 1;
        }
        self.active_dropdown = None;
        self.active_input = target;

        let text = self.text_for_input(target).unwrap_or_default();
        let hit_idx = hit_test_text_input(target, event.position)
            .unwrap_or_else(|| text.chars().count());
        if event.click_count >= 3 {
            self.text_cursor.select_all(&text);
            self.text_cursor.dragging = false;
        } else if event.click_count == 2 {
            self.text_cursor.select_word_at(&text, hit_idx);
            self.text_cursor.dragging = false;
        } else if was_same && event.modifiers.shift {
            self.text_cursor.extend_to(hit_idx, &text);
            self.text_cursor.dragging = true;
        } else {
            self.text_cursor.set_caret(hit_idx, &text);
            self.text_cursor.dragging = true;
        }
        reset_caret_blink_epoch();
    }

    /// 将当前活跃输入框的光标重置到文本末尾（清空选区）
    fn move_text_cursor_to_end(&mut self) {
        if let Some(text) = self.text_for_input(self.active_input) {
            self.text_cursor.move_to_end(&text);
        } else {
            self.text_cursor = TextCursorState::default();
        }
    }

    /// 读取指定输入框当前的完整文本快照（用于光标落位、选区复制与全选）
    fn text_for_input(&self, target: ActiveInput) -> Option<String> {
        match target {
            ActiveInput::Terminal => None,
            ActiveInput::SidebarSearch if self.screen == AppScreen::Workspace => {
                Some(self.search_query.clone())
            }
            ActiveInput::TerminalSearch if self.screen == AppScreen::Workspace => {
                self.terminal_search.as_ref().map(|st| st.query.clone())
            }
            ActiveInput::SidebarNewGroup
                if self.creating_group && self.screen == AppScreen::Workspace =>
            {
                Some(self.new_group_name.clone())
            }
            ActiveInput::SidebarRenameGroup
                if self.renaming_group.is_some() && self.screen == AppScreen::Workspace =>
            {
                Some(self.rename_group_input.clone())
            }
            ActiveInput::SftpNewDir
                if self.sftp_creating_dir && self.screen == AppScreen::Workspace =>
            {
                Some(self.sftp_new_dir_name.clone())
            }
            ActiveInput::ModalName if self.show_new_host_modal => {
                Some(self.new_host_draft.name.clone())
            }
            ActiveInput::ModalHost if self.show_new_host_modal => {
                Some(self.new_host_draft.hostname.clone())
            }
            ActiveInput::ModalPort if self.show_new_host_modal => {
                Some(self.new_host_draft.port.clone())
            }
            ActiveInput::ModalProxyHost if self.show_new_host_modal => {
                Some(self.new_host_draft.proxy_host.clone())
            }
            ActiveInput::ModalProxyPort if self.show_new_host_modal => {
                Some(self.new_host_draft.proxy_port.clone())
            }
            ActiveInput::ModalUser if self.show_new_host_modal => {
                Some(self.new_host_draft.username.clone())
            }
            ActiveInput::ModalPassword if self.show_new_host_modal => {
                Some(self.new_host_draft.password.clone())
            }
            ActiveInput::ModalKeyPath if self.show_new_host_modal => {
                Some(self.new_host_draft.private_key_path.clone())
            }
            ActiveInput::VaultModalPassword => {
                self.vault_modal.as_ref().map(|d| d.password.clone())
            }
            ActiveInput::VaultModalConfirm => self.vault_modal.as_ref().map(|d| {
                if d.is_setup {
                    d.confirm_password.clone()
                } else {
                    d.password.clone()
                }
            }),
            ActiveInput::VaultSetupPassword if self.screen == AppScreen::Settings => {
                Some(self.vault_settings_form.setup_password.clone())
            }
            ActiveInput::VaultSetupConfirm if self.screen == AppScreen::Settings => {
                Some(self.vault_settings_form.setup_confirm.clone())
            }
            ActiveInput::VaultUnlockPassword if self.screen == AppScreen::Settings => {
                Some(self.vault_settings_form.unlock_password.clone())
            }
            ActiveInput::VaultChangeOld if self.screen == AppScreen::Settings => {
                Some(self.vault_settings_form.change_old.clone())
            }
            ActiveInput::VaultChangeNew if self.screen == AppScreen::Settings => {
                Some(self.vault_settings_form.change_new.clone())
            }
            ActiveInput::VaultChangeConfirm if self.screen == AppScreen::Settings => {
                Some(self.vault_settings_form.change_confirm.clone())
            }
            ActiveInput::SyncGistToken if self.screen == AppScreen::Settings => {
                Some(self.settings.sync_gist_token.clone())
            }
            ActiveInput::SyncGistId if self.screen == AppScreen::Settings => {
                Some(self.settings.gist_id.clone())
            }
            ActiveInput::SyncGistFilename if self.screen == AppScreen::Settings => {
                Some(self.settings.gist_filename.clone())
            }
            ActiveInput::SyncS3Endpoint if self.screen == AppScreen::Settings => {
                Some(self.settings.s3_endpoint.clone())
            }
            ActiveInput::SyncS3Region if self.screen == AppScreen::Settings => {
                Some(self.settings.s3_region.clone())
            }
            ActiveInput::SyncS3Bucket if self.screen == AppScreen::Settings => {
                Some(self.settings.s3_bucket.clone())
            }
            ActiveInput::SyncS3Key if self.screen == AppScreen::Settings => {
                Some(self.settings.s3_object_key.clone())
            }
            ActiveInput::SyncS3AccessKey if self.screen == AppScreen::Settings => {
                Some(self.settings.s3_access_key_id.clone())
            }
            ActiveInput::SyncS3SecretKey if self.screen == AppScreen::Settings => {
                Some(self.settings.sync_s3_secret.clone())
            }
            ActiveInput::SyncConflictPassword => {
                self.sync_conflict_modal.as_ref().map(|s| s.password.clone())
            }
            _ => None,
        }
    }

    /// 鼠标移出搜索框：仅平滑缩小搜索框并恢复右侧添加按钮，保留输入焦点可继续打字
    pub fn collapse_sidebar_search_visual(&mut self) -> bool {
        if self.search_expanded {
            self.search_expanded = false;
            self.search_anim_gen += 1;
            true
        } else {
            false
        }
    }

    /// 点击其他区域或按 Esc/Enter：彻底失焦停止输入，同时确保搜索框收起
    pub fn blur_sidebar_search(&mut self) {
        if self.active_input == ActiveInput::SidebarSearch {
            self.ime_marked_text = None;
            self.active_input = ActiveInput::Terminal;
        }
        if self.search_expanded {
            self.search_expanded = false;
            self.search_anim_gen += 1;
        }
    }

    /// 打开原生 macOS 文件选择框导入私钥路径
    pub fn prompt_private_key_file(&mut self, cx: &mut Context<Self>) {
        let rx = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some(tr("modal.host.select_private_key_prompt").into()),
        });
        cx.spawn(async move |this, cx| {
            if let Ok(Ok(Some(paths))) = rx.await {
                if let Some(first) = paths.into_iter().next() {
                    let display = abbreviate_home_path(&first);
                    let _ = this.update(cx, |app, cx| {
                        app.new_host_draft.private_key_path = display;
                        app.new_host_draft.auth_method = AuthMethod::PrivateKey;
                        cx.notify();
                    });
                }
            }
        })
        .detach();
    }

    /// 生成不重复的默认本地终端名称（如「本地终端」「本地终端 2」）
    fn next_default_local_host_name(&self, excluding_id: Option<&str>) -> String {
        let base = tr("enum.conn.local").to_string();
        let exists = |candidate: &str| {
            self.hosts
                .iter()
                .any(|h| excluding_id != Some(h.id.as_str()) && h.name == candidate)
        };
        if !exists(&base) {
            return base;
        }
        for idx in 2..1000 {
            let candidate = format!("{base} {idx}");
            if !exists(&candidate) {
                return candidate;
            }
        }
        base
    }

    /// 保存新建或编辑后的主机：校验必填项、持久化 hosts.json、更新加密密钥库，新建时立刻连接
    pub fn save_and_connect_draft(&mut self) {
        let is_local = self.new_host_draft.connection == ConnectionKind::Local;
        let (hostname, port, jump_host_id, proxy_host_opt, proxy_port_opt, name, username, key_opt) =
            if is_local {
                let name = if self.new_host_draft.name.trim().is_empty() {
                    self.next_default_local_host_name(self.editing_host_id.as_deref())
                } else {
                    self.new_host_draft.name.trim().to_string()
                };
                let local_user = std::env::var("USER")
                    .ok()
                    .filter(|s| !s.trim().is_empty())
                    .unwrap_or_else(|| self.settings.default_username.clone());
                (
                    String::new(),
                    self.settings.default_port,
                    None,
                    None,
                    None,
                    name,
                    local_user,
                    None,
                )
            } else {
                let hostname = self.new_host_draft.hostname.trim().to_string();
                if hostname.is_empty() {
                    self.toast_error(tr("toast.host_addr_required"));
                    self.active_input = ActiveInput::ModalHost;
                    return;
                }
                let port = match self.new_host_draft.port.trim() {
                    "" => self.settings.default_port,
                    raw => match raw.parse::<u16>() {
                        Ok(p) if p > 0 => p,
                        _ => {
                            self.toast_error(tr("toast.host_port_invalid"));
                            self.active_input = ActiveInput::ModalPort;
                            return;
                        }
                    },
                };
                let (jump_host_id, proxy_host_opt, proxy_port_opt) =
                    match self.new_host_draft.connection {
                        ConnectionKind::Direct | ConnectionKind::Local => (None, None, None),
                        ConnectionKind::JumpHost => {
                            let Some(jid) = self
                                .new_host_draft
                                .jump_host_id
                                .as_deref()
                                .filter(|id| {
                                    self.hosts.iter().any(|h| h.id == *id && !h.is_local())
                                })
                                .map(|s| s.to_string())
                            else {
                                self.toast_error(tr("toast.host_jump_required"));
                                return;
                            };
                            if self.has_jump_cycle(self.editing_host_id.as_deref(), &jid) {
                                self.toast_error(tr("toast.host_jump_cycle"));
                                return;
                            }
                            (Some(jid), None, None)
                        }
                        ConnectionKind::Socks5 | ConnectionKind::HttpProxy => {
                            let raw_host = self.new_host_draft.proxy_host.trim();
                            if raw_host.is_empty() {
                                self.toast_error(trf(
                                    "toast.host_proxy_host_required",
                                    &[("conn", self.new_host_draft.connection.label_zh())],
                                ));
                                self.active_input = ActiveInput::ModalProxyHost;
                                return;
                            }
                            let default_p =
                                if self.new_host_draft.connection == ConnectionKind::Socks5 {
                                    1080u16
                                } else {
                                    8080u16
                                };
                            let parsed_p = match self.new_host_draft.proxy_port.trim() {
                                "" => default_p,
                                raw => match raw.parse::<u16>() {
                                    Ok(p) if p > 0 => p,
                                    _ => {
                                        self.toast_error(tr("toast.host_proxy_port_invalid"));
                                        self.active_input = ActiveInput::ModalProxyPort;
                                        return;
                                    }
                                },
                            };
                            let clean_h = raw_host
                                .trim_start_matches("http://")
                                .trim_start_matches("https://")
                                .trim_start_matches("socks5://")
                                .trim_end_matches('/')
                                .to_string();
                            (None, Some(clean_h), Some(parsed_p))
                        }
                    };

                let name = if self.new_host_draft.name.trim().is_empty() {
                    hostname.clone()
                } else {
                    self.new_host_draft.name.trim().to_string()
                };
                let username = if self.new_host_draft.username.trim().is_empty() {
                    self.settings.default_username.clone()
                } else {
                    self.new_host_draft.username.trim().to_string()
                };
                let key_path = self.new_host_draft.private_key_path.trim();
                let key_opt = (!key_path.is_empty()).then(|| key_path.to_string());
                (
                    hostname,
                    port,
                    jump_host_id,
                    proxy_host_opt,
                    proxy_port_opt,
                    name,
                    username,
                    key_opt,
                )
            };

        if let Some(edit_id) = self.editing_host_id.take() {
            if !is_local {
                if let (Some(key), Some(data)) = (self.vault_key.as_ref(), self.vault_data.as_mut())
                {
                    vault::upsert_host_secret(
                        data,
                        &edit_id,
                        &name,
                        Some(self.new_host_draft.password.clone()),
                        key_opt.as_deref(),
                    );
                    let _ = vault::save_vault_with_key(key, data);
                } else if vault::vault_supports_sealed_write()
                    && (!self.new_host_draft.password.is_empty() || key_opt.is_some())
                {
                    let pwd_opt = (!self.new_host_draft.password.is_empty())
                        .then(|| self.new_host_draft.password.clone());
                    if let Err(err) = vault::seal_host_secret_to_locked_vault(
                        &edit_id,
                        &name,
                        pwd_opt,
                        key_opt.as_deref(),
                    ) {
                        self.toast_error(err);
                    }
                }
            }
            if let Some(host) = self.hosts.iter_mut().find(|h| h.id == edit_id) {
                host.name = name.clone();
                host.group = self.new_host_draft.group.clone();
                host.connection = self.new_host_draft.connection;
                host.jump_host_id = jump_host_id;
                host.proxy_host = proxy_host_opt;
                host.proxy_port = proxy_port_opt;
                host.hostname = hostname;
                host.port = port;
                host.username = username;
                host.auth_method = self.new_host_draft.auth_method;
                host.private_key_path = key_opt;
                host.updated_at_ms = now_unix_ms();
            }
            // 同步已打开标签页标题与所有分屏窗格标题
            for tab in &mut self.tabs {
                if tab.host_id == edit_id {
                    tab.title = name.clone();
                }
                tab.root_pane.update_host_name(&edit_id, &name);
            }
            HostProfile::save_all(&self.hosts);
            if !self.new_host_draft.group.trim().is_empty() {
                self.expanded_groups.insert(self.new_host_draft.group.clone());
            }
            self.show_new_host_modal = false;
            self.active_input = ActiveInput::Terminal;
            if self.settings.vault_remember_policy == VaultRememberPolicy::Never {
                self.lock_vault_now();
            }
            self.maybe_auto_push_sync();
            return;
        }

        let id = generate_host_uuid();
        let now_ms = now_unix_ms();

        let inline_password = (!is_local && !self.new_host_draft.password.is_empty())
            .then(|| self.new_host_draft.password.clone());
        let mut inline_key_pem: Option<String> = None;

        if !is_local {
            if let (Some(key), Some(data)) = (self.vault_key.as_ref(), self.vault_data.as_mut()) {
                vault::upsert_host_secret(
                    data,
                    &id,
                    &name,
                    inline_password.clone(),
                    key_opt.as_deref(),
                );
                inline_key_pem = data
                    .host_secrets
                    .get(&id)
                    .and_then(|e| e.private_key_pem.clone());
                let _ = vault::save_vault_with_key(key, data);
            } else if vault::vault_supports_sealed_write() {
                match vault::seal_host_secret_to_locked_vault(
                    &id,
                    &name,
                    inline_password.clone(),
                    key_opt.as_deref(),
                ) {
                    Ok(pem) => {
                        inline_key_pem = pem;
                    }
                    Err(err) => {
                        self.toast_error(err);
                    }
                }
            }
        }

        let profile = HostProfile {
            id: id.clone(),
            name,
            group: self.new_host_draft.group.clone(),
            connection: self.new_host_draft.connection,
            jump_host_id,
            proxy_host: proxy_host_opt,
            proxy_port: proxy_port_opt,
            hostname,
            port,
            username,
            auth_method: self.new_host_draft.auth_method,
            private_key_path: key_opt,
            created_at_ms: now_ms,
            updated_at_ms: now_ms,
            status: HostStatus::Idle,
        };

        let group_name = profile.group.clone();
        self.hosts.push(profile);
        HostProfile::save_all(&self.hosts);
        if !group_name.trim().is_empty() {
            self.expanded_groups.insert(group_name);
        }

        self.show_new_host_modal = false;
        self.active_input = ActiveInput::Terminal;
        let needs_jump_vault = self
            .hosts
            .iter()
            .find(|h| h.id == id)
            .is_some_and(|h| h.connection == ConnectionKind::JumpHost)
            && !self.is_vault_unlocked();
        if self.settings.vault_remember_policy == VaultRememberPolicy::Never {
            self.lock_vault_now();
        }
        if needs_jump_vault {
            self.open_or_connect_host(&id, None, None);
        } else {
            self.open_or_connect_host(&id, inline_password, inline_key_pem);
        }
        self.maybe_auto_push_sync();
    }

    pub fn toggle_group_expanded(&mut self, group_name: &str) {
        if !self.expanded_groups.remove(group_name) {
            self.expanded_groups.insert(group_name.to_string());
        }
    }

    /// 打开侧边栏内联「新建分组文件夹」输入框
    pub fn start_creating_group(&mut self) {
        self.host_context_menu = None;
        self.group_context_menu = None;
        self.sftp_context_menu = None;
        self.active_dropdown = None;
        if !self.settings.show_groups {
            self.settings.show_groups = true;
            self.settings.save();
        }
        self.creating_group = true;
        self.new_group_name.clear();
        self.active_input = ActiveInput::SidebarNewGroup;
    }

    pub fn cancel_creating_group(&mut self) {
        self.creating_group = false;
        self.new_group_name.clear();
        if self.active_input == ActiveInput::SidebarNewGroup {
            self.active_input = ActiveInput::Terminal;
        }
    }

    pub fn commit_creating_group(&mut self) {
        let name = self.new_group_name.trim().to_string();
        self.creating_group = false;
        self.new_group_name.clear();
        self.active_input = ActiveInput::Terminal;
        if name.is_empty() {
            return;
        }
        if !self.settings.custom_groups.contains(&name) {
            self.settings.custom_groups.push(name.clone());
        }
        let now_ms = now_unix_ms();
        self.settings.group_updated_at.insert(name.clone(), now_ms);
        self.settings.deleted_groups.remove(&name);
        self.settings.save();
        self.expanded_groups.insert(name);
        self.maybe_auto_push_sync();
    }

    pub fn select_host(&mut self, host_id: &str) {
        self.blur_sidebar_search();
        self.active_input = ActiveInput::Terminal;
        self.open_or_connect_host(host_id, None, None);
    }

    fn open_or_connect_host(
        &mut self,
        host_id: &str,
        inline_password: Option<String>,
        inline_key_pem: Option<String>,
    ) {
        self.active_host_id = host_id.to_string();
        if let Some(existing) = self.tabs.iter().find(|t| t.host_id == host_id) {
            self.active_tab_id = existing.id.clone();
            let pane_id = existing.focused_pane_id.clone();
            let sftp_open = existing.sftp_open;
            if !self.pane_handles.contains_key(&pane_id)
                && !self.pending_vault_connects.contains_key(&pane_id)
            {
                if let Some(host) = self.hosts.iter().find(|h| h.id == host_id).cloned() {
                    self.connect_pane(&pane_id, &host, inline_password, inline_key_pem);
                }
            }
            if sftp_open && self.sftp_pane_id.as_deref() != Some(pane_id.as_str()) {
                self.reset_and_load_sftp_for_pane(&pane_id);
            }
            return;
        }

        if let Some(host) = self.hosts.iter().find(|h| h.id == host_id).cloned() {
            self.next_id_seq += 1;
            let tab_id = format!("tab-{}", self.next_id_seq);
            let new_tab =
                SessionTab::new_for_host(tab_id.clone(), host.id.clone(), host.name.clone());
            let pane_id = new_tab.focused_pane_id.clone();
            self.pane_emulators.insert(
                pane_id.clone(),
                TerminalEmulator::new_with_history(100, 32, self.settings.scrollback_lines),
            );
            self.tabs.push(new_tab);
            self.active_tab_id = tab_id;
            self.connect_pane(&pane_id, &host, inline_password, inline_key_pem);
        }
    }

    /// 连接前校验密钥库策略：
    /// - 若策略为「不记住密码」且已存在密钥库（且非新建表单刚传入内联凭据），先在终端内要求输入密钥库主密码（30 秒超时）；
    /// - 若策略为记住 1~30 天且在有效期内，直接从已解锁密钥库取关联密码与内置私钥发起 SSH 连接。
    fn connect_pane(
        &mut self,
        pane_id: &str,
        host: &HostProfile,
        inline_password: Option<String>,
        inline_key_pem: Option<String>,
    ) {
        self.connect_pane_inner(pane_id, host, inline_password, inline_key_pem, false);
    }

    fn connect_pane_inner(
        &mut self,
        pane_id: &str,
        host: &HostProfile,
        inline_password: Option<String>,
        inline_key_pem: Option<String>,
        preserve_retry_count: bool,
    ) {
        self.pending_vault_connects.remove(pane_id);
        self.pending_auth_prompts.remove(pane_id);
        self.pending_host_key_prompts.remove(pane_id);
        self.pending_reconnects.remove(pane_id);
        self.pending_pty_resizes.remove(pane_id);
        if !preserve_retry_count {
            self.pane_reconnect_attempts.remove(pane_id);
        }
        if let Some(old) = self.pane_handles.remove(pane_id) {
            old.disconnect();
        }
        let scrollback = self.settings.scrollback_lines;
        let (cols, rows) = {
            let emu = self
                .pane_emulators
                .entry(pane_id.to_string())
                .or_insert_with(|| TerminalEmulator::new_with_history(100, 32, scrollback));
            (emu.cols(), emu.rows())
        };

        // 原生本地终端：跳过密钥库解锁、SSH 握手与指纹校验，直接拉起本机 PTY Shell
        if host.is_local() {
            let handle = spawn_local_pty_session(
                pane_id.to_string(),
                host.id.clone(),
                cols,
                rows,
                self.ssh_event_tx.clone(),
            );
            self.pane_handles.insert(pane_id.to_string(), handle);
            return;
        }

        if inline_password.is_some() || inline_key_pem.is_some() {
            let vault_snapshot = self.vault_data.clone();
            self.spawn_real_ssh_for_pane(
                pane_id,
                host,
                inline_password,
                inline_key_pem,
                vault_snapshot.as_ref(),
            );
            return;
        }

        if self.settings.vault_remember_policy != VaultRememberPolicy::Never
            && self.try_auto_unlock_remembered()
        {
            let vault_snapshot = self.vault_data.clone();
            let (pwd, key_pem) = vault_snapshot
                .as_ref()
                .and_then(|d| d.host_secrets.get(&host.id))
                .map(|e| (e.password.clone(), e.private_key_pem.clone()))
                .unwrap_or((None, None));
            self.spawn_real_ssh_for_pane(pane_id, host, pwd, key_pem, vault_snapshot.as_ref());
            return;
        }

        if vault::vault_exists() {
            if let Some(emu) = self.pane_emulators.get_mut(pane_id) {
                emu.feed_bytes(
                    trf(
                        "workspace.vault_prompt",
                        &[
                            ("user", &host.username),
                            ("host", &host.hostname),
                            ("port", &host.port.to_string()),
                        ],
                    )
                    .as_bytes(),
                );
            }
            self.pending_vault_connects.insert(
                pane_id.to_string(),
                PendingVaultConnect {
                    host: host.clone(),
                    input_buf: String::new(),
                    started_at: Instant::now(),
                    timeout_secs: 30,
                },
            );
            return;
        }

        self.spawn_real_ssh_for_pane(pane_id, host, None, None, None);
    }

    /// 递归解析跳板主机链（最外层跳板机在前，自动防环并从密钥库提取各级跳板机凭据）
    fn resolve_jump_hops(
        &self,
        host: &HostProfile,
        vault_data: Option<&VaultData>,
    ) -> Vec<ResolvedJumpHop> {
        if host.connection != ConnectionKind::JumpHost {
            return Vec::new();
        }
        let mut chain_rev = Vec::new();
        let mut visited: HashSet<String> = HashSet::new();
        visited.insert(host.id.clone());

        let mut next_id = host.jump_host_id.clone();
        while let Some(jid) = next_id {
            if !visited.insert(jid.clone()) {
                break;
            }
            let Some(hop_host) = self.hosts.iter().find(|h| h.id == jid).cloned() else {
                break;
            };
            let (pwd, key_pem) = vault_data
                .and_then(|d| d.host_secrets.get(&hop_host.id))
                .map(|e| (e.password.clone(), e.private_key_pem.clone()))
                .unwrap_or((None, None));
            next_id = if hop_host.connection == ConnectionKind::JumpHost {
                hop_host.jump_host_id.clone()
            } else {
                None
            };
            chain_rev.push(ResolvedJumpHop {
                host: hop_host,
                password: pwd,
                key_pem,
            });
        }
        chain_rev.reverse();
        chain_rev
    }

    /// 实际启动后台异步 SSHv2 连接
    fn spawn_real_ssh_for_pane(
        &mut self,
        pane_id: &str,
        host: &HostProfile,
        vault_password: Option<String>,
        vault_key_pem: Option<String>,
        vault_data: Option<&VaultData>,
    ) {
        let jump_hops = self.resolve_jump_hops(host, vault_data);
        let scrollback = self.settings.scrollback_lines;
        let emu = self
            .pane_emulators
            .entry(pane_id.to_string())
            .or_insert_with(|| TerminalEmulator::new_with_history(100, 32, scrollback));
        let cols = emu.cols();
        let rows = emu.rows();
        let handle = spawn_ssh_session(
            pane_id.to_string(),
            host.clone(),
            vault_password,
            vault_key_pem,
            jump_hops,
            self.settings.extra_key_paths.clone(),
            cols,
            rows,
            self.settings.keepalive_secs,
            self.settings.connect_timeout_secs,
            self.settings.auto_accept_host_key,
            self.ssh_event_tx.clone(),
        );
        self.pane_handles.insert(pane_id.to_string(), handle);
    }

    /// 判断指定窗格关联的主机是否为原生本地终端
    fn pane_is_local(&self, pane_id: &str) -> bool {
        let Some(host_id) = self
            .tabs
            .iter()
            .find_map(|t| t.root_pane.find_pane(pane_id))
            .map(|p| p.host_id.as_str())
        else {
            return false;
        };
        self.hosts
            .iter()
            .find(|h| h.id == host_id)
            .is_some_and(HostProfile::is_local)
    }

    /// 判断当前活跃标签页的聚焦窗格是否为原生本地终端
    fn active_focused_pane_is_local(&self) -> bool {
        self.active_focused_pane_id()
            .is_some_and(|pid| self.pane_is_local(&pid))
    }

    pub fn select_tab(&mut self, tab_id: &str) {
        if self.active_tab_id != tab_id {
            self.tab_anim_gen += 1;
        }
        self.active_tab_id = tab_id.to_string();
        let focused_pane_id = self
            .tabs
            .iter()
            .find(|t| t.id == tab_id)
            .map(|t| t.focused_pane_id.clone());
        let focused_is_local = focused_pane_id
            .as_deref()
            .is_some_and(|pid| self.pane_is_local(pid));
        let mut sftp_pane_to_load = None;
        if let Some(tab) = self.tabs.iter_mut().find(|t| t.id == tab_id) {
            tab.has_bell = false;
            self.active_host_id = tab.host_id.clone();
            if focused_is_local {
                tab.sftp_open = false;
            } else if tab.sftp_open
                && self.sftp_pane_id.as_deref() != Some(tab.focused_pane_id.as_str())
            {
                sftp_pane_to_load = Some(tab.focused_pane_id.clone());
            }
        }
        if let Some(pane_id) = sftp_pane_to_load {
            self.reset_and_load_sftp_for_pane(&pane_id);
        }
    }

    pub fn close_tab(&mut self, tab_id: &str) {
        let mut closed_host_id = None;
        if let Some(tab) = self.tabs.iter().find(|t| t.id == tab_id) {
            closed_host_id = Some(tab.host_id.clone());
            let mut ids = Vec::new();
            tab.root_pane.collect_pane_ids(&mut ids);
            for pid in ids {
                self.pending_vault_connects.remove(&pid);
                self.pending_auth_prompts.remove(&pid);
                self.pending_host_key_prompts.remove(&pid);
                self.pending_reconnects.remove(&pid);
                self.pane_reconnect_attempts.remove(&pid);
                self.pending_pty_resizes.remove(&pid);
                if let Some(h) = self.pane_handles.remove(&pid) {
                    h.disconnect();
                }
                self.pane_emulators.remove(&pid);
                self.pane_scroll_accum.remove(&pid);
            }
        }
        self.tabs.retain(|t| t.id != tab_id);
        if let Some(hid) = closed_host_id {
            let still_connected = self.tabs.iter().any(|t| t.host_id == hid && t.connected);
            if !still_connected {
                if let Some(host) = self.hosts.iter_mut().find(|h| h.id == hid) {
                    host.status = HostStatus::Idle;
                }
            }
        }
        if self.active_tab_id == tab_id {
            if let Some(first) = self.tabs.first() {
                self.active_tab_id = first.id.clone();
                self.active_host_id = first.host_id.clone();
                if first.sftp_open {
                    let pid = first.focused_pane_id.clone();
                    self.reset_and_load_sftp_for_pane(&pid);
                }
            } else {
                // 全部标签关闭后回到 08 未选择主机初始空状态
                self.active_tab_id.clear();
                self.active_host_id.clear();
                self.sftp = SftpPanelState::default();
                self.sftp_pane_id = None;
            }
        }
    }

    pub fn new_tab(&mut self) {
        let target_host = self
            .hosts
            .iter()
            .find(|h| h.id == self.active_host_id)
            .or_else(|| self.hosts.first())
            .cloned();
        let Some(host) = target_host else {
            self.open_new_host_modal();
            return;
        };
        self.active_host_id = host.id.clone();
        self.next_id_seq += 1;
        let tab_id = format!("tab-{}", self.next_id_seq);
        let tab = SessionTab::new_for_host(tab_id.clone(), host.id.clone(), host.name.clone());
        let pane_id = tab.focused_pane_id.clone();
        self.pane_emulators.insert(
            pane_id.clone(),
            TerminalEmulator::new_with_history(100, 32, self.settings.scrollback_lines),
        );
        self.tabs.push(tab);
        self.active_tab_id = tab_id;
        self.connect_pane(&pane_id, &host, None, None);
    }

    /// 复制指定标签对应的 SSH 主机会话，并在其右侧打开全新标签页
    pub fn duplicate_tab(&mut self, tab_id: &str) {
        let Some((pos, host_id)) = self
            .tabs
            .iter()
            .enumerate()
            .find(|(_, t)| t.id == tab_id)
            .map(|(i, t)| (i, t.host_id.clone()))
        else {
            return;
        };
        let Some(host) = self.hosts.iter().find(|h| h.id == host_id).cloned() else {
            return;
        };
        self.active_host_id = host.id.clone();
        self.next_id_seq += 1;
        let new_tab_id = format!("tab-{}", self.next_id_seq);
        let tab = SessionTab::new_for_host(new_tab_id.clone(), host.id.clone(), host.name.clone());
        let pane_id = tab.focused_pane_id.clone();
        self.pane_emulators.insert(
            pane_id.clone(),
            TerminalEmulator::new_with_history(100, 32, self.settings.scrollback_lines),
        );
        self.tabs.insert((pos + 1).min(self.tabs.len()), tab);
        self.active_tab_id = new_tab_id;
        self.connect_pane(&pane_id, &host, None, None);
    }

    pub fn toggle_split_view(&mut self) {
        let Some(tab_idx) = self.tabs.iter().position(|t| t.id == self.active_tab_id) else {
            return;
        };
        if self.tabs[tab_idx].root_pane.pane_count() > 1 {
            self.collapse_to_focused_pane(tab_idx);
        } else {
            self.split_focused_pane(SplitAxis::Horizontal);
        }
    }

    pub fn split_focused_pane(&mut self, axis: SplitAxis) {
        let Some(tab_idx) = self.tabs.iter().position(|t| t.id == self.active_tab_id) else {
            return;
        };
        let focused_id = self.tabs[tab_idx].focused_pane_id.clone();
        let host_id = self.tabs[tab_idx].host_id.clone();
        let Some(host) = self.hosts.iter().find(|h| h.id == host_id).cloned() else {
            return;
        };

        self.next_id_seq += 1;
        let new_pane_id = format!("{}-pane-{}", self.tabs[tab_idx].id, self.next_id_seq);
        let new_pane = TerminalPaneState {
            pane_id: new_pane_id.clone(),
            host_id: host.id.clone(),
            host_name: host.name.clone(),
            current_dir: "~".into(),
            connected: false,
        };

        if self.tabs[tab_idx]
            .root_pane
            .split_pane(&focused_id, axis, new_pane)
        {
            self.tabs[tab_idx].split_active = true;
            self.tabs[tab_idx].focused_pane_id = new_pane_id.clone();

            let mut emu =
                TerminalEmulator::new_with_history(64, 24, self.settings.scrollback_lines);
            emu.write_notice(&trf("workspace.pane_split_notice", &[("host", &host.name)]));
            self.pane_emulators.insert(new_pane_id.clone(), emu);
            self.connect_pane(&new_pane_id, &host, None, None);
            self.sync_active_tab_pty_sizes();
        }
    }

    pub fn close_pane(&mut self, pane_id: &str) {
        let Some(tab_idx) = self.tabs.iter().position(|t| t.id == self.active_tab_id) else {
            return;
        };
        if self.tabs[tab_idx].root_pane.pane_count() <= 1 {
            return;
        }
        if self.tabs[tab_idx].root_pane.close_pane(pane_id) {
            self.pending_vault_connects.remove(pane_id);
            self.pending_auth_prompts.remove(pane_id);
            self.pending_host_key_prompts.remove(pane_id);
            self.pending_reconnects.remove(pane_id);
            self.pane_reconnect_attempts.remove(pane_id);
            self.pending_pty_resizes.remove(pane_id);
            if let Some(h) = self.pane_handles.remove(pane_id) {
                h.disconnect();
            }
            self.pane_emulators.remove(pane_id);
            self.pane_scroll_accum.remove(pane_id);
            let mut remaining = Vec::new();
            self.tabs[tab_idx].root_pane.collect_pane_ids(&mut remaining);
            if self.tabs[tab_idx].focused_pane_id == pane_id {
                if let Some(first_id) = remaining.first() {
                    self.tabs[tab_idx].focused_pane_id = first_id.clone();
                }
            }
            self.tabs[tab_idx].split_active = remaining.len() > 1;
            self.sync_active_tab_pty_sizes();
        }
    }

    fn collapse_to_focused_pane(&mut self, tab_idx: usize) {
        let focused_id = self.tabs[tab_idx].focused_pane_id.clone();
        let mut all_ids = Vec::new();
        self.tabs[tab_idx].root_pane.collect_pane_ids(&mut all_ids);
        let kept = self.tabs[tab_idx]
            .root_pane
            .find_pane(&focused_id)
            .cloned();
        for pid in all_ids {
            if pid != focused_id {
                if let Some(h) = self.pane_handles.remove(&pid) {
                    h.disconnect();
                }
                self.pane_emulators.remove(&pid);
            }
        }
        if let Some(pane) = kept {
            self.tabs[tab_idx].root_pane = crate::state::session::SplitNode::Leaf(pane);
            self.tabs[tab_idx].split_active = false;
        }
        self.sync_active_tab_pty_sizes();
    }

    pub fn focus_pane(&mut self, pane_id: &str) {
        self.blur_sidebar_search();
        self.active_input = ActiveInput::Terminal;
        let prev_id = self.active_focused_pane_id();
        if prev_id.as_deref() == Some(pane_id) {
            return;
        }
        if let Some(old_pid) = prev_id {
            if self
                .pane_emulators
                .get(&old_pid)
                .is_some_and(|e| e.mode().contains(TermMode::FOCUS_IN_OUT))
            {
                if let Some(handle) = self.pane_handles.get(&old_pid) {
                    handle.send_bytes(b"\x1b[O".to_vec());
                }
            }
        }
        let switched_to_local = self.pane_is_local(pane_id);
        let mut closed_sftp = false;
        if let Some(tab) = self.tabs.iter_mut().find(|t| t.id == self.active_tab_id) {
            tab.focused_pane_id = pane_id.to_string();
            if switched_to_local && tab.sftp_open {
                tab.sftp_open = false;
                self.sftp_creating_dir = false;
                self.sftp_context_menu = None;
                closed_sftp = true;
            }
        }
        if closed_sftp {
            self.sync_active_tab_pty_sizes();
        }
        if self
            .pane_emulators
            .get(pane_id)
            .is_some_and(|e| e.mode().contains(TermMode::FOCUS_IN_OUT))
        {
            if let Some(handle) = self.pane_handles.get(pane_id) {
                handle.send_bytes(b"\x1b[I".to_vec());
            }
        }
    }

    fn point_to_terminal_cell(
        &self,
        pane_id: &str,
        pos: Point<Pixels>,
    ) -> Option<(usize, usize, Side)> {
        let m = self.pane_metrics.borrow().get(pane_id).copied()?;
        let rel_x = (f32::from(pos.x) - f32::from(m.bounds.origin.x)).max(0.0);
        let rel_y = (f32::from(pos.y) - f32::from(m.bounds.origin.y)).max(0.0);
        let col_f = rel_x / m.cell_width.max(4.0);
        let col = (col_f.floor() as usize).min(m.cols.saturating_sub(1));
        let frac = col_f - col_f.floor();
        let side = if frac >= 0.5 { Side::Right } else { Side::Left };
        let row = ((rel_y / m.line_height.max(10.0)).floor() as usize)
            .min(m.rows.saturating_sub(1));
        Some((col, row, side))
    }

    fn send_sgr_mouse_event(
        &self,
        pane_id: &str,
        mode: TermMode,
        btn_code: u8,
        col: usize,
        row: usize,
        pressed: bool,
    ) {
        let Some(handle) = self.pane_handles.get(pane_id) else {
            return;
        };
        let x1 = col + 1;
        let y1 = row + 1;
        if mode.contains(TermMode::SGR_MOUSE) {
            let suffix = if pressed { 'M' } else { 'm' };
            let seq = format!("\x1b[<{btn_code};{x1};{y1}{suffix}");
            handle.send_bytes(seq.into_bytes());
        } else {
            let cx = (x1.min(223) as u8).saturating_add(32);
            let cy = (y1.min(223) as u8).saturating_add(32);
            let cb = if pressed { btn_code } else { 3 }.saturating_add(32);
            handle.send_bytes(vec![0x1b, b'[', b'M', cb, cx, cy]);
        }
    }

    pub fn scroll_terminal_pane(&mut self, pane_id: &str, event: &ScrollWheelEvent) -> bool {
        let lines = match event.delta {
            ScrollDelta::Lines(pt) => {
                let acc = self
                    .pane_scroll_accum
                    .entry(pane_id.to_string())
                    .or_insert(0.0);
                *acc += pt.y * 1.5;
                let steps = acc.trunc() as i32;
                *acc -= steps as f32;
                steps
            }
            ScrollDelta::Pixels(pt) => {
                let py: f32 = pt.y.into();
                let line_px = (self.settings.terminal_font_size * 1.15).max(12.0);
                let acc = self
                    .pane_scroll_accum
                    .entry(pane_id.to_string())
                    .or_insert(0.0);
                *acc += py;
                let steps = (*acc / line_px).trunc() as i32;
                *acc -= (steps as f32) * line_px;
                steps
            }
        };
        if lines == 0 {
            return false;
        }

        let mode = self
            .pane_emulators
            .get(pane_id)
            .map(|e| e.mode())
            .unwrap_or_else(TermMode::empty);

        // 1. 远端开启鼠标上报且未按 Shift：将滚轮按 SGR 鼠标协议（64 上 / 65 下）转发给 tmux/nvim/htop
        if mode.intersects(TermMode::MOUSE_MODE) && !event.modifiers.shift {
            if let Some((col, row, _)) = self.point_to_terminal_cell(pane_id, event.position) {
                let btn = if lines > 0 { 64 } else { 65 };
                let count = lines.unsigned_abs().min(6);
                for _ in 0..count {
                    self.send_sgr_mouse_event(pane_id, mode, btn, col, row, true);
                }
            }
            return false;
        }

        // 2. 备用屏（less/man/git log）且未开鼠标上报：滚轮自动转为上下方向键
        if mode.contains(TermMode::ALT_SCREEN) {
            if let Some(handle) = self.pane_handles.get(pane_id) {
                let seq: &[u8] = match (lines > 0, mode.contains(TermMode::APP_CURSOR)) {
                    (true, true) => b"\x1bOA",
                    (true, false) => b"\x1b[A",
                    (false, true) => b"\x1bOB",
                    (false, false) => b"\x1b[B",
                };
                let count = lines.unsigned_abs().min(8);
                let mut payload = Vec::with_capacity(seq.len() * count as usize);
                for _ in 0..count {
                    payload.extend_from_slice(seq);
                }
                handle.send_bytes(payload);
            }
            return false;
        }

        // 3. 普通主屏：滚动历史回滚缓冲（上翻后自然锁定视口 Scroll Pinning，不被后台输出拉回）
        if let Some(emu) = self.pane_emulators.get_mut(pane_id) {
            return emu.scroll_display(lines);
        }
        false
    }

    /// 一键滚回指定窗格最新输出底部（由右下角「↓ 回到底部」胶囊触发）
    pub fn scroll_pane_to_bottom(&mut self, pane_id: &str) {
        if let Some(emu) = self.pane_emulators.get_mut(pane_id) {
            emu.scroll_to_bottom();
        }
    }

    /// 拖放 Finder 本地文件到终端窗格：
    /// - 已连接且全部为普通文件（未按 Option）：直接经 SFTP 上传到该窗格当前目录
    /// - 按住 Option、拖入文件夹或未连接时：自动做 POSIX 单引号转义并粘贴路径到终端
    pub fn handle_terminal_external_paths_drop(
        &mut self,
        pane_id: &str,
        external: &gpui::ExternalPaths,
        window: &mut Window,
        _cx: &mut Context<Self>,
    ) {
        let paths = external.paths();
        if paths.is_empty() {
            return;
        }
        window.focus(&self.focus_handle);
        self.focus_pane(pane_id);

        let all_files = paths.iter().all(|p| p.is_file());
        let force_paste_paths = window.modifiers().alt || self.pane_is_local(pane_id);

        if all_files && !force_paste_paths {
            if let Some(handle) = self.pane_handles.get(pane_id).cloned() {
                let remote_dir = self
                    .tabs
                    .iter()
                    .find_map(|t| t.root_pane.find_pane(pane_id))
                    .map(|p| p.current_dir.clone())
                    .filter(|d| !d.trim().is_empty())
                    .unwrap_or_else(|| "~".to_string());
                let count = paths.len();
                for local_path in paths {
                    let fname = local_path
                        .file_name()
                        .and_then(|n| n.to_str())
                        .unwrap_or("file")
                        .to_string();
                    let tid = handle.sftp_upload(local_path.clone(), remote_dir.clone());
                    self.sftp.upsert_transfer(TransferTask {
                        id: tid,
                        direction: TransferDirection::Upload,
                        file_name: fname,
                        transferred_mb: 0.0,
                        total_mb: 0.0,
                        progress: 0.0,
                        done: false,
                    });
                }
                self.toast_info(trf(
                    "toast.drop_upload_started",
                    &[("count", &count.to_string()), ("dir", &remote_dir)],
                ));
                return;
            }
        }

        // 回退：POSIX 安全转义后将本地路径粘贴到终端命令行
        let escaped: Vec<String> = paths
            .iter()
            .map(|p| shell_escape_posix_path(&p.to_string_lossy()))
            .collect();
        let joined = format!("{} ", escaped.join(" "));
        if let Some(emu) = self.pane_emulators.get_mut(pane_id) {
            emu.scroll_to_bottom();
            emu.clear_selection();
        }
        if let Some(handle) = self.pane_handles.get(pane_id) {
            handle.send_bytes(joined.into_bytes());
            self.toast_info(trf(
                "toast.drop_paths_pasted",
                &[("count", &paths.len().to_string())],
            ));
        }
    }

    pub fn handle_terminal_mouse_down(
        &mut self,
        pane_id: &str,
        button: MouseButton,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        window.focus(&self.focus_handle);
        self.focus_pane(pane_id);
        self.terminal_context_menu = None;
        self.active_dropdown = None;

        let mode = self
            .pane_emulators
            .get(pane_id)
            .map(|e| e.mode())
            .unwrap_or_else(TermMode::empty);
        let shift = event.modifiers.shift;
        let cmd = event.modifiers.platform;
        let alt = event.modifiers.alt;

        // 远端开启鼠标上报且未按 Shift：完整转发左/中/右键给 tmux/nvim/htop，并抑制右键菜单（规避 Tabby #11892）
        if mode.intersects(TermMode::MOUSE_MODE) && !shift {
            if let Some((col, row, _)) = self.point_to_terminal_cell(pane_id, event.position) {
                let mut code: u8 = match button {
                    MouseButton::Left => 0,
                    MouseButton::Middle => 1,
                    MouseButton::Right => 2,
                    _ => return,
                };
                if alt {
                    code |= 8;
                }
                if event.modifiers.control {
                    code |= 16;
                }
                self.terminal_sgr_button = Some((pane_id.to_string(), button));
                self.send_sgr_mouse_event(pane_id, mode, code, col, row, true);
            }
            return;
        }

        match button {
            MouseButton::Right => {
                self.open_terminal_context_menu(pane_id, event.position);
            }
            MouseButton::Middle => {
                self.paste_to_terminal(pane_id, cx);
            }
            MouseButton::Left => {
                let Some((col, row, side)) = self.point_to_terminal_cell(pane_id, event.position)
                else {
                    return;
                };
                // ⌘+点击打开 OSC 8 / URL 链接
                if cmd {
                    let is_dark = self.current_theme(window).1;
                    if let Some(uri) = self
                        .pane_emulators
                        .get(pane_id)
                        .and_then(|e| e.hyperlink_at(col, row, is_dark))
                    {
                        self.open_external_url(&uri);
                        return;
                    }
                }
                // 清除旧选区（确保单击单点绝不产生长条）
                if let Some(emu) = self.pane_emulators.get_mut(pane_id) {
                    emu.clear_selection();
                }

                let ty = if event.click_count >= 3 {
                    SelectionType::Lines
                } else if event.click_count == 2 {
                    SelectionType::Semantic
                } else if alt {
                    SelectionType::Block
                } else {
                    SelectionType::Simple
                };

                // 双击选词 / 三击选整行立即生效；单击仅记录锚点，拖动后才进入选区
                let immediate = matches!(ty, SelectionType::Semantic | SelectionType::Lines);
                if immediate {
                    if let Some(emu) = self.pane_emulators.get_mut(pane_id) {
                        emu.start_selection(col, row, ty, side);
                    }
                }
                self.terminal_drag_anchor = Some(MouseDragAnchor {
                    pane_id: pane_id.to_string(),
                    col,
                    row,
                    side,
                    ty,
                    started_selection: immediate,
                });
            }
            _ => {}
        }
    }

    pub fn handle_terminal_mouse_move(
        &mut self,
        pane_id: &str,
        event: &MouseMoveEvent,
    ) -> bool {
        let mode = self
            .pane_emulators
            .get(pane_id)
            .map(|e| e.mode())
            .unwrap_or_else(TermMode::empty);

        // 1. SGR 鼠标拖动/悬停事件上报
        if mode.intersects(TermMode::MOUSE_MODE) && !event.modifiers.shift {
            let report_motion = mode.contains(TermMode::MOUSE_MOTION);
            let report_drag = mode.contains(TermMode::MOUSE_DRAG) && event.dragging();
            if report_motion || report_drag {
                if let Some((col, row, _)) = self.point_to_terminal_cell(pane_id, event.position) {
                    let base = match self.terminal_sgr_button.as_ref() {
                        Some((p, MouseButton::Left)) if p == pane_id => 32,
                        Some((p, MouseButton::Middle)) if p == pane_id => 33,
                        Some((p, MouseButton::Right)) if p == pane_id => 34,
                        _ => 35,
                    };
                    self.send_sgr_mouse_event(pane_id, mode, base, col, row, true);
                }
            }
            return false;
        }

        // 2. ⌘+悬停识别超链接区间并显示手型光标与下划线
        if event.modifiers.platform && !event.dragging() {
            let next_hover = self
                .point_to_terminal_cell(pane_id, event.position)
                .and_then(|(col, row, _)| {
                    let emu = self.pane_emulators.get(pane_id)?;
                    let grid = emu.render_grid(true);
                    let r = grid.grid_rows.get(row)?;
                    r.links
                        .iter()
                        .find(|l| col >= l.col_start && col < l.col_end)
                        .map(|l| {
                            (
                                pane_id.to_string(),
                                HoveredLinkSpan {
                                    viewport_row: row,
                                    col_start: l.col_start,
                                    col_end: l.col_end,
                                },
                                l.uri.clone(),
                            )
                        })
                });
            let changed = match (&self.hovered_link, &next_hover) {
                (None, None) => false,
                (Some((p1, s1, _)), Some((p2, s2, _))) => {
                    p1 != p2
                        || s1.viewport_row != s2.viewport_row
                        || s1.col_start != s2.col_start
                        || s1.col_end != s2.col_end
                }
                _ => true,
            };
            if changed {
                self.hovered_link = next_hover;
                return true;
            }
        } else if self.hovered_link.is_some() {
            self.hovered_link = None;
            return true;
        }

        // 3. 原生鼠标拖选（字符 / 词 / 整行 / 矩形块）
        if !event.dragging() {
            self.last_drag_mouse_pos = None;
            return false;
        }
        self.last_drag_mouse_pos = Some(event.position);
        let Some((col, row, side)) = self.point_to_terminal_cell(pane_id, event.position) else {
            return false;
        };
        let Some(anchor) = self.terminal_drag_anchor.as_mut() else {
            return false;
        };
        if anchor.pane_id != pane_id {
            return false;
        }
        if !anchor.started_selection {
            if col == anchor.col && row == anchor.row && side == anchor.side {
                return false;
            }
            anchor.started_selection = true;
            let (ac, ar, asd, ty) = (anchor.col, anchor.row, anchor.side, anchor.ty);
            if let Some(emu) = self.pane_emulators.get_mut(pane_id) {
                emu.start_selection(ac, ar, ty, asd);
                emu.update_selection(col, row, side);
                return true;
            }
            return false;
        }
        if let Some(emu) = self.pane_emulators.get_mut(pane_id) {
            return emu.update_selection(col, row, side);
        }
        false
    }

    pub fn handle_terminal_mouse_up(
        &mut self,
        pane_id: &str,
        button: MouseButton,
        event: &MouseUpEvent,
        cx: &mut Context<Self>,
    ) {
        let mode = self
            .pane_emulators
            .get(pane_id)
            .map(|e| e.mode())
            .unwrap_or_else(TermMode::empty);

        if let Some((sgr_pid, sgr_btn)) = self.terminal_sgr_button.take() {
            if sgr_pid == pane_id && sgr_btn == button && mode.intersects(TermMode::MOUSE_MODE) {
                if let Some((col, row, _)) = self.point_to_terminal_cell(pane_id, event.position) {
                    let code: u8 = match button {
                        MouseButton::Left => 0,
                        MouseButton::Middle => 1,
                        MouseButton::Right => 2,
                        _ => 0,
                    };
                    self.send_sgr_mouse_event(pane_id, mode, code, col, row, false);
                }
                return;
            }
        }

        if button == MouseButton::Left {
            self.last_drag_mouse_pos = None;
            self.last_edge_scroll_at = None;
            let had_sel = self
                .terminal_drag_anchor
                .take()
                .is_some_and(|a| a.pane_id == pane_id && a.started_selection);
            if had_sel && self.settings.copy_on_select {
                self.copy_terminal_selection(cx);
            }
        }
    }

    fn open_external_url(&mut self, uri: &str) {
        let _ = std::process::Command::new("open").arg(uri).spawn();
        self.toast_info(trf("toast.link_opened", &[("url", uri)]));
    }

    pub fn has_terminal_selection(&self) -> bool {
        let Some(pid) = self.active_focused_pane_id() else {
            return false;
        };
        self.pane_emulators
            .get(&pid)
            .is_some_and(|e| e.has_selection())
    }

    pub fn copy_terminal_selection(&self, cx: &mut Context<Self>) -> bool {
        let Some(pid) = self.active_focused_pane_id() else {
            return false;
        };
        let Some(text) = self
            .pane_emulators
            .get(&pid)
            .and_then(|e| e.selection_to_string())
        else {
            return false;
        };
        cx.write_to_clipboard(ClipboardItem::new_string(text));
        true
    }

    pub fn paste_to_terminal(&mut self, pane_id: &str, cx: &mut Context<Self>) {
        let Some(text) = cx.read_from_clipboard().and_then(|c| c.text()) else {
            return;
        };
        let payload = if self.settings.trim_paste_newlines {
            text.trim_matches(|c| c == '\r' || c == '\n').to_string()
        } else {
            text
        };
        if payload.is_empty() {
            return;
        }
        let normalized = payload.replace("\r\n", "\n").replace('\r', "\n");
        let is_alt_screen = self
            .pane_emulators
            .get(pane_id)
            .is_some_and(|e| e.mode().contains(TermMode::ALT_SCREEN));

        // 非备用屏粘贴多行文本时弹出确认浮条，防止运维误执行整段脚本
        if normalized.contains('\n') && !is_alt_screen {
            let line_count = normalized.lines().count().max(2);
            let preview = normalized
                .lines()
                .find(|l| !l.trim().is_empty())
                .unwrap_or("")
                .trim()
                .chars()
                .take(48)
                .collect::<String>();
            self.pending_paste_confirm = Some(PendingPasteConfirm {
                pane_id: pane_id.to_string(),
                payload: normalized,
                line_count,
                preview,
            });
            return;
        }
        self.send_paste_payload(pane_id, &normalized);
    }

    pub fn confirm_pending_paste(&mut self) {
        if let Some(pending) = self.pending_paste_confirm.take() {
            self.send_paste_payload(&pending.pane_id, &pending.payload);
        }
    }

    pub fn cancel_pending_paste(&mut self) {
        self.pending_paste_confirm = None;
    }

    fn send_paste_payload(&mut self, pane_id: &str, text: &str) {
        let bracketed = self
            .pane_emulators
            .get(pane_id)
            .is_some_and(|e| e.mode().contains(TermMode::BRACKETED_PASTE));
        if let Some(emu) = self.pane_emulators.get_mut(pane_id) {
            emu.scroll_to_bottom();
            emu.clear_selection();
        }
        // 防御括弧粘贴逃逸注入：剥离粘贴内容中可能夹带的 \x1b[200~ / \x1b[201~ 序列
        let sanitized = text.replace("\x1b[200~", "").replace("\x1b[201~", "");
        let cr_text = sanitized.replace('\n', "\r");
        let mut bytes = Vec::with_capacity(cr_text.len() + 16);
        if bracketed {
            bytes.extend_from_slice(b"\x1b[200~");
        }
        bytes.extend_from_slice(cr_text.as_bytes());
        if bracketed {
            bytes.extend_from_slice(b"\x1b[201~");
        }
        if let Some(handle) = self.pane_handles.get(pane_id) {
            handle.send_bytes(bytes);
        }
    }

    // ────────── 终端内搜索（⌘F，原生 RegexSearch）──────────

    pub fn open_terminal_search(&mut self) {
        let Some(pane_id) = self.active_focused_pane_id() else {
            return;
        };
        // 打开终端搜索条时自动关闭所有浮层右键菜单与下拉框，避免双浮层重叠
        self.terminal_context_menu = None;
        self.host_context_menu = None;
        self.group_context_menu = None;
        self.sftp_context_menu = None;
        self.active_dropdown = None;
        let prev_query = self
            .terminal_search
            .as_ref()
            .map(|s| s.query.clone())
            .unwrap_or_default();
        self.terminal_search = Some(TerminalSearchState {
            pane_id,
            query: prev_query,
            case_sensitive: self
                .terminal_search
                .as_ref()
                .map(|s| s.case_sensitive)
                .unwrap_or(false),
            is_regex: self
                .terminal_search
                .as_ref()
                .map(|s| s.is_regex)
                .unwrap_or(false),
            matches: Arc::new(Vec::new()),
            active_index: None,
        });
        self.active_input = ActiveInput::TerminalSearch;
        self.refresh_terminal_search_matches(true);
    }

    pub fn close_terminal_search(&mut self) {
        self.terminal_search = None;
        if self.active_input == ActiveInput::TerminalSearch {
            self.active_input = ActiveInput::Terminal;
        }
    }

    pub fn toggle_terminal_search_case(&mut self) {
        if let Some(st) = self.terminal_search.as_mut() {
            st.case_sensitive = !st.case_sensitive;
        }
        self.refresh_terminal_search_matches(true);
    }

    pub fn toggle_terminal_search_regex(&mut self) {
        if let Some(st) = self.terminal_search.as_mut() {
            st.is_regex = !st.is_regex;
        }
        self.refresh_terminal_search_matches(true);
    }

    pub fn refresh_terminal_search_matches(&mut self, jump_to_match: bool) {
        let Some((pane_id, query, case_sensitive, is_regex)) = self
            .terminal_search
            .as_ref()
            .map(|s| (s.pane_id.clone(), s.query.clone(), s.case_sensitive, s.is_regex))
        else {
            return;
        };
        let found = self
            .pane_emulators
            .get(&pane_id)
            .map(|e| e.search_all(&query, case_sensitive, is_regex))
            .unwrap_or_default();
        let active_idx = if found.is_empty() {
            None
        } else {
            Some(found.len() - 1)
        };
        let target_line = active_idx.and_then(|i| found.get(i)).map(|m| m.start_line);
        if let Some(st) = self.terminal_search.as_mut() {
            st.matches = Arc::new(found);
            st.active_index = active_idx;
        }
        if jump_to_match {
            if let Some(line) = target_line {
                if let Some(emu) = self.pane_emulators.get_mut(&pane_id) {
                    emu.scroll_to_grid_line(line);
                }
            }
        }
    }

    pub fn step_terminal_search(&mut self, delta: isize) {
        let Some(st) = self.terminal_search.as_mut() else {
            return;
        };
        let len = st.matches.len();
        if len == 0 {
            return;
        }
        let cur = st.active_index.unwrap_or(0) as isize;
        let next = (cur + delta).rem_euclid(len as isize) as usize;
        st.active_index = Some(next);
        let pane_id = st.pane_id.clone();
        let target_line = st.matches[next].start_line;
        if let Some(emu) = self.pane_emulators.get_mut(&pane_id) {
            emu.scroll_to_grid_line(target_line);
        }
    }

    pub fn open_terminal_context_menu(&mut self, pane_id: &str, position: Point<Pixels>) {
        self.active_dropdown = None;
        self.host_context_menu = None;
        self.group_context_menu = None;
        self.sftp_context_menu = None;
        self.focus_pane(pane_id);
        self.ctx_menu_anim_gen += 1;
        self.terminal_context_menu = Some(TerminalContextMenu {
            pane_id: pane_id.to_string(),
            position,
        });
    }

    pub fn close_terminal_context_menu(&mut self) {
        self.terminal_context_menu = None;
    }

    fn active_focused_pane_id(&self) -> Option<String> {
        self.tabs
            .iter()
            .find(|t| t.id == self.active_tab_id)
            .map(|t| t.focused_pane_id.clone())
    }

    fn reset_and_load_sftp_for_pane(&mut self, pane_id: &str) {
        self.sftp = SftpPanelState::default();
        self.sftp_pane_id = Some(pane_id.to_string());
        let initial_dir = self
            .tabs
            .iter()
            .find_map(|t| t.root_pane.find_pane(pane_id))
            .map(|p| p.current_dir.clone())
            .filter(|d| !d.is_empty() && (d.starts_with('/') || d == "~"))
            .unwrap_or_else(|| "~".to_string());
        self.request_sftp_list_for_pane(pane_id, &initial_dir);
    }

    fn request_sftp_list_for_pane(&mut self, pane_id: &str, remote_path: &str) {
        self.sftp_pane_id = Some(pane_id.to_string());
        if let Some(handle) = self.pane_handles.get(pane_id) {
            self.sftp.loading = true;
            handle.sftp_list(remote_path.to_string());
        } else {
            self.sftp.loading = false;
            self.sftp.entries.clear();
            self.sftp.selected_name = None;
            self.toast_info(tr("toast.sftp_not_connected_list"));
        }
    }

    pub fn toggle_sftp_panel(&mut self) {
        let Some(tab_idx) = self.tabs.iter().position(|t| t.id == self.active_tab_id) else {
            return;
        };
        if self.active_focused_pane_is_local() {
            if self.tabs[tab_idx].sftp_open {
                self.tabs[tab_idx].sftp_open = false;
                self.sftp_creating_dir = false;
                self.sftp_context_menu = None;
                self.sync_active_tab_pty_sizes();
            }
            self.toast_info(tr("toast.sftp_disabled_local"));
            return;
        }
        let next_open = !self.tabs[tab_idx].sftp_open;
        self.tabs[tab_idx].sftp_open = next_open;
        self.sftp_anim_gen += 1;
        self.sftp_anim_tab_id = Some(self.tabs[tab_idx].id.clone());
        self.sftp_creating_dir = false;
        self.sftp_context_menu = None;
        if next_open {
            let pane_id = self.tabs[tab_idx].focused_pane_id.clone();
            if self.sftp_pane_id.as_deref() != Some(pane_id.as_str()) {
                self.reset_and_load_sftp_for_pane(&pane_id);
            } else {
                let path = self.sftp.current_path.clone();
                self.request_sftp_list_for_pane(&pane_id, &path);
            }
        }
        self.sync_active_tab_pty_sizes();
    }

    pub fn sftp_refresh(&mut self) {
        self.sftp_context_menu = None;
        if let Some(pane_id) = self.active_focused_pane_id() {
            let path = self.sftp.current_path.clone();
            self.request_sftp_list_for_pane(&pane_id, &path);
        }
    }

    pub fn start_sftp_create_folder(&mut self) {
        self.sftp_context_menu = None;
        let Some(pane_id) = self.active_focused_pane_id() else {
            return;
        };
        if !self.pane_handles.contains_key(&pane_id) {
            self.toast_error(tr("toast.sftp_not_connected_mkdir"));
            return;
        }
        self.sftp_creating_dir = true;
        self.sftp_new_dir_name.clear();
        self.active_input = ActiveInput::SftpNewDir;
    }

    pub fn cancel_sftp_create_folder(&mut self) {
        self.sftp_creating_dir = false;
        self.sftp_new_dir_name.clear();
        if self.active_input == ActiveInput::SftpNewDir {
            self.active_input = ActiveInput::Terminal;
        }
    }

    pub fn commit_sftp_create_folder(&mut self) {
        let folder_name = self.sftp_new_dir_name.trim().trim_matches('/').to_string();
        self.sftp_creating_dir = false;
        self.sftp_new_dir_name.clear();
        if self.active_input == ActiveInput::SftpNewDir {
            self.active_input = ActiveInput::Terminal;
        }
        if folder_name.is_empty() {
            return;
        }
        let Some(pane_id) = self.active_focused_pane_id() else {
            return;
        };
        if let Some(handle) = self.pane_handles.get(&pane_id) {
            let parent_dir = self.sftp.current_path.clone();
            self.sftp.loading = true;
            handle.sftp_create_dir(parent_dir, folder_name);
        } else {
            self.toast_error(tr("toast.sftp_not_connected_mkdir"));
        }
    }

    pub fn open_sftp_context_menu(
        &mut self,
        entry_name: &str,
        is_dir: bool,
        position: Point<Pixels>,
    ) {
        self.active_dropdown = None;
        self.host_context_menu = None;
        self.group_context_menu = None;
        self.ctx_menu_anim_gen += 1;
        self.sftp.selected_name = Some(entry_name.to_string());
        self.sftp_context_menu = Some(SftpContextMenu {
            entry_name: entry_name.to_string(),
            is_dir,
            position,
        });
    }

    pub fn close_sftp_context_menu(&mut self) {
        self.sftp_context_menu = None;
    }

    pub fn sftp_download_entry(&mut self, name: &str) {
        self.sftp_context_menu = None;
        let Some(pane_id) = self.active_focused_pane_id() else {
            return;
        };
        if let Some(handle) = self.pane_handles.get(&pane_id) {
            let clean = name.trim_end_matches('/');
            let remote_path = self.sftp.join_child(clean);
            let local_dir = dirs::download_dir()
                .or_else(dirs::home_dir)
                .unwrap_or_else(|| PathBuf::from("."));
            let local_path = local_dir.join(clean);
            let tid = handle.sftp_download(remote_path, local_path);
            self.sftp.upsert_transfer(TransferTask {
                id: tid,
                direction: TransferDirection::Download,
                file_name: clean.to_string(),
                transferred_mb: 0.0,
                total_mb: 0.0,
                progress: 0.0,
                done: false,
            });
        } else {
            self.toast_error(tr("toast.sftp_not_connected_download"));
        }
    }

    pub fn sftp_delete_entry(&mut self, name: &str, is_dir: bool) {
        self.sftp_context_menu = None;
        let Some(pane_id) = self.active_focused_pane_id() else {
            return;
        };
        if let Some(handle) = self.pane_handles.get(&pane_id) {
            let clean = name.trim_end_matches('/');
            let target = self.sftp.join_child(clean);
            self.sftp.loading = true;
            handle.sftp_delete(target, is_dir);
        } else {
            self.toast_error(tr("toast.sftp_not_connected_delete"));
        }
    }

    pub fn prompt_sftp_upload(&mut self, cx: &mut Context<Self>) {
        let Some(pane_id) = self.active_focused_pane_id() else {
            return;
        };
        if !self.pane_handles.contains_key(&pane_id) {
            self.toast_error(tr("toast.sftp_not_connected_upload"));
            cx.notify();
            return;
        }
        let remote_dir = self.sftp.current_path.clone();
        let rx = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some(tr("sftp.upload_prompt").into()),
        });
        cx.spawn(async move |this, cx| {
            if let Ok(Ok(Some(paths))) = rx.await {
                if let Some(local_path) = paths.into_iter().next() {
                    let _ = this.update(cx, |app, cx| {
                        if let Some(handle) = app.pane_handles.get(&pane_id) {
                            let fname = local_path
                                .file_name()
                                .and_then(|n| n.to_str())
                                .unwrap_or("file")
                                .to_string();
                            let tid = handle.sftp_upload(local_path, remote_dir);
                            app.sftp.upsert_transfer(TransferTask {
                                id: tid,
                                direction: TransferDirection::Upload,
                                file_name: fname,
                                transferred_mb: 0.0,
                                total_mb: 0.0,
                                progress: 0.0,
                                done: false,
                            });
                            cx.notify();
                        }
                    });
                }
            }
        })
        .detach();
    }

    pub fn sftp_select_or_enter(&mut self, name: &str, is_dir: bool, is_double_click: bool) {
        self.sftp.selected_name = Some(name.to_string());

        let Some(pane_id) = self.active_focused_pane_id() else {
            return;
        };

        if is_dir {
            let next_path = self.sftp.join_child(name);
            if self.pane_handles.contains_key(&pane_id) {
                self.request_sftp_list_for_pane(&pane_id, &next_path);
            } else {
                self.toast_info(tr("toast.sftp_not_connected_list"));
            }
        } else if is_double_click {
            if let Some(handle) = self.pane_handles.get(&pane_id) {
                let remote_path = self.sftp.join_child(name);
                let local_dir = dirs::download_dir()
                    .or_else(dirs::home_dir)
                    .unwrap_or_else(|| PathBuf::from("."));
                let local_path = local_dir.join(name);
                let tid = handle.sftp_download(remote_path, local_path);
                self.sftp.upsert_transfer(TransferTask {
                    id: tid,
                    direction: TransferDirection::Download,
                    file_name: name.to_string(),
                    transferred_mb: 0.0,
                    total_mb: 0.0,
                    progress: 0.0,
                    done: false,
                });
            }
        }
    }

    pub fn sftp_navigate_to_crumb(&mut self, idx: usize) {
        let target = self.sftp.path_for_crumb(idx);
        if let Some(pane_id) = self.active_focused_pane_id() {
            if self.pane_handles.contains_key(&pane_id) {
                self.request_sftp_list_for_pane(&pane_id, &target);
            } else {
                self.toast_info(tr("toast.sftp_not_connected_list"));
            }
        }
    }

    pub fn sftp_toggle_transfers_expanded(&mut self) {
        if self.sftp.transfers.len() > 1 {
            self.sftp.transfers_expanded = !self.sftp.transfers_expanded;
        } else {
            self.sftp.transfers_expanded = false;
        }
    }

    pub fn sftp_cancel_or_remove_transfer(&mut self, transfer_id: u64) {
        let is_running = self
            .sftp
            .transfers
            .iter()
            .any(|t| t.id == transfer_id && !t.done);
        if is_running {
            if let Some(pane_id) = self.active_focused_pane_id() {
                if let Some(handle) = self.pane_handles.get(&pane_id) {
                    handle.sftp_cancel_transfer(transfer_id);
                }
            }
        }
        self.sftp.remove_transfer(transfer_id);
    }

    pub fn sftp_clear_transfers(&mut self) {
        if let Some(pane_id) = self.active_focused_pane_id() {
            if let Some(handle) = self.pane_handles.get(&pane_id) {
                for task in &self.sftp.transfers {
                    if !task.done {
                        handle.sftp_cancel_transfer(task.id);
                    }
                }
            }
        }
        self.sftp.clear_transfers();
    }

    /// 全局键盘事件路由（遵循设置页「全局快捷键」开关与全部快捷组合）
    fn handle_key_down(
        &mut self,
        event: &KeyDownEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // 0. 设置页正在录制自定义快捷键时优先拦截所有按键（Esc 取消录制，纯修饰键忽略，有效组合立即生效）
        if let Some(target_action) = self.recording_shortcut {
            cx.stop_propagation();
            let ks = &event.keystroke;
            if ks.key == "escape" {
                self.recording_shortcut = None;
                cx.notify();
                return;
            }
            if is_modifier_only_key(&ks.key) {
                return;
            }
            let Some(canonical) = keystroke_to_canonical_shortcut(ks) else {
                self.toast_error(tr("toast.shortcut_need_modifier"));
                cx.notify();
                return;
            };
            if canonical == "cmd-q" {
                self.toast_error(tr("toast.shortcut_reserved"));
                cx.notify();
                return;
            }
            let disp = format_shortcut_display(&canonical);
            if let Some(conflict) = self
                .settings
                .shortcuts
                .find_conflict(target_action, &canonical)
            {
                self.toast_error(trf(
                    "toast.shortcut_conflict",
                    &[("key", &disp), ("action", tr(conflict.i18n_key()))],
                ));
                cx.notify();
                return;
            }
            self.settings.shortcuts.set(target_action, canonical);
            self.settings.save();
            self.recording_shortcut = None;
            self.toast_success(trf("toast.shortcut_updated", &[("key", &disp)]));
            cx.notify();
            return;
        }

        // 若 macOS 输入法正在进行中/日/韩候选组词（存在 marked text），不拦截任何按键，交由系统输入法原生处理
        if self.ime_marked_text.is_some() {
            return;
        }

        let ks = &event.keystroke;
        let cmd = ks.modifiers.platform;
        let shift = ks.modifiers.shift;
        let ctrl = ks.modifiers.control;
        let key = ks.key.as_str();

        // 删除文件夹确认弹窗快捷响应（Esc 取消 / Enter 确认删除）
        if self.deleting_group_confirm.is_some() {
            if key == "escape" {
                self.cancel_delete_group();
                cx.notify();
                return;
            }
            if key == "enter" {
                self.confirm_delete_group();
                cx.notify();
                return;
            }
            return;
        }

        // 按 Esc 先关闭悬浮下拉菜单或右键菜单
        if self.active_dropdown.is_some() && key == "escape" {
            self.close_dropdown();
            cx.notify();
            return;
        }
        if self.host_context_menu.is_some() && key == "escape" {
            self.close_host_context_menu();
            cx.notify();
            return;
        }
        if self.group_context_menu.is_some() && key == "escape" {
            self.close_group_context_menu();
            cx.notify();
            return;
        }
        if self.sftp_context_menu.is_some() && key == "escape" {
            self.close_sftp_context_menu();
            cx.notify();
            return;
        }
        if self.terminal_context_menu.is_some() && key == "escape" {
            self.close_terminal_context_menu();
            cx.notify();
            return;
        }

        // 全局同步冲突 / 首次绑定决策弹窗键盘路由
        if self.sync_conflict_modal.is_some() {
            let in_pwd_step = self
                .sync_conflict_modal
                .as_ref()
                .is_some_and(|s| s.password_action.is_some());
            if key == "escape" {
                if in_pwd_step {
                    self.back_from_sync_conflict_password();
                } else {
                    self.dismiss_sync_conflict_modal(false);
                }
                cx.notify();
                return;
            }
            if in_pwd_step {
                if key == "enter" {
                    self.submit_sync_conflict_password();
                    cx.notify();
                    return;
                }
                if let Some(st) = self.sync_conflict_modal.as_mut() {
                    let outcome =
                        apply_text_edit_key(&mut self.text_cursor, &mut st.password, ks, cx);
                    if outcome == TextKeyOutcome::Modified {
                        st.error = None;
                    }
                    if outcome != TextKeyOutcome::Unhandled {
                        cx.notify();
                    }
                }
            }
            return;
        }

        // 密钥库首次设密 / 解锁弹窗键盘路由
        if self.vault_modal.is_some() {
            match key {
                "escape" => {
                    self.close_vault_modal();
                    cx.notify();
                    return;
                }
                "enter" => {
                    self.submit_vault_modal();
                    cx.notify();
                    return;
                }
                "tab" => {
                    let is_setup = self.vault_modal.as_ref().map(|m| m.is_setup).unwrap_or(false);
                    if is_setup {
                        let next = match self.active_input {
                            ActiveInput::VaultModalPassword => ActiveInput::VaultModalConfirm,
                            _ => ActiveInput::VaultModalPassword,
                        };
                        self.set_active_input(next);
                        cx.notify();
                    }
                    return;
                }
                _ => {}
            }
            if let Some(draft) = self.vault_modal.as_mut() {
                let target = match self.active_input {
                    ActiveInput::VaultModalConfirm if draft.is_setup => {
                        Some(&mut draft.confirm_password)
                    }
                    _ => Some(&mut draft.password),
                };
                if let Some(buf) = target {
                    if apply_text_edit_key(&mut self.text_cursor, buf, ks, cx)
                        != TextKeyOutcome::Unhandled
                    {
                        cx.notify();
                    }
                }
            }
            return;
        }

        // 设置页「密钥库」与「同步」表单输入路由（支持光标定位、选区、退格、剪切/复制/粘贴）
        if self.screen == AppScreen::Settings && !self.show_new_host_modal {
            match self.active_input {
                ActiveInput::VaultSetupPassword | ActiveInput::VaultSetupConfirm => {
                    if key == "enter" {
                        self.submit_settings_vault_setup();
                        cx.notify();
                        return;
                    }
                    if key == "tab" {
                        let next = if self.active_input == ActiveInput::VaultSetupPassword {
                            ActiveInput::VaultSetupConfirm
                        } else {
                            ActiveInput::VaultSetupPassword
                        };
                        self.set_active_input(next);
                        cx.notify();
                        return;
                    }
                    if key == "escape" {
                        self.set_active_input(ActiveInput::Terminal);
                        cx.notify();
                        return;
                    }
                    let buf = if self.active_input == ActiveInput::VaultSetupPassword {
                        &mut self.vault_settings_form.setup_password
                    } else {
                        &mut self.vault_settings_form.setup_confirm
                    };
                    if apply_text_edit_key(&mut self.text_cursor, buf, ks, cx)
                        != TextKeyOutcome::Unhandled
                    {
                        cx.notify();
                    }
                    return;
                }
                ActiveInput::VaultUnlockPassword => {
                    if key == "enter" {
                        self.submit_settings_vault_unlock();
                        cx.notify();
                        return;
                    }
                    if key == "escape" {
                        self.set_active_input(ActiveInput::Terminal);
                        cx.notify();
                        return;
                    }
                    if apply_text_edit_key(
                        &mut self.text_cursor,
                        &mut self.vault_settings_form.unlock_password,
                        ks,
                        cx,
                    ) != TextKeyOutcome::Unhandled
                    {
                        cx.notify();
                    }
                    return;
                }
                ActiveInput::VaultChangeOld
                | ActiveInput::VaultChangeNew
                | ActiveInput::VaultChangeConfirm => {
                    if key == "enter" {
                        self.submit_change_vault_password();
                        cx.notify();
                        return;
                    }
                    if key == "tab" {
                        let next = match self.active_input {
                            ActiveInput::VaultChangeOld => ActiveInput::VaultChangeNew,
                            ActiveInput::VaultChangeNew => ActiveInput::VaultChangeConfirm,
                            _ => ActiveInput::VaultChangeOld,
                        };
                        self.set_active_input(next);
                        cx.notify();
                        return;
                    }
                    if key == "escape" {
                        self.set_active_input(ActiveInput::Terminal);
                        cx.notify();
                        return;
                    }
                    let buf = match self.active_input {
                        ActiveInput::VaultChangeOld => &mut self.vault_settings_form.change_old,
                        ActiveInput::VaultChangeNew => &mut self.vault_settings_form.change_new,
                        _ => &mut self.vault_settings_form.change_confirm,
                    };
                    if apply_text_edit_key(&mut self.text_cursor, buf, ks, cx)
                        != TextKeyOutcome::Unhandled
                    {
                        cx.notify();
                    }
                    return;
                }
                _ => {}
            }

            let is_sync_field = matches!(
                self.active_input,
                ActiveInput::SyncGistToken
                    | ActiveInput::SyncGistId
                    | ActiveInput::SyncGistFilename
                    | ActiveInput::SyncS3Endpoint
                    | ActiveInput::SyncS3Region
                    | ActiveInput::SyncS3Bucket
                    | ActiveInput::SyncS3Key
                    | ActiveInput::SyncS3AccessKey
                    | ActiveInput::SyncS3SecretKey
            );
            if is_sync_field {
                if key == "escape" || key == "enter" {
                    self.set_active_input(ActiveInput::Terminal);
                    cx.notify();
                    return;
                }
                if let Some(outcome) = self.edit_active_input_buf(|cursor, buf, _| {
                    let o = apply_text_edit_key(cursor, buf, ks, cx);
                    (o, o == TextKeyOutcome::Modified)
                }) {
                    if outcome != TextKeyOutcome::Unhandled {
                        cx.notify();
                    }
                }
                return;
            }
        }

        // 在设置页按 Esc：处于二级页时先返回一级，否则返回工作区
        if self.screen == AppScreen::Settings && !self.show_new_host_modal && key == "escape" {
            if self.active_settings_subpage.is_some() {
                self.pop_settings_subpage();
            } else {
                self.close_settings();
            }
            cx.notify();
            return;
        }

        // 弹窗输入路由
        if self.show_new_host_modal {
            match key {
                "escape" => {
                    self.close_new_host_modal();
                    cx.notify();
                    return;
                }
                "enter" => {
                    self.save_and_connect_draft();
                    cx.notify();
                    return;
                }
                "tab" => {
                    if self.new_host_draft.connection == ConnectionKind::Local {
                        self.set_active_input(ActiveInput::ModalName);
                        cx.notify();
                        return;
                    }
                    let has_proxy_inputs = matches!(
                        self.new_host_draft.connection,
                        ConnectionKind::Socks5 | ConnectionKind::HttpProxy
                    );
                    let has_pwd_input = matches!(
                        self.new_host_draft.auth_method,
                        AuthMethod::Auto
                            | AuthMethod::Password
                            | AuthMethod::PrivateKey
                            | AuthMethod::KeyboardInteractive
                    );
                    let has_key_input = matches!(
                        self.new_host_draft.auth_method,
                        AuthMethod::Auto | AuthMethod::PrivateKey | AuthMethod::Agent
                    );
                    let next = match self.active_input {
                        ActiveInput::ModalName => ActiveInput::ModalHost,
                        ActiveInput::ModalHost => ActiveInput::ModalPort,
                        ActiveInput::ModalPort => {
                            if has_proxy_inputs {
                                ActiveInput::ModalProxyHost
                            } else {
                                ActiveInput::ModalUser
                            }
                        }
                        ActiveInput::ModalProxyHost => ActiveInput::ModalProxyPort,
                        ActiveInput::ModalProxyPort => ActiveInput::ModalUser,
                        ActiveInput::ModalUser => {
                            if has_pwd_input {
                                ActiveInput::ModalPassword
                            } else if has_key_input {
                                ActiveInput::ModalKeyPath
                            } else {
                                ActiveInput::ModalName
                            }
                        }
                        ActiveInput::ModalPassword => {
                            if has_key_input {
                                ActiveInput::ModalKeyPath
                            } else {
                                ActiveInput::ModalName
                            }
                        }
                        _ => ActiveInput::ModalName,
                    };
                    self.set_active_input(next);
                    cx.notify();
                    return;
                }
                _ => {}
            }

            if let Some(outcome) = self.edit_active_input_buf(|cursor, buf, _| {
                let o = apply_text_edit_key(cursor, buf, ks, cx);
                (o, o == TextKeyOutcome::Modified)
            }) {
                if outcome != TextKeyOutcome::Unhandled {
                    cx.notify();
                }
            }
            return;
        }

        // SFTP 面板内联「新建远程文件夹」输入框路由
        if self.sftp_creating_dir
            && self.active_input == ActiveInput::SftpNewDir
            && self.screen == AppScreen::Workspace
        {
            if key == "escape" {
                self.cancel_sftp_create_folder();
                cx.notify();
                return;
            }
            if key == "enter" {
                self.commit_sftp_create_folder();
                cx.notify();
                return;
            }
            if apply_text_edit_key(&mut self.text_cursor, &mut self.sftp_new_dir_name, ks, cx)
                != TextKeyOutcome::Unhandled
            {
                cx.notify();
            }
            return;
        }

        // 侧边栏「新建分组文件夹」内联输入框路由
        if self.creating_group
            && self.active_input == ActiveInput::SidebarNewGroup
            && self.screen == AppScreen::Workspace
        {
            if key == "escape" {
                self.cancel_creating_group();
                cx.notify();
                return;
            }
            if key == "enter" {
                self.commit_creating_group();
                cx.notify();
                return;
            }
            if apply_text_edit_key(&mut self.text_cursor, &mut self.new_group_name, ks, cx)
                != TextKeyOutcome::Unhandled
            {
                cx.notify();
            }
            return;
        }

        // 侧边栏「重命名文件夹」内联输入框路由
        if self.renaming_group.is_some()
            && self.active_input == ActiveInput::SidebarRenameGroup
            && self.screen == AppScreen::Workspace
        {
            if key == "escape" {
                self.cancel_renaming_group();
                cx.notify();
                return;
            }
            if key == "enter" {
                self.commit_renaming_group();
                cx.notify();
                return;
            }
            if apply_text_edit_key(&mut self.text_cursor, &mut self.rename_group_input, ks, cx)
                != TextKeyOutcome::Unhandled
            {
                cx.notify();
            }
            return;
        }

        // 多行粘贴安全确认条快捷响应（Enter 确认 / Esc 取消）
        if self.pending_paste_confirm.is_some() {
            if key == "escape" {
                self.cancel_pending_paste();
                cx.stop_propagation();
                cx.notify();
                return;
            }
            if key == "enter" {
                self.confirm_pending_paste();
                cx.stop_propagation();
                cx.notify();
                return;
            }
        }

        // 终端内 ⌘F 搜索框输入路由
        if self.active_input == ActiveInput::TerminalSearch
            && self.screen == AppScreen::Workspace
            && self.terminal_search.is_some()
        {
            if key == "escape" {
                self.close_terminal_search();
                cx.stop_propagation();
                cx.notify();
                return;
            }
            if key == "enter" {
                self.step_terminal_search(if shift { -1 } else { 1 });
                cx.stop_propagation();
                cx.notify();
                return;
            }
            let outcome = if let Some(st) = self.terminal_search.as_mut() {
                apply_text_edit_key(&mut self.text_cursor, &mut st.query, ks, cx)
            } else {
                TextKeyOutcome::Unhandled
            };
            if outcome == TextKeyOutcome::Modified {
                self.refresh_terminal_search_matches(true);
            }
            if outcome != TextKeyOutcome::Unhandled {
                cx.stop_propagation();
                cx.notify();
            }
            return;
        }

        // 侧边栏搜索框输入
        if self.active_input == ActiveInput::SidebarSearch && self.screen == AppScreen::Workspace {
            if key == "escape" || key == "enter" {
                self.blur_sidebar_search();
                cx.notify();
                return;
            }
            if apply_text_edit_key(&mut self.text_cursor, &mut self.search_query, ks, cx)
                != TextKeyOutcome::Unhandled
            {
                cx.notify();
            }
            return;
        }

        // 工作区真实 SSH 终端按键发送（含连接前终端内密钥库密码交互验证）
        if self.screen == AppScreen::Workspace {
            let Some(pane_id) = self.active_focused_pane_id() else {
                return;
            };

            // 若当前窗格正在等待终端内主机公钥指纹确认（未知主机或公钥变更 MITM 告警）
            if self.pending_host_key_prompts.contains_key(&pane_id) {
                if key == "escape" || (ctrl && key == "c") {
                    if let Some(pending) = self.pending_host_key_prompts.remove(&pane_id) {
                        if let Some(emu) = self.pane_emulators.get_mut(&pane_id) {
                            emu.feed_bytes(b"\r\n");
                            emu.write_notice(tr("term.hostkey_rejected"));
                        }
                        if let Some(handle) = self.pane_handles.get(&pane_id) {
                            handle.send_host_key_response(pending.prompt_id, false);
                        }
                        cx.stop_propagation();
                        cx.notify();
                    }
                    return;
                }
                if key == "enter" {
                    if let Some(pending) = self.pending_host_key_prompts.remove(&pane_id) {
                        let ans = pending.input_buf.trim().to_ascii_lowercase();
                        let accepted = ans == "yes" || ans == "y";
                        if let Some(emu) = self.pane_emulators.get_mut(&pane_id) {
                            emu.feed_bytes(b"\r\n");
                            if accepted {
                                emu.write_notice(&trf(
                                    "term.hostkey_accepted",
                                    &[
                                        ("host", &pending.hostname),
                                        ("port", &pending.port.to_string()),
                                    ],
                                ));
                            } else {
                                emu.write_notice(tr("term.hostkey_rejected"));
                            }
                        }
                        if let Some(handle) = self.pane_handles.get(&pane_id) {
                            handle.send_host_key_response(pending.prompt_id, accepted);
                        }
                        cx.stop_propagation();
                        cx.notify();
                    }
                    return;
                }
                if key == "backspace" {
                    if let Some(pending) = self.pending_host_key_prompts.get_mut(&pane_id) {
                        if pending.input_buf.pop().is_some() {
                            if let Some(emu) = self.pane_emulators.get_mut(&pane_id) {
                                emu.feed_bytes(b"\x08 \x08");
                            }
                            cx.stop_propagation();
                            cx.notify();
                        }
                    }
                    return;
                }
                if !cmd && !ctrl {
                    if let Some(ch_str) = &ks.key_char {
                        let cleaned: String = ch_str.chars().filter(|c| !c.is_control()).collect();
                        if !cleaned.is_empty() {
                            if let Some(pending) = self.pending_host_key_prompts.get_mut(&pane_id) {
                                pending.input_buf.push_str(&cleaned);
                            }
                            if let Some(emu) = self.pane_emulators.get_mut(&pane_id) {
                                emu.feed_bytes(cleaned.as_bytes());
                            }
                            cx.stop_propagation();
                            cx.notify();
                        }
                    }
                }
                return;
            }

            // 若当前窗格正在等待终端内 SSH 交互式认证输入（2FA/OTP、键盘交互、未存密码或私钥口令）
            if self.pending_auth_prompts.contains_key(&pane_id) {
                if key == "escape" || (ctrl && key == "c") {
                    if let Some(pending) = self.pending_auth_prompts.remove(&pane_id) {
                        if let Some(emu) = self.pane_emulators.get_mut(&pane_id) {
                            emu.feed_bytes(b"\r\n");
                        }
                        if let Some(handle) = self.pane_handles.get(&pane_id) {
                            handle.send_auth_response(pending.prompt_id, None);
                        }
                        cx.stop_propagation();
                        cx.notify();
                    }
                    return;
                }
                if key == "enter" {
                    if let Some(pending) = self.pending_auth_prompts.remove(&pane_id) {
                        if let Some(emu) = self.pane_emulators.get_mut(&pane_id) {
                            emu.feed_bytes(b"\r\n");
                        }
                        if let Some(handle) = self.pane_handles.get(&pane_id) {
                            handle.send_auth_response(pending.prompt_id, Some(pending.input_buf));
                        }
                        cx.stop_propagation();
                        cx.notify();
                    }
                    return;
                }
                if key == "backspace" {
                    if let Some(pending) = self.pending_auth_prompts.get_mut(&pane_id) {
                        if pending.input_buf.pop().is_some() {
                            if let Some(emu) = self.pane_emulators.get_mut(&pane_id) {
                                emu.feed_bytes(b"\x08 \x08");
                            }
                            cx.stop_propagation();
                            cx.notify();
                        }
                    }
                    return;
                }
                if !cmd && !ctrl {
                    if let Some(ch_str) = &ks.key_char {
                        let cleaned: String = ch_str.chars().filter(|c| !c.is_control()).collect();
                        if !cleaned.is_empty() {
                            let mut echo_flag = false;
                            if let Some(pending) = self.pending_auth_prompts.get_mut(&pane_id) {
                                pending.input_buf.push_str(&cleaned);
                                echo_flag = pending.echo;
                            }
                            if let Some(emu) = self.pane_emulators.get_mut(&pane_id) {
                                if echo_flag {
                                    emu.feed_bytes(cleaned.as_bytes());
                                } else {
                                    let mask = "*".repeat(cleaned.chars().count());
                                    emu.feed_bytes(mask.as_bytes());
                                }
                            }
                            cx.stop_propagation();
                            cx.notify();
                        }
                    }
                }
                return;
            }

            // 若当前窗格正在等待输入密钥库密码（不记住密码策略）
            if self.pending_vault_connects.contains_key(&pane_id) {
                if key == "escape" {
                    if let Some(pending) = self.pending_vault_connects.remove(&pane_id) {
                        if let Some(emu) = self.pane_emulators.get_mut(&pane_id) {
                            emu.feed_bytes(b"\r\n");
                            emu.write_notice(tr("workspace.vault_skipped"));
                        }
                        self.spawn_real_ssh_for_pane(&pane_id, &pending.host, None, None, None);
                        cx.notify();
                    }
                    return;
                }
                if key == "enter" {
                    if let Some(pending) = self.pending_vault_connects.remove(&pane_id) {
                        if let Some(emu) = self.pane_emulators.get_mut(&pane_id) {
                            emu.feed_bytes(b"\r\n");
                        }
                        if pending.input_buf.is_empty() {
                            if let Some(emu) = self.pane_emulators.get_mut(&pane_id) {
                                emu.write_notice(tr("workspace.vault_empty"));
                            }
                            self.spawn_real_ssh_for_pane(&pane_id, &pending.host, None, None, None);
                        } else {
                            match vault::unlock_vault(&pending.input_buf) {
                                Ok((mut transient_key, mut data)) => {
                                    if let Some(pending_snap) = sync::load_pending_pull() {
                                        let local_cat = self.build_sync_catalog();
                                        if let Some((merged_vdata, _vfile, merged_cat)) =
                                            sync::try_merge_snapshot_with_unlocked_key(
                                                &transient_key,
                                                &data,
                                                &local_cat,
                                                &pending_snap,
                                            )
                                        {
                                            data = merged_vdata;
                                            self.apply_merged_catalog(
                                                merged_cat,
                                                pending_snap.revision,
                                            );
                                            sync::clear_pending_pull();
                                        }
                                    }
                                    transient_key.zeroize();
                                    let (pwd, key_pem) = data
                                        .host_secrets
                                        .get(&pending.host.id)
                                        .map(|e| (e.password.clone(), e.private_key_pem.clone()))
                                        .unwrap_or((None, None));
                                    if let Some(emu) = self.pane_emulators.get_mut(&pane_id) {
                                        emu.write_notice(tr("workspace.vault_verified"));
                                    }
                                    self.spawn_real_ssh_for_pane(
                                        &pane_id,
                                        &pending.host,
                                        pwd,
                                        key_pem,
                                        Some(&data),
                                    );
                                }
                                Err(_) => {
                                    if let Some(emu) = self.pane_emulators.get_mut(&pane_id) {
                                        emu.write_notice(tr("workspace.vault_wrong"));
                                    }
                                    self.spawn_real_ssh_for_pane(
                                        &pane_id,
                                        &pending.host,
                                        None,
                                        None,
                                        None,
                                    );
                                }
                            }
                        }
                        cx.notify();
                    }
                    return;
                }
                if key == "backspace" {
                    if let Some(pending) = self.pending_vault_connects.get_mut(&pane_id) {
                        if pending.input_buf.pop().is_some() {
                            if let Some(emu) = self.pane_emulators.get_mut(&pane_id) {
                                emu.feed_bytes(b"\x08 \x08");
                            }
                            cx.notify();
                        }
                    }
                    return;
                }
                if !cmd && !ctrl {
                    if let Some(ch_str) = &ks.key_char {
                        let cleaned: String = ch_str.chars().filter(|c| !c.is_control()).collect();
                        if !cleaned.is_empty() {
                            let mask = "*".repeat(cleaned.chars().count());
                            if let Some(pending) = self.pending_vault_connects.get_mut(&pane_id) {
                                pending.input_buf.push_str(&cleaned);
                            }
                            if let Some(emu) = self.pane_emulators.get_mut(&pane_id) {
                                emu.feed_bytes(mask.as_bytes());
                            }
                            cx.notify();
                        }
                    }
                }
                return;
            }

            if !self.pane_handles.contains_key(&pane_id) {
                if key == "escape" && self.pending_reconnects.remove(&pane_id).is_some() {
                    self.pane_reconnect_attempts.remove(&pane_id);
                    if let Some(emu) = self.pane_emulators.get_mut(&pane_id) {
                        emu.write_notice(tr("term.auto_reconnect_cancelled"));
                    }
                    cx.stop_propagation();
                    cx.notify();
                    return;
                }
                if key == "enter" {
                    let host_id = self
                        .tabs
                        .iter()
                        .find(|t| t.id == self.active_tab_id)
                        .and_then(|t| t.root_pane.find_pane(&pane_id))
                        .map(|p| p.host_id.clone());
                    if let Some(hid) = host_id {
                        if let Some(host) = self.hosts.iter().find(|h| h.id == hid).cloned() {
                            self.connect_pane(&pane_id, &host, None, None);
                            cx.notify();
                        }
                    }
                }
                return;
            }

            // 复制 / 粘贴快捷键统一由原生菜单动作 MenuCopy / MenuPaste 处理（避免重复触发）
            if self.settings.shortcuts.matches_keystroke(ShortcutAction::Copy, ks)
                || self.settings.shortcuts.matches_keystroke(ShortcutAction::Paste, ks)
            {
                return;
            }

            let mode = self
                .pane_emulators
                .get(&pane_id)
                .map(|e| e.mode())
                .unwrap_or_else(TermMode::empty);

            // 关键：当焦点在纯终端模式且非控制/修饰/功能键时，不在此截断普通可打印按键，
            // 让 macOS NSTextInputClient -> GPUI EntityInputHandler 原生处理英/中/日/韩 IME，
            // 彻底杜绝重复上屏、候选框漂移与中文标点丢失（规避 Tabby #11916/#11013）！
            if let Some(bytes) = encode_terminal_special_keystroke(ks, mode, ctrl, cmd) {
                if let Some(emu) = self.pane_emulators.get_mut(&pane_id) {
                    emu.scroll_to_bottom();
                    emu.clear_selection();
                }
                if let Some(handle) = self.pane_handles.get(&pane_id) {
                    handle.send_bytes(bytes);
                }
                cx.stop_propagation();
                cx.notify();
            }
        }
    }

    pub fn open_about_settings(&mut self) {
        self.open_settings();
        self.active_settings_section = SettingsSection::About;
        self.active_settings_subpage = None;
    }

    pub fn open_sync_settings(&mut self) {
        self.open_settings();
        self.active_settings_section = SettingsSection::Sync;
        self.active_settings_subpage = None;
    }

    fn current_cloud_sync_visual_state(&self) -> CloudSyncVisualState {
        if self.settings.sync_provider == SyncProvider::Disabled || !self.has_sync_credentials() {
            return CloudSyncVisualState::Disabled;
        }
        if self.sync_in_progress {
            return match self.sync_active_action {
                Some(SyncAction::Push | SyncAction::ForcePush) => CloudSyncVisualState::Uploading,
                Some(SyncAction::Pull | SyncAction::AutoPull | SyncAction::ProbeRemote) => {
                    CloudSyncVisualState::Downloading
                }
                _ => CloudSyncVisualState::IdleSynced,
            };
        }
        if self.sync_last_failed {
            return CloudSyncVisualState::Failed;
        }
        CloudSyncVisualState::IdleSynced
    }

    pub fn clear_active_terminal(&mut self) {
        if self.screen != AppScreen::Workspace {
            return;
        }
        let Some(pane_id) = self.active_focused_pane_id() else {
            return;
        };
        if let Some(emu) = self.pane_emulators.get_mut(&pane_id) {
            emu.clear_selection();
            emu.feed_bytes(b"\x1b[2J\x1b[3J\x1b[H");
        }
        if let Some(handle) = self.pane_handles.get(&pane_id) {
            handle.send_bytes(vec![0x0c]);
        }
    }

    fn active_input_text_for_copy(&self) -> Option<String> {
        self.text_for_input(self.active_input)
    }

    /// 统一可变访问当前活跃非终端输入框缓冲区并执行编辑回调（自动处理设置落盘与终端搜索刷新）
    fn edit_active_input_buf<R>(&mut self, f: impl FnOnce(&mut TextCursorState, &mut String, bool) -> (R, bool)) -> Option<R> {
        let digits_only = matches!(
            self.active_input,
            ActiveInput::ModalPort | ActiveInput::ModalProxyPort
        );
        let mut save_settings = false;
        let mut refresh_term_search = false;
        let mut clear_conflict_err = false;

        let result = match self.active_input {
            ActiveInput::Terminal => None,
            ActiveInput::SidebarSearch if self.screen == AppScreen::Workspace => {
                Some(f(&mut self.text_cursor, &mut self.search_query, digits_only))
            }
            ActiveInput::TerminalSearch if self.screen == AppScreen::Workspace => {
                let res = self
                    .terminal_search
                    .as_mut()
                    .map(|st| f(&mut self.text_cursor, &mut st.query, digits_only));
                refresh_term_search = res.as_ref().is_some_and(|(_, modified)| *modified);
                res
            }
            ActiveInput::SidebarNewGroup
                if self.creating_group && self.screen == AppScreen::Workspace =>
            {
                Some(f(&mut self.text_cursor, &mut self.new_group_name, digits_only))
            }
            ActiveInput::SidebarRenameGroup
                if self.renaming_group.is_some() && self.screen == AppScreen::Workspace =>
            {
                Some(f(
                    &mut self.text_cursor,
                    &mut self.rename_group_input,
                    digits_only,
                ))
            }
            ActiveInput::SftpNewDir
                if self.sftp_creating_dir && self.screen == AppScreen::Workspace =>
            {
                Some(f(
                    &mut self.text_cursor,
                    &mut self.sftp_new_dir_name,
                    digits_only,
                ))
            }
            ActiveInput::ModalName if self.show_new_host_modal => {
                Some(f(&mut self.text_cursor, &mut self.new_host_draft.name, digits_only))
            }
            ActiveInput::ModalHost if self.show_new_host_modal => Some(f(
                &mut self.text_cursor,
                &mut self.new_host_draft.hostname,
                digits_only,
            )),
            ActiveInput::ModalPort if self.show_new_host_modal => {
                Some(f(&mut self.text_cursor, &mut self.new_host_draft.port, digits_only))
            }
            ActiveInput::ModalProxyHost if self.show_new_host_modal => Some(f(
                &mut self.text_cursor,
                &mut self.new_host_draft.proxy_host,
                digits_only,
            )),
            ActiveInput::ModalProxyPort if self.show_new_host_modal => Some(f(
                &mut self.text_cursor,
                &mut self.new_host_draft.proxy_port,
                digits_only,
            )),
            ActiveInput::ModalUser if self.show_new_host_modal => Some(f(
                &mut self.text_cursor,
                &mut self.new_host_draft.username,
                digits_only,
            )),
            ActiveInput::ModalPassword if self.show_new_host_modal => Some(f(
                &mut self.text_cursor,
                &mut self.new_host_draft.password,
                digits_only,
            )),
            ActiveInput::ModalKeyPath if self.show_new_host_modal => Some(f(
                &mut self.text_cursor,
                &mut self.new_host_draft.private_key_path,
                digits_only,
            )),
            ActiveInput::VaultModalPassword => self
                .vault_modal
                .as_mut()
                .map(|draft| f(&mut self.text_cursor, &mut draft.password, digits_only)),
            ActiveInput::VaultModalConfirm => self.vault_modal.as_mut().map(|draft| {
                let buf = if draft.is_setup {
                    &mut draft.confirm_password
                } else {
                    &mut draft.password
                };
                f(&mut self.text_cursor, buf, digits_only)
            }),
            ActiveInput::VaultSetupPassword if self.screen == AppScreen::Settings => Some(f(
                &mut self.text_cursor,
                &mut self.vault_settings_form.setup_password,
                digits_only,
            )),
            ActiveInput::VaultSetupConfirm if self.screen == AppScreen::Settings => Some(f(
                &mut self.text_cursor,
                &mut self.vault_settings_form.setup_confirm,
                digits_only,
            )),
            ActiveInput::VaultUnlockPassword if self.screen == AppScreen::Settings => Some(f(
                &mut self.text_cursor,
                &mut self.vault_settings_form.unlock_password,
                digits_only,
            )),
            ActiveInput::VaultChangeOld if self.screen == AppScreen::Settings => Some(f(
                &mut self.text_cursor,
                &mut self.vault_settings_form.change_old,
                digits_only,
            )),
            ActiveInput::VaultChangeNew if self.screen == AppScreen::Settings => Some(f(
                &mut self.text_cursor,
                &mut self.vault_settings_form.change_new,
                digits_only,
            )),
            ActiveInput::VaultChangeConfirm if self.screen == AppScreen::Settings => Some(f(
                &mut self.text_cursor,
                &mut self.vault_settings_form.change_confirm,
                digits_only,
            )),
            ActiveInput::SyncGistToken if self.screen == AppScreen::Settings => {
                let res = f(
                    &mut self.text_cursor,
                    &mut self.settings.sync_gist_token,
                    digits_only,
                );
                save_settings = res.1;
                Some(res)
            }
            ActiveInput::SyncGistId if self.screen == AppScreen::Settings => {
                let res = f(&mut self.text_cursor, &mut self.settings.gist_id, digits_only);
                save_settings = res.1;
                Some(res)
            }
            ActiveInput::SyncGistFilename if self.screen == AppScreen::Settings => {
                let res = f(
                    &mut self.text_cursor,
                    &mut self.settings.gist_filename,
                    digits_only,
                );
                save_settings = res.1;
                Some(res)
            }
            ActiveInput::SyncS3Endpoint if self.screen == AppScreen::Settings => {
                let res = f(
                    &mut self.text_cursor,
                    &mut self.settings.s3_endpoint,
                    digits_only,
                );
                save_settings = res.1;
                Some(res)
            }
            ActiveInput::SyncS3Region if self.screen == AppScreen::Settings => {
                let res = f(&mut self.text_cursor, &mut self.settings.s3_region, digits_only);
                save_settings = res.1;
                Some(res)
            }
            ActiveInput::SyncS3Bucket if self.screen == AppScreen::Settings => {
                let res = f(&mut self.text_cursor, &mut self.settings.s3_bucket, digits_only);
                save_settings = res.1;
                Some(res)
            }
            ActiveInput::SyncS3Key if self.screen == AppScreen::Settings => {
                let res = f(
                    &mut self.text_cursor,
                    &mut self.settings.s3_object_key,
                    digits_only,
                );
                save_settings = res.1;
                Some(res)
            }
            ActiveInput::SyncS3AccessKey if self.screen == AppScreen::Settings => {
                let res = f(
                    &mut self.text_cursor,
                    &mut self.settings.s3_access_key_id,
                    digits_only,
                );
                save_settings = res.1;
                Some(res)
            }
            ActiveInput::SyncS3SecretKey if self.screen == AppScreen::Settings => {
                let res = f(
                    &mut self.text_cursor,
                    &mut self.settings.sync_s3_secret,
                    digits_only,
                );
                save_settings = res.1;
                Some(res)
            }
            ActiveInput::SyncConflictPassword => {
                let res = self.sync_conflict_modal.as_mut().and_then(|st| {
                    if st.password_action.is_some() {
                        Some(f(&mut self.text_cursor, &mut st.password, digits_only))
                    } else {
                        None
                    }
                });
                clear_conflict_err = res.as_ref().is_some_and(|(_, modified)| *modified);
                res
            }
            _ => None,
        };

        if clear_conflict_err {
            if let Some(st) = self.sync_conflict_modal.as_mut() {
                st.error = None;
            }
        }
        if save_settings {
            self.settings.save();
        }
        if refresh_term_search {
            self.refresh_terminal_search_matches(true);
        }
        result.map(|(r, _)| r)
    }

    fn append_text_to_active_input(&mut self, text: &str) -> bool {
        self.edit_active_input_buf(|cursor, buf, digits_only| {
            let modified = cursor.insert_text(buf, text, digits_only);
            (modified, modified)
        })
        .unwrap_or(false)
    }

    fn perform_menu_cut(&mut self, cx: &mut Context<Self>) {
        if let Some(Some(cut)) = self.edit_active_input_buf(|cursor, buf, _| {
            let cut = cursor.cut_selection(buf);
            let modified = cut.is_some();
            (cut, modified)
        }) {
            if !cut.is_empty() {
                cx.write_to_clipboard(ClipboardItem::new_string(cut));
            }
            cx.notify();
        }
    }

    fn perform_menu_select_all(&mut self, cx: &mut Context<Self>) {
        if let Some(text) = self.active_input_text_for_copy() {
            self.text_cursor.select_all(&text);
            reset_caret_blink_epoch();
            cx.notify();
        }
    }

    fn perform_menu_copy(&mut self, cx: &mut Context<Self>) {
        if let Some(text) = self.active_input_text_for_copy() {
            let to_copy = if self.text_cursor.has_selection(&text) {
                self.text_cursor.selected_text(&text).to_string()
            } else {
                // 未手动拉选区时保持兼容：仍允许直接复制当前整行（若选中了局部则仅复制选区）
                text
            };
            if !to_copy.is_empty() {
                cx.write_to_clipboard(ClipboardItem::new_string(to_copy));
            }
            return;
        }
        if self.screen == AppScreen::Workspace
            && !self.show_new_host_modal
            && self.vault_modal.is_none()
        {
            self.copy_terminal_selection(cx);
        }
    }

    fn perform_menu_paste(&mut self, cx: &mut Context<Self>) {
        let Some(clip) = cx.read_from_clipboard().and_then(|c| c.text()) else {
            return;
        };
        if self.append_text_to_active_input(&clip) {
            cx.notify();
            return;
        }
        if self.screen != AppScreen::Workspace
            || self.show_new_host_modal
            || self.vault_modal.is_some()
        {
            return;
        }
        let Some(pane_id) = self.active_focused_pane_id() else {
            return;
        };
        if self.pending_host_key_prompts.contains_key(&pane_id) {
            let cleaned: String = clip.chars().filter(|c| !c.is_control()).collect();
            if !cleaned.is_empty() {
                if let Some(pending) = self.pending_host_key_prompts.get_mut(&pane_id) {
                    pending.input_buf.push_str(&cleaned);
                }
                if let Some(emu) = self.pane_emulators.get_mut(&pane_id) {
                    emu.feed_bytes(cleaned.as_bytes());
                }
                cx.notify();
            }
            return;
        }
        if self.pending_auth_prompts.contains_key(&pane_id) {
            let cleaned: String = clip.chars().filter(|c| !c.is_control()).collect();
            if !cleaned.is_empty() {
                let mut echo_flag = false;
                if let Some(pending) = self.pending_auth_prompts.get_mut(&pane_id) {
                    pending.input_buf.push_str(&cleaned);
                    echo_flag = pending.echo;
                }
                if let Some(emu) = self.pane_emulators.get_mut(&pane_id) {
                    if echo_flag {
                        emu.feed_bytes(cleaned.as_bytes());
                    } else {
                        let mask = "*".repeat(cleaned.chars().count());
                        emu.feed_bytes(mask.as_bytes());
                    }
                }
                cx.notify();
            }
            return;
        }
        if self.pending_vault_connects.contains_key(&pane_id) {
            let cleaned: String = clip.chars().filter(|c| !c.is_control()).collect();
            if !cleaned.is_empty() {
                let mask = "*".repeat(cleaned.chars().count());
                if let Some(pending) = self.pending_vault_connects.get_mut(&pane_id) {
                    pending.input_buf.push_str(&cleaned);
                }
                if let Some(emu) = self.pane_emulators.get_mut(&pane_id) {
                    emu.feed_bytes(mask.as_bytes());
                }
                cx.notify();
            }
            return;
        }
        self.paste_to_terminal(&pane_id, cx);
        cx.notify();
    }
}

/// GPUI 原生 macOS 输入法（`NSTextInputClient`）桥接：打通中/日/韩 IME 候选框光标跟随与内联预编辑
impl EntityInputHandler for HoloSshApp {
    fn text_for_range(
        &mut self,
        range: Range<usize>,
        adjusted_range: &mut Option<Range<usize>>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<String> {
        let marked = self.ime_marked_text.as_deref().unwrap_or("");
        let utf16: Vec<u16> = marked.encode_utf16().collect();
        let start = range.start.min(utf16.len());
        let end = range.end.min(utf16.len());
        *adjusted_range = Some(start..end);
        Some(String::from_utf16_lossy(&utf16[start..end]))
    }

    fn selected_text_range(
        &mut self,
        _ignore_disabled_input: bool,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        let len = self
            .ime_marked_text
            .as_deref()
            .map(|s| s.encode_utf16().count())
            .unwrap_or(0);
        Some(UTF16Selection {
            range: len..len,
            reversed: false,
        })
    }

    fn marked_text_range(
        &self,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<Range<usize>> {
        let marked = self.ime_marked_text.as_deref()?;
        if marked.is_empty() {
            None
        } else {
            Some(0..marked.encode_utf16().count())
        }
    }

    fn unmark_text(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        if self.ime_marked_text.take().is_some() {
            cx.notify();
        }
    }

    fn replace_text_in_range(
        &mut self,
        _range: Option<Range<usize>>,
        text: &str,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.ime_marked_text = None;
        if text.is_empty() {
            cx.notify();
            return;
        }

        // 1. 终端画布模式：直传 UTF-8 给 SSH PTY
        if self.screen == AppScreen::Workspace
            && self.active_input == ActiveInput::Terminal
            && !self.show_new_host_modal
            && self.vault_modal.is_none()
        {
            let Some(pane_id) = self.active_focused_pane_id() else {
                return;
            };
            if self.pending_vault_connects.contains_key(&pane_id)
                || self.pending_auth_prompts.contains_key(&pane_id)
                || self.pending_host_key_prompts.contains_key(&pane_id)
            {
                return;
            }
            if let Some(emu) = self.pane_emulators.get_mut(&pane_id) {
                emu.scroll_to_bottom();
                emu.clear_selection();
            }
            if let Some(handle) = self.pane_handles.get(&pane_id) {
                handle.send_bytes(text.as_bytes().to_vec());
            }
            cx.notify();
            return;
        }

        // 2. 所有非终端输入框统一通过原生 macOS IME 上屏（支持英/中/日/韩/符号/Emoji）
        let _ = self.append_text_to_active_input(text);
        cx.notify();
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        _range: Option<Range<usize>>,
        new_text: &str,
        _new_selected_range: Option<Range<usize>>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if new_text.is_empty() {
            self.ime_marked_text = None;
        } else {
            self.ime_marked_text = Some(new_text.to_string());
        }
        cx.notify();
    }

    fn bounds_for_range(
        &mut self,
        _range_utf16: Range<usize>,
        element_bounds: Bounds<Pixels>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        if self.screen == AppScreen::Workspace
            && self.active_input == ActiveInput::Terminal
            && !self.show_new_host_modal
            && self.vault_modal.is_none()
        {
            if let Some(pane_id) = self.active_focused_pane_id() {
                if let Some(m) = self.pane_metrics.borrow().get(&pane_id).copied() {
                    let x = f32::from(m.bounds.origin.x) + (m.cursor_col as f32) * m.cell_width;
                    let y = f32::from(m.bounds.origin.y) + (m.cursor_row as f32) * m.line_height;
                    return Some(Bounds::new(
                        point(px(x), px(y)),
                        size(px(m.cell_width.max(8.0)), px(m.line_height.max(16.0))),
                    ));
                }
            }
        }
        if let Some(caret) = current_ime_caret_bounds() {
            return Some(Bounds::new(
                caret.origin,
                size(px(10.0), px(f32::from(caret.size.height).max(16.0))),
            ));
        }
        Some(Bounds::new(
            element_bounds.origin,
            size(px(10.0), px(18.0)),
        ))
    }

    fn character_index_for_point(
        &mut self,
        _point: Point<Pixels>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<usize> {
        Some(0)
    }
}

fn apply_text_edit_key(
    cursor: &mut TextCursorState,
    buf: &mut String,
    ks: &gpui::Keystroke,
    cx: &mut Context<HoloSshApp>,
) -> TextKeyOutcome {
    let outcome = cursor.apply_key(buf, ks);
    if outcome != TextKeyOutcome::Unhandled {
        cx.stop_propagation();
    }
    outcome
}

/// 完整 `TermMode` 感知的终端特殊/控制/功能/修饰按键编码器
///
/// - 动态遵循 `TermMode::APP_CURSOR`（彻底根治 `vim`/`tmux`/`less` 下 Home/End 与方向键异常，Tabby #2738）
/// - 完整支持 XTerm CSI 修饰位编码（`Ctrl+Left/Right`、`Shift+Home`、`Alt+Up`、`Ctrl+F1..F12` 等）
/// - 普通可打印文本交由 GPUI `EntityInputHandler` 走原生 macOS IME，避免重复输入
fn encode_terminal_special_keystroke(
    ks: &gpui::Keystroke,
    mode: TermMode,
    ctrl: bool,
    cmd: bool,
) -> Option<Vec<u8>> {
    if cmd {
        return None;
    }
    let key = ks.key.as_str();
    let shift = ks.modifiers.shift;
    let alt = ks.modifiers.alt;
    let app_cursor = mode.contains(TermMode::APP_CURSOR);

    // XTerm CSI 修饰参数：1 + (Shift=1) + (Alt=2) + (Ctrl=4)
    let mod_param = 1u8 + (if shift { 1 } else { 0 }) + (if alt { 2 } else { 0 }) + (if ctrl { 4 } else { 0 });

    // 1. 方向键 / Home / End（感知 DECCKM APP_CURSOR 与修饰组合键）
    let cursor_final = match key {
        "up" => Some('A'),
        "down" => Some('B'),
        "right" => Some('C'),
        "left" => Some('D'),
        "home" => Some('H'),
        "end" => Some('F'),
        _ => None,
    };
    if let Some(ch) = cursor_final {
        return if mod_param > 1 {
            Some(format!("\x1b[1;{mod_param}{ch}").into_bytes())
        } else if app_cursor {
            Some(format!("\x1bO{ch}").into_bytes())
        } else {
            Some(format!("\x1b[{ch}").into_bytes())
        };
    }

    // 2. Delete / Insert / PageUp / PageDown（带修饰编码）
    let tilde_code = match key {
        "insert" => Some(2u8),
        "delete" => Some(3u8),
        "pageup" => Some(5u8),
        "pagedown" => Some(6u8),
        "f5" => Some(15u8),
        "f6" => Some(17u8),
        "f7" => Some(18u8),
        "f8" => Some(19u8),
        "f9" => Some(20u8),
        "f10" => Some(21u8),
        "f11" => Some(23u8),
        "f12" => Some(24u8),
        _ => None,
    };
    if let Some(code) = tilde_code {
        return if mod_param > 1 {
            Some(format!("\x1b[{code};{mod_param}~").into_bytes())
        } else {
            Some(format!("\x1b[{code}~").into_bytes())
        };
    }

    // 3. F1–F4（SS3 / 带修饰 CSI）
    let f1_4 = match key {
        "f1" => Some('P'),
        "f2" => Some('Q'),
        "f3" => Some('R'),
        "f4" => Some('S'),
        _ => None,
    };
    if let Some(ch) = f1_4 {
        return if mod_param > 1 {
            Some(format!("\x1b[1;{mod_param}{ch}").into_bytes())
        } else {
            Some(format!("\x1bO{ch}").into_bytes())
        };
    }

    // 4. 控制/编辑常用特殊键
    let kitty_keyboard = mode.contains(TermMode::DISAMBIGUATE_ESC_CODES);
    match key {
        "enter" => {
            if shift && !ctrl && !alt {
                return Some(if kitty_keyboard {
                    b"\x1b[13;2u".to_vec()
                } else {
                    b"\x1b\r".to_vec()
                });
            }
            if ctrl && !shift && !alt {
                return Some(if kitty_keyboard {
                    b"\x1b[13;5u".to_vec()
                } else {
                    vec![b'\r']
                });
            }
            if alt {
                return Some(b"\x1b\r".to_vec());
            }
            return Some(vec![b'\r']);
        }
        "backspace" => {
            if alt {
                return Some(vec![0x1b, 0x7f]);
            }
            if ctrl {
                return Some(vec![0x08]);
            }
            return Some(vec![0x7f]);
        }
        "tab" => {
            return if shift {
                Some(b"\x1b[Z".to_vec())
            } else {
                Some(vec![b'\t'])
            };
        }
        "escape" => return Some(vec![0x1b]),
        _ => {}
    }

    // 5. Ctrl+<key> 控制字符
    if ctrl {
        match key {
            "space" | "@" | "2" => return Some(vec![0x00]),
            "[" => return Some(vec![0x1b]),
            "\\" => return Some(vec![0x1c]),
            "]" => return Some(vec![0x1d]),
            "^" | "6" => return Some(vec![0x1e]),
            "_" | "-" => return Some(vec![0x1f]),
            s if s.len() == 1 => {
                let b = s.as_bytes()[0].to_ascii_lowercase();
                if b.is_ascii_lowercase() {
                    let ctrl_byte = b - b'a' + 1;
                    return if alt {
                        Some(vec![0x1b, ctrl_byte])
                    } else {
                        Some(vec![ctrl_byte])
                    };
                }
            }
            _ => {}
        }
    }

    // 6. Alt/Option+<key> Meta 前缀（优先用物理 ASCII 键名 ks.key，避免 macOS Option+b/f/d 变成 ∫/ƒ/∂）
    if alt && !ctrl {
        let key_bytes = key.as_bytes();
        if key_bytes.len() == 1 && key_bytes[0].is_ascii_graphic() {
            let b = if shift && key_bytes[0].is_ascii_alphabetic() {
                key_bytes[0].to_ascii_uppercase()
            } else {
                key_bytes[0]
            };
            return Some(vec![0x1b, b]);
        }
        if key == "space" {
            return Some(vec![0x1b, b' ']);
        }
        if let Some(ch) = ks.key_char.as_ref().filter(|s| !s.is_empty()) {
            let mut out = vec![0x1b];
            out.extend_from_slice(ch.as_bytes());
            return Some(out);
        }
    }

    // 普通可打印按键返回 None，交由 GPUI EntityInputHandler (macOS IME) 统一输出
    None
}

/// POSIX Shell 单引号安全转义（用于拖放本地路径到终端）
fn shell_escape_posix_path(path: &str) -> String {
    if !path.is_empty()
        && path.bytes().all(|b| {
            b.is_ascii_alphanumeric() || matches!(b, b'/' | b'.' | b'_' | b'-' | b'+' | b':')
        })
    {
        return path.to_string();
    }
    let escaped = path.replace('\'', "'\\''");
    format!("'{escaped}'")
}

fn extract_dir_from_osc_title(title: &str) -> Option<String> {
    if let Some((_, dir)) = title.rsplit_once(':') {
        let trimmed = dir.trim();
        if !trimmed.is_empty() {
            return Some(trimmed.to_string());
        }
    }
    None
}

fn abbreviate_home_path(path: &PathBuf) -> String {
    if let Some(home) = dirs::home_dir() {
        if let Ok(rel) = path.strip_prefix(&home) {
            return format!("~/{}", rel.display());
        }
    }
    path.display().to_string()
}

impl Render for HoloSshApp {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        set_active_ime_marked_text(self.ime_marked_text.as_deref());
        sync_active_text_input_render_state(self.active_input, self.text_cursor);
        let vp = window.viewport_size();
        let cur_vp = (f32::from(vp.width), f32::from(vp.height));
        if (cur_vp.0 - self.last_viewport_size.0).abs() > 2.0
            || (cur_vp.1 - self.last_viewport_size.1).abs() > 2.0
        {
            self.last_viewport_size = cur_vp;
            self.sync_active_tab_pty_sizes();
        }
        let (vp_w, vp_h) = self.last_viewport_size;

        // 若有刚下载完成的内置字体，立刻注册至 GPUI 进程私有内存字体源（绝不安装到系统）
        if !self.pending_register_fonts.is_empty() {
            let pending: Vec<std::borrow::Cow<'static, [u8]>> = self
                .pending_register_fonts
                .drain(..)
                .map(std::borrow::Cow::Owned)
                .collect();
            let _ = window.text_system().add_fonts(pending);
        }

        let effective_font = fonts::effective_gpui_family(&self.settings.terminal_font_family);

        let desired_glass = (
            self.settings.glass_enabled,
            self.settings.blur_radius.round() as i32,
        );
        if self.last_applied_glass != Some(desired_glass) {
            macos_glass::apply_window_glass(
                window,
                self.settings.glass_enabled,
                self.settings.blur_radius,
            );
            self.last_applied_glass = Some(desired_glass);
        }

        let (theme, is_dark) = self.current_theme(window);
        if self.last_applied_app_icon_dark != Some(is_dark) {
            macos_glass::apply_app_icon(is_dark);
            self.last_applied_app_icon_dark = Some(is_dark);
        }
        let recording_now = self.recording_shortcut.is_some();
        if recording_now {
            if !self.last_shortcuts_suspended {
                macos_menu::suspend_custom_keybindings(cx);
                self.last_shortcuts_suspended = true;
            }
        } else if self.last_shortcuts_suspended
            || self.last_applied_shortcuts.as_ref() != Some(&self.settings.shortcuts)
            || self.last_applied_menu_lang != Some(self.settings.language)
        {
            macos_menu::rebind_keybindings(&self.settings.shortcuts, cx);
            macos_menu::apply_app_menus(self.settings.language, cx);
            self.last_shortcuts_suspended = false;
            self.last_applied_shortcuts = Some(self.settings.shortcuts.clone());
            self.last_applied_menu_lang = Some(self.settings.language);
        }

        let body = match self.screen {
            AppScreen::Onboarding => div().size_full().child(render_onboarding_view(
                &self.settings,
                self.detected_ssh_config_hosts.len(),
                self.import_ssh_config_on_start,
                &theme,
                cx,
            )),
            AppScreen::Settings => {
                let vault_entries = self.vault_entries_sorted();
                div().size_full().child(render_settings_view(
                    &self.settings,
                    self.active_settings_section,
                    self.active_settings_subpage,
                    vault::vault_exists(),
                    self.is_vault_unlocked(),
                    &vault_entries,
                    self.vault_revealed_host_id.as_deref(),
                    &self.vault_settings_form,
                    &self.vault_local_keys,
                    self.hosts.len(),
                    self.active_input,
                    self.recording_shortcut,
                    self.sync_show_secret,
                    self.sync_in_progress,
                    &theme,
                    cx,
                ))
            }
            AppScreen::Workspace => {
                let active_tab = self
                    .tabs
                    .iter()
                    .find(|t| t.id == self.active_tab_id)
                    .or_else(|| self.tabs.first())
                    .cloned();

                let main_pane: gpui::AnyElement = if let Some(tab) = active_tab.as_ref() {
                    let sftp_open = tab.sftp_open;
                    let sftp_anim_gen = self.sftp_anim_gen;
                    let tab_anim_gen = self.tab_anim_gen;
                    let mut workspace_content = div()
                        .flex_1()
                        .min_w_0()
                        .min_h_0()
                        .overflow_hidden()
                        .size_full()
                        .flex()
                        .flex_row()
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .min_h_0()
                                .overflow_hidden()
                                .size_full()
                                .bg(theme.terminal_bg)
                                .child(
                                    div()
                                        .size_full()
                                        .child({
                                            let render_params = TerminalRenderParams {
                                                focused_pane_id: &tab.focused_pane_id,
                                                show_pane_headers: tab.split_active,
                                                emulators: &self.pane_emulators,
                                                font_family: effective_font,
                                                base_font_size: self.settings.terminal_font_size,
                                                cursor_style: self.settings.cursor_style,
                                                is_dark,
                                                theme: &theme,
                                                active_input: self.active_input,
                                                ime_marked_text: if self.active_input
                                                    == ActiveInput::Terminal
                                                    && !self.show_new_host_modal
                                                    && self.vault_modal.is_none()
                                                {
                                                    self.ime_marked_text.as_deref()
                                                } else {
                                                    None
                                                },
                                                search_state: self.terminal_search.as_ref(),
                                                paste_confirm: self.pending_paste_confirm.as_ref(),
                                                hovered_link: self
                                                    .hovered_link
                                                    .as_ref()
                                                    .map(|(pid, span, _)| (pid.as_str(), span.clone())),
                                                focus_handle: self.focus_handle.clone(),
                                                app_entity: cx.entity().clone(),
                                                metrics_store: self.pane_metrics.clone(),
                                                shape_cache: self.pane_shapes.clone(),
                                            };
                                            render_split_tree(&tab.root_pane, &render_params, cx)
                                        })
                                        .with_animation(
                                            SharedString::from(format!("tab-fade-{tab_anim_gen}")),
                                            Animation::new(Duration::from_millis(165))
                                                .with_easing(ease_out_quint()),
                                            |el, d| el.opacity(0.35 + 0.65 * d),
                                        ),
                                ),
                        );

                    // SFTP 抽屉丝滑展开/收起宽度与透明度过渡（本地终端会话强制禁用 SFTP）
                    let active_pane_is_local = self.pane_is_local(&tab.focused_pane_id);
                    let effective_sftp_open = sftp_open && !active_pane_is_local;
                    let show_sftp_drawer = !active_pane_is_local
                        && (effective_sftp_open
                            || (sftp_anim_gen > 0
                                && self.sftp_anim_tab_id.as_deref() == Some(tab.id.as_str())));
                    if show_sftp_drawer {
                        let sftp_target_w = self.settings.sftp_width.clamp(260.0, 740.0);
                        let sftp_drawer = div()
                            .flex_none()
                            .h_full()
                            .overflow_hidden()
                            .child(render_sftp_panel(
                                &self.sftp,
                                self.sftp_creating_dir,
                                &self.sftp_new_dir_name,
                                self.active_input == ActiveInput::SftpNewDir,
                                self.sftp_scroll_handle.clone(),
                                sftp_target_w,
                                &theme,
                                cx,
                            ))
                            .with_animation(
                                SharedString::from(format!(
                                    "sftp-drawer-{sftp_anim_gen}-{effective_sftp_open}"
                                )),
                                Animation::new(Duration::from_millis(220))
                                    .with_easing(ease_out_quint()),
                                move |el, d| {
                                    let w = if effective_sftp_open {
                                        sftp_target_w * d
                                    } else {
                                        sftp_target_w * (1.0 - d)
                                    };
                                    let op = if effective_sftp_open {
                                        0.35 + 0.65 * d
                                    } else {
                                        1.0 - d
                                    };
                                    el.w(px(w)).opacity(op)
                                },
                            );
                        workspace_content = workspace_content.child(sftp_drawer);
                    }

                    // 单层无缝着色：min_w_0 + overflow_hidden 锁定主区宽度严格等于窗口剩余宽度，永不将右上角胶囊顶出可视区
                    div()
                        .flex_1()
                        .min_w_0()
                        .min_h_0()
                        .overflow_hidden()
                        .h_full()
                        .flex()
                        .flex_col()
                        .child(
                            div()
                                .w_full()
                                .flex_none()
                                .bg(theme.terminal_bg)
                                .border_b_1()
                                .border_color(theme.border_subtle)
                                .child(render_titlebar(
                                    &self.tabs,
                                    &self.active_tab_id,
                                    self.sidebar_collapsed,
                                    self.settings.sidebar_side,
                                    tab.split_active,
                                    effective_sftp_open,
                                    active_pane_is_local,
                                    &theme,
                                    cx,
                                )),
                        )
                        .child(workspace_content)
                        .into_any_element()
                } else {
                    // 未选择任何主机时展示 Figma 08 初始空状态（带柔和淡入）
                    render_empty_workspace_pane(
                        self.sidebar_collapsed,
                        self.settings.sidebar_side,
                        self.settings.language,
                        &theme,
                        cx,
                    )
                    .into_any_element()
                };

                let mut row = div().size_full().overflow_hidden().flex().flex_row();
                let sb_collapsed = self.sidebar_collapsed;
                let sb_anim_gen = self.sidebar_anim_gen;
                let sb_target_w = self.settings.sidebar_width.clamp(190.0, 480.0);
                let cloud_sync_state = self.current_cloud_sync_visual_state();
                self.host_drop_zones.borrow_mut().clear();
                let active_host_drag = self.host_drag.as_ref().filter(|d| d.active);
                let sidebar_el: Option<gpui::AnyElement> =
                    (!sb_collapsed || sb_anim_gen > 0).then(|| {
                        let search_kbd = self
                            .settings
                            .shortcuts
                            .display_for(ShortcutAction::SearchHosts);
                        let inner = render_sidebar(
                            &self.hosts,
                            &self.settings.custom_groups,
                            &self.settings.group_updated_at,
                            &self.active_host_id,
                            &self.expanded_groups,
                            &self.search_query,
                            &search_kbd,
                            self.active_input == ActiveInput::SidebarSearch,
                            self.search_expanded,
                            self.search_anim_gen,
                            self.creating_group,
                            &self.new_group_name,
                            self.active_input == ActiveInput::SidebarNewGroup,
                            self.renaming_group.as_deref(),
                            &self.rename_group_input,
                            self.active_input == ActiveInput::SidebarRenameGroup,
                            is_dark,
                            self.settings.compact_host_list,
                            self.settings.show_groups,
                            self.settings.sidebar_side,
                            sb_target_w,
                            cloud_sync_state,
                            active_host_drag,
                            self.host_drop_zones.clone(),
                            self.settings.language,
                            &theme,
                            cx,
                        );
                        if sb_anim_gen == 0 {
                            div()
                                .flex_none()
                                .w(px(sb_target_w))
                                .h_full()
                                .overflow_hidden()
                                .child(inner)
                                .into_any_element()
                        } else {
                            div()
                                .flex_none()
                                .h_full()
                                .overflow_hidden()
                                .child(inner)
                                .with_animation(
                                    SharedString::from(format!(
                                        "sidebar-slide-{sb_anim_gen}-{sb_collapsed}"
                                    )),
                                    Animation::new(Duration::from_millis(220))
                                        .with_easing(ease_out_quint()),
                                    move |el, d| {
                                        let w = if sb_collapsed {
                                            sb_target_w * (1.0 - d)
                                        } else {
                                            sb_target_w * d
                                        };
                                        let op = if sb_collapsed {
                                            1.0 - d
                                        } else {
                                            0.25 + 0.75 * d
                                        };
                                        el.w(px(w)).opacity(op)
                                    },
                                )
                                .into_any_element()
                        }
                    });

                match self.settings.sidebar_side {
                    SidebarSide::Left => {
                        if let Some(sb) = sidebar_el {
                            row = row.child(sb);
                        }
                        row = row.child(main_pane);
                    }
                    SidebarSide::Right => {
                        row = row.child(main_pane);
                        if let Some(sb) = sidebar_el {
                            row = row.child(sb);
                        }
                    }
                }
                row
            }
        };

        // 不对整窗壳体施加透明度渐变，确保主页 ↔ 设置页切换时左右磨砂/实底外壳纹丝不动、零透底闪烁
        let animated_body = body.into_any_element();

        let workspace_shortcuts_active = self.screen == AppScreen::Workspace
            && self.settings.shortcuts_enabled
            && !self.show_new_host_modal
            && self.vault_modal.is_none()
            && self.sync_conflict_modal.is_none();

        let mut root = div()
            .id("holossh-root")
            .track_focus(&self.focus_handle)
            .when(workspace_shortcuts_active, |el| el.key_context("Workspace"))
            .on_action(cx.listener(|this, _: &MenuAbout, _, cx| {
                this.open_about_settings();
                cx.notify();
            }))
            .on_action(cx.listener(|this, _: &MenuSettings, _, cx| {
                if this.screen == AppScreen::Settings {
                    this.close_settings();
                } else {
                    this.open_settings();
                }
                cx.notify();
            }))
            .on_action(cx.listener(|this, _: &MenuNewHost, _, cx| {
                if this.screen == AppScreen::Settings {
                    this.close_settings();
                }
                if this.screen == AppScreen::Workspace {
                    this.open_new_host_modal();
                    cx.notify();
                }
            }))
            .on_action(cx.listener(|this, _: &MenuNewGroup, _, cx| {
                if this.screen == AppScreen::Settings {
                    this.close_settings();
                }
                if this.screen == AppScreen::Workspace {
                    if this.sidebar_collapsed {
                        this.sidebar_collapsed = false;
                        this.sidebar_anim_gen += 1;
                        this.sync_active_tab_pty_sizes();
                    }
                    this.start_creating_group();
                    cx.notify();
                }
            }))
            .on_action(cx.listener(|this, _: &MenuNewTab, _, cx| {
                if this.screen == AppScreen::Settings {
                    this.close_settings();
                }
                if this.screen == AppScreen::Workspace {
                    this.new_tab();
                    cx.notify();
                }
            }))
            .on_action(cx.listener(|this, _: &MenuClosePaneOrTab, _, cx| {
                if this.screen == AppScreen::Workspace {
                    if let Some(tab) = this.tabs.iter().find(|t| t.id == this.active_tab_id) {
                        if tab.root_pane.pane_count() > 1 {
                            let pid = tab.focused_pane_id.clone();
                            this.close_pane(&pid);
                        } else {
                            let tid = tab.id.clone();
                            this.close_tab(&tid);
                        }
                    }
                    cx.notify();
                }
            }))
            .on_action(cx.listener(|this, _: &MenuCut, _, cx| {
                this.perform_menu_cut(cx);
            }))
            .on_action(cx.listener(|this, _: &MenuCopy, _, cx| {
                this.perform_menu_copy(cx);
            }))
            .on_action(cx.listener(|this, _: &MenuPaste, _, cx| {
                this.perform_menu_paste(cx);
            }))
            .on_action(cx.listener(|this, _: &MenuSelectAll, _, cx| {
                this.perform_menu_select_all(cx);
            }))
            .on_action(cx.listener(|this, _: &MenuSearchHosts, _, cx| {
                if this.screen == AppScreen::Settings {
                    this.close_settings();
                }
                if this.screen == AppScreen::Workspace {
                    if this.sidebar_collapsed {
                        this.sidebar_collapsed = false;
                        this.sidebar_anim_gen += 1;
                        this.sync_active_tab_pty_sizes();
                    }
                    this.set_active_input(ActiveInput::SidebarSearch);
                    cx.notify();
                }
            }))
            .on_action(cx.listener(|this, _: &MenuFindInTerminal, _, cx| {
                if this.screen == AppScreen::Workspace {
                    this.open_terminal_search();
                    cx.notify();
                }
            }))
            .on_action(cx.listener(|this, _: &MenuClearTerminal, _, cx| {
                this.clear_active_terminal();
                cx.notify();
            }))
            .on_action(cx.listener(|this, _: &MenuSplitRight, _, cx| {
                if this.screen == AppScreen::Workspace {
                    this.split_focused_pane(SplitAxis::Horizontal);
                    cx.notify();
                }
            }))
            .on_action(cx.listener(|this, _: &MenuSplitDown, _, cx| {
                if this.screen == AppScreen::Workspace {
                    this.split_focused_pane(SplitAxis::Vertical);
                    cx.notify();
                }
            }))
            .on_action(cx.listener(|this, _: &MenuToggleSftp, _, cx| {
                if this.screen == AppScreen::Workspace {
                    this.toggle_sftp_panel();
                    cx.notify();
                }
            }))
            .on_action(cx.listener(|this, _: &MenuToggleSidebar, _, cx| {
                if this.screen == AppScreen::Workspace {
                    this.toggle_sidebar();
                    cx.notify();
                }
            }))
            .on_action(cx.listener(|this, _: &MenuToggleAppearance, _, cx| {
                this.toggle_appearance();
                cx.notify();
            }))
            .on_action(cx.listener(|this, _: &MenuZoomIn, _, cx| {
                this.adjust_font_size(1.0);
                cx.notify();
            }))
            .on_action(cx.listener(|this, _: &MenuZoomOut, _, cx| {
                this.adjust_font_size(-1.0);
                cx.notify();
            }))
            .on_action(cx.listener(|this, _: &MenuZoomReset, _, cx| {
                this.set_font_size(13.5);
                cx.notify();
            }))
            .on_action(cx.listener(|_, _: &MenuToggleFullscreen, window, _| {
                window.toggle_fullscreen();
            }))
            .on_action(cx.listener(|_, _: &MenuMinimize, window, _| {
                window.minimize_window();
            }))
            .on_action(cx.listener(|_, _: &MenuZoomWindow, window, _| {
                window.zoom_window();
            }))
            .on_key_down(cx.listener(Self::handle_key_down))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _: &MouseDownEvent, _, cx| {
                    let mut dirty = false;
                    if this.recording_shortcut.take().is_some() {
                        dirty = true;
                    }
                    if this.active_input == ActiveInput::SidebarSearch {
                        this.blur_sidebar_search();
                        dirty = true;
                    }
                    if dirty {
                        cx.notify();
                    }
                }),
            )
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(|this, _: &MouseDownEvent, _, cx| {
                    if this.recording_shortcut.take().is_some() {
                        cx.notify();
                    }
                }),
            )
            .on_mouse_move(cx.listener(Self::handle_mouse_move))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::handle_mouse_up))
            .relative()
            .size_full()
            .child(animated_body);

        // 当焦点处于任意非终端输入框（搜索框 / 弹窗 / 设置页 / 新建分组 / SFTP）或无终端标签时，
        // 在根节点挂接 GPUI ElementInputHandler，让 macOS 原生中/日/韩 IME 完整接管文本输入与候选框定位
        let terminal_owns_ime = self.screen == AppScreen::Workspace
            && self.active_input == ActiveInput::Terminal
            && !self.show_new_host_modal
            && self.vault_modal.is_none()
            && self.sync_conflict_modal.is_none()
            && !self.tabs.is_empty();

        // 光标进入终端活跃态（上升沿）时自动切换至系统英文输入法；停留在终端内时绝不干预用户手动切换中文等其他输入法
        let terminal_active_now = terminal_owns_ime && window.is_window_active();
        if terminal_active_now && !self.last_terminal_active {
            macos_glass::switch_to_ascii_input_source();
        }
        self.last_terminal_active = terminal_active_now;
        if !terminal_owns_ime {
            let ime_focus = self.focus_handle.clone();
            let ime_entity = cx.entity().clone();
            root = root.child(
                canvas(
                    move |_bounds, _window, _cx| {},
                    move |bounds: Bounds<Pixels>, _, window, cx| {
                        window.handle_input(
                            &ime_focus,
                            ElementInputHandler::new(bounds, ime_entity.clone()),
                            cx,
                        );
                    },
                )
                .absolute()
                .top_0()
                .left_0()
                .w(px(1.0))
                .h(px(1.0)),
            );
        }

        if let Some(menu) = self.host_context_menu.clone() {
            let menu_w = 148.0_f32;
            let menu_h = 104.0_f32;
            let raw_x: f32 = menu.position.x.into();
            let raw_y: f32 = menu.position.y.into();
            let x = raw_x.clamp(8.0, (vp_w - menu_w - 8.0).max(8.0));
            let y = raw_y.clamp(8.0, (vp_h - menu_h - 8.0).max(8.0));
            let connect_id = menu.host_id.clone();
            let edit_id = menu.host_id.clone();
            let delete_id = menu.host_id;
            let danger_color: gpui::Hsla = if is_dark {
                gpui::Rgba {
                    r: 0.96,
                    g: 0.44,
                    b: 0.44,
                    a: 0.92,
                }
                .into()
            } else {
                gpui::Rgba {
                    r: 0.78,
                    g: 0.18,
                    b: 0.18,
                    a: 0.92,
                }
                .into()
            };

            let ctx_gen = self.ctx_menu_anim_gen;
            root = root.child(
                div()
                    .id("host-ctx-backdrop")
                    .absolute()
                    .inset_0()
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, _: &MouseDownEvent, _, cx| {
                            this.close_host_context_menu();
                            cx.notify();
                        }),
                    )
                    .on_mouse_down(
                        MouseButton::Right,
                        cx.listener(|this, _: &MouseDownEvent, _, cx| {
                            this.close_host_context_menu();
                            cx.notify();
                        }),
                    )
                    .child(
                        div()
                            .id("host-ctx-menu")
                            .absolute()
                            .left(px(x))
                            .w(px(menu_w))
                            .p(px(5.0))
                            .flex()
                            .flex_col()
                            .gap(px(2.0))
                            .rounded(px(10.0))
                            .bg(theme.sheet_bg)
                            .border_1()
                            .border_color(theme.border_specular)
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(|_, _: &MouseDownEvent, _, cx| {
                                    cx.stop_propagation();
                                }),
                            )
                            .child(
                                div()
                                    .id("ctx-host-connect")
                                    .w_full()
                                    .h(px(28.0))
                                    .px(px(10.0))
                                    .rounded(px(6.5))
                                    .flex()
                                    .items_center()
                                    .gap(px(8.0))
                                    .cursor_pointer()
                                    .hover(|s| s.bg(theme.surface_active))
                                    .active(|s| s.opacity(0.75))
                                    .on_click(cx.listener(move |this, _, window, cx| {
                                        this.close_host_context_menu();
                                        window.focus(&this.focus_handle);
                                        this.select_host(&connect_id);
                                        cx.notify();
                                    }))
                                    .child(icon_svg("icons/terminal.svg", 13.0, theme.text_secondary))
                                    .child(
                                        div()
                                            .text_size(crate::theme::LayoutMetrics::font_control())
                                            .text_color(theme.text_primary)
                                            .child(tr("common.connect")),
                                    ),
                            )
                            .child(
                                div()
                                    .id("ctx-host-edit")
                                    .w_full()
                                    .h(px(28.0))
                                    .px(px(10.0))
                                    .rounded(px(6.5))
                                    .flex()
                                    .items_center()
                                    .gap(px(8.0))
                                    .cursor_pointer()
                                    .hover(|s| s.bg(theme.surface_active))
                                    .active(|s| s.opacity(0.75))
                                    .on_click(cx.listener(move |this, _, window, cx| {
                                        window.focus(&this.focus_handle);
                                        this.open_edit_host_modal(&edit_id);
                                        cx.notify();
                                    }))
                                    .child(icon_svg("icons/gear.svg", 13.0, theme.text_secondary))
                                    .child(
                                        div()
                                            .text_size(crate::theme::LayoutMetrics::font_control())
                                            .text_color(theme.text_primary)
                                            .child(tr("common.edit")),
                                    ),
                            )
                            .child(div().w_full().py(px(2.0)).px(px(6.0)).child(hairline_h(&theme)))
                            .child(
                                div()
                                    .id("ctx-host-delete")
                                    .w_full()
                                    .h(px(28.0))
                                    .px(px(10.0))
                                    .rounded(px(6.5))
                                    .flex()
                                    .items_center()
                                    .gap(px(8.0))
                                    .cursor_pointer()
                                    .hover(|s| s.bg(theme.surface_active))
                                    .active(|s| s.opacity(0.75))
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.delete_host(&delete_id);
                                        cx.notify();
                                    }))
                                    .child(icon_svg("icons/close.svg", 11.5, danger_color))
                                    .child(
                                        div()
                                            .text_size(crate::theme::LayoutMetrics::font_control())
                                            .text_color(danger_color)
                                            .child(tr("common.delete")),
                                    ),
                            )
                            .with_animation(
                                SharedString::from(format!("ctx-pop-{ctx_gen}")),
                                Animation::new(Duration::from_millis(145))
                                    .with_easing(ease_out_quint()),
                                move |el, d| el.top(px(y - 5.0 * (1.0 - d))).opacity(d),
                            ),
                    ),
            );
        }

        if let Some(gmenu) = self.group_context_menu.clone() {
            let menu_w = 140.0_f32;
            let menu_h = 74.0_f32;
            let raw_x: f32 = gmenu.position.x.into();
            let raw_y: f32 = gmenu.position.y.into();
            let x = raw_x.clamp(8.0, (vp_w - menu_w - 8.0).max(8.0));
            let y = raw_y.clamp(8.0, (vp_h - menu_h - 8.0).max(8.0));
            let rename_target = gmenu.group_name.clone();
            let delete_target = gmenu.group_name;
            let ctx_gen = self.ctx_menu_anim_gen;
            let danger_color: gpui::Hsla = if is_dark {
                gpui::Rgba {
                    r: 0.96,
                    g: 0.44,
                    b: 0.44,
                    a: 0.92,
                }
                .into()
            } else {
                gpui::Rgba {
                    r: 0.78,
                    g: 0.18,
                    b: 0.18,
                    a: 0.92,
                }
                .into()
            };

            root = root.child(
                div()
                    .id("group-ctx-backdrop")
                    .absolute()
                    .inset_0()
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, _: &MouseDownEvent, _, cx| {
                            this.close_group_context_menu();
                            cx.notify();
                        }),
                    )
                    .on_mouse_down(
                        MouseButton::Right,
                        cx.listener(|this, _: &MouseDownEvent, _, cx| {
                            this.close_group_context_menu();
                            cx.notify();
                        }),
                    )
                    .child(
                        div()
                            .id("group-ctx-menu")
                            .absolute()
                            .left(px(x))
                            .w(px(menu_w))
                            .p(px(5.0))
                            .flex()
                            .flex_col()
                            .gap(px(2.0))
                            .rounded(px(10.0))
                            .bg(theme.sheet_bg)
                            .border_1()
                            .border_color(theme.border_specular)
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(|_, _: &MouseDownEvent, _, cx| {
                                    cx.stop_propagation();
                                }),
                            )
                            .child(
                                div()
                                    .id("ctx-group-rename")
                                    .w_full()
                                    .h(px(28.0))
                                    .px(px(10.0))
                                    .rounded(px(6.5))
                                    .flex()
                                    .items_center()
                                    .gap(px(8.0))
                                    .cursor_pointer()
                                    .hover(|s| s.bg(theme.surface_active))
                                    .active(|s| s.opacity(0.75))
                                    .on_click(cx.listener(move |this, _, window, cx| {
                                        window.focus(&this.focus_handle);
                                        this.start_renaming_group(&rename_target);
                                        cx.notify();
                                    }))
                                    .child(icon_svg("icons/folder.svg", 13.0, theme.text_secondary))
                                    .child(
                                        div()
                                            .text_size(crate::theme::LayoutMetrics::font_control())
                                            .text_color(theme.text_primary)
                                            .child(tr("common.rename")),
                                    ),
                            )
                            .child(div().w_full().py(px(2.0)).px(px(6.0)).child(hairline_h(&theme)))
                            .child(
                                div()
                                    .id("ctx-group-delete")
                                    .w_full()
                                    .h(px(28.0))
                                    .px(px(10.0))
                                    .rounded(px(6.5))
                                    .flex()
                                    .items_center()
                                    .gap(px(8.0))
                                    .cursor_pointer()
                                    .hover(|s| s.bg(theme.surface_active))
                                    .active(|s| s.opacity(0.75))
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.request_delete_group(&delete_target);
                                        cx.notify();
                                    }))
                                    .child(icon_svg("icons/close.svg", 11.5, danger_color))
                                    .child(
                                        div()
                                            .text_size(crate::theme::LayoutMetrics::font_control())
                                            .text_color(danger_color)
                                            .child(tr("common.delete")),
                                    ),
                            )
                            .with_animation(
                                SharedString::from(format!("gctx-pop-{ctx_gen}")),
                                Animation::new(Duration::from_millis(145))
                                    .with_easing(ease_out_quint()),
                                move |el, d| el.top(px(y - 5.0 * (1.0 - d))).opacity(d),
                            ),
                    ),
            );
        }

        // 删除文件夹确认磨砂弹窗（明确提示所有主机将移至无文件夹状态）
        if let Some(group_to_del) = self.deleting_group_confirm.clone() {
            let m_gen = self.modal_anim_gen;
            let host_count = self.hosts.iter().filter(|h| h.group == group_to_del).count();
            let desc = if host_count > 0 {
                trf(
                    "sidebar.delete_group_with_hosts",
                    &[("group", &group_to_del), ("count", &host_count.to_string())],
                )
            } else {
                trf("sidebar.delete_group_empty", &[("group", &group_to_del)])
            };
            let danger_text: gpui::Hsla = if is_dark {
                gpui::Rgba {
                    r: 0.98,
                    g: 0.48,
                    b: 0.48,
                    a: 0.96,
                }
                .into()
            } else {
                gpui::Rgba {
                    r: 0.80,
                    g: 0.16,
                    b: 0.16,
                    a: 0.96,
                }
                .into()
            };

            root = root.child(
                div()
                    .id("delete-group-backdrop")
                    .absolute()
                    .inset_0()
                    .bg(theme.sheet_scrim)
                    .flex()
                    .items_center()
                    .justify_center()
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, _: &MouseDownEvent, _, cx| {
                            cx.stop_propagation();
                            this.cancel_delete_group();
                            cx.notify();
                        }),
                    )
                    .on_mouse_down(
                        MouseButton::Right,
                        cx.listener(|this, _: &MouseDownEvent, _, cx| {
                            cx.stop_propagation();
                            this.cancel_delete_group();
                            cx.notify();
                        }),
                    )
                    .on_click(cx.listener(|_, _, _, cx| {
                        cx.stop_propagation();
                    }))
                    .child(
                        div()
                            .id("delete-group-dialog")
                            .w(px(380.0))
                            .p(px(22.0))
                            .flex()
                            .flex_col()
                            .gap(px(14.0))
                            .rounded(crate::theme::LayoutMetrics::radius_window())
                            .bg(theme.sheet_bg)
                            .border_1()
                            .border_color(theme.border_specular)
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(|_, _: &MouseDownEvent, _, cx| {
                                    cx.stop_propagation();
                                }),
                            )
                            .child(
                                div()
                                    .text_size(crate::theme::LayoutMetrics::font_title())
                                    .text_color(theme.text_primary)
                                    .child(tr("sidebar.delete_group_title")),
                            )
                            .child(
                                div()
                                    .text_size(crate::theme::LayoutMetrics::font_control())
                                    .text_color(theme.text_secondary)
                                    .child(desc),
                            )
                            .child(
                                div()
                                    .w_full()
                                    .pt(px(6.0))
                                    .flex()
                                    .items_center()
                                    .justify_end()
                                    .gap(px(8.0))
                                    .child(
                                        crate::ui::components::glass_pill_button(
                                            tr("common.cancel"),
                                            false,
                                            &theme,
                                        )
                                            .id("btn-del-group-cancel")
                                            .on_click(cx.listener(|this, _, _, cx| {
                                                this.cancel_delete_group();
                                                cx.notify();
                                            })),
                                    )
                                    .child(
                                        crate::ui::components::glass_pill_button(
                                            tr("common.confirm_delete"),
                                            true,
                                            &theme,
                                        )
                                            .id("btn-del-group-confirm")
                                            .text_color(danger_text)
                                            .on_click(cx.listener(|this, _, _, cx| {
                                                this.confirm_delete_group();
                                                cx.notify();
                                            })),
                                    ),
                            )
                            .with_animation(
                                SharedString::from(format!("del-grp-sheet-{m_gen}")),
                                Animation::new(Duration::from_millis(210))
                                    .with_easing(ease_out_quint()),
                                |el, d| el.mt(px(12.0 * (1.0 - d))).opacity(d),
                            ),
                    )
                    .with_animation(
                        SharedString::from(format!("del-grp-bg-{m_gen}")),
                        Animation::new(Duration::from_millis(180)).with_easing(ease_out_quint()),
                        move |el, d| {
                            el.bg(gpui::Rgba {
                                r: 0.0,
                                g: 0.0,
                                b: 0.0,
                                a: 0.28 * d,
                            })
                        },
                    ),
            );
        }

        // SFTP 文件/文件夹右键菜单（下载 / 删除）
        if let Some(smenu) = self.sftp_context_menu.clone() {
            let menu_w = 152.0_f32;
            let menu_h = if smenu.is_dir { 42.0_f32 } else { 74.0_f32 };
            let raw_x: f32 = smenu.position.x.into();
            let raw_y: f32 = smenu.position.y.into();
            let x = raw_x.clamp(8.0, (vp_w - menu_w - 8.0).max(8.0));
            let y = raw_y.clamp(8.0, (vp_h - menu_h - 8.0).max(8.0));
            let dl_name = smenu.entry_name.clone();
            let del_name = smenu.entry_name.clone();
            let is_dir = smenu.is_dir;
            let ctx_gen = self.ctx_menu_anim_gen;
            let danger_color: gpui::Hsla = if is_dark {
                gpui::Rgba {
                    r: 0.96,
                    g: 0.44,
                    b: 0.44,
                    a: 0.92,
                }
                .into()
            } else {
                gpui::Rgba {
                    r: 0.78,
                    g: 0.18,
                    b: 0.18,
                    a: 0.92,
                }
                .into()
            };

            let mut menu_box = div()
                .id("sftp-ctx-menu")
                .absolute()
                .left(px(x))
                .w(px(menu_w))
                .p(px(5.0))
                .flex()
                .flex_col()
                .gap(px(2.0))
                .rounded(px(10.0))
                .bg(theme.sheet_bg)
                .border_1()
                .border_color(theme.border_specular)
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|_, _: &MouseDownEvent, _, cx| {
                        cx.stop_propagation();
                    }),
                );

            if !is_dir {
                menu_box = menu_box
                    .child(
                        div()
                            .id("ctx-sftp-download")
                            .w_full()
                            .h(px(28.0))
                            .px(px(10.0))
                            .rounded(px(6.5))
                            .flex()
                            .items_center()
                            .gap(px(8.0))
                            .cursor_pointer()
                            .hover(|s| s.bg(theme.surface_active))
                            .active(|s| s.opacity(0.75))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.sftp_download_entry(&dl_name);
                                cx.notify();
                            }))
                            .child(icon_svg("icons/download.svg", 12.5, theme.text_secondary))
                            .child(
                                div()
                                    .text_size(crate::theme::LayoutMetrics::font_control())
                                    .text_color(theme.text_primary)
                                    .child(tr("sftp.ctx_download")),
                            ),
                    )
                    .child(div().w_full().py(px(2.0)).px(px(6.0)).child(hairline_h(&theme)));
            }

            menu_box = menu_box.child(
                div()
                    .id("ctx-sftp-delete")
                    .w_full()
                    .h(px(28.0))
                    .px(px(10.0))
                    .rounded(px(6.5))
                    .flex()
                    .items_center()
                    .gap(px(8.0))
                    .cursor_pointer()
                    .hover(|s| s.bg(theme.surface_active))
                    .active(|s| s.opacity(0.75))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.sftp_delete_entry(&del_name, is_dir);
                        cx.notify();
                    }))
                    .child(icon_svg("icons/close.svg", 11.5, danger_color))
                    .child(
                        div()
                            .text_size(crate::theme::LayoutMetrics::font_control())
                            .text_color(danger_color)
                            .child(if is_dir {
                                tr("sftp.ctx_delete_dir")
                            } else {
                                tr("sftp.ctx_delete_file")
                            }),
                    ),
            );

            root = root.child(
                div()
                    .id("sftp-ctx-backdrop")
                    .absolute()
                    .inset_0()
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, _: &MouseDownEvent, _, cx| {
                            this.close_sftp_context_menu();
                            cx.notify();
                        }),
                    )
                    .on_mouse_down(
                        MouseButton::Right,
                        cx.listener(|this, _: &MouseDownEvent, _, cx| {
                            this.close_sftp_context_menu();
                            cx.notify();
                        }),
                    )
                    .child(menu_box.with_animation(
                        SharedString::from(format!("sftp-ctx-pop-{ctx_gen}")),
                        Animation::new(Duration::from_millis(145)).with_easing(ease_out_quint()),
                        move |el, d| el.top(px(y - 5.0 * (1.0 - d))).opacity(d),
                    )),
            );
        }

        // 终端内右键菜单（拖选后支持复制，输入行直接支持右键粘贴）
        if let Some(tmenu) = self.terminal_context_menu.clone() {
            let has_sel = self.has_terminal_selection();
            let menu_w = 148.0_f32;
            let menu_h = if has_sel { 104.0_f32 } else { 72.0_f32 };
            let raw_x: f32 = tmenu.position.x.into();
            let raw_y: f32 = tmenu.position.y.into();
            let x = raw_x.clamp(8.0, (vp_w - menu_w - 8.0).max(8.0));
            let y = raw_y.clamp(8.0, (vp_h - menu_h - 8.0).max(8.0));
            let paste_pane_id = tmenu.pane_id;
            let ctx_gen = self.ctx_menu_anim_gen;

            let mut menu_box = div()
                .id("term-ctx-menu")
                .absolute()
                .left(px(x))
                .w(px(menu_w))
                .p(px(5.0))
                .flex()
                .flex_col()
                .gap(px(2.0))
                .rounded(px(10.0))
                .bg(theme.sheet_bg)
                .border_1()
                .border_color(theme.border_specular)
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|_, _: &MouseDownEvent, _, cx| {
                        cx.stop_propagation();
                    }),
                );

            if has_sel {
                menu_box = menu_box
                    .child(
                        div()
                            .id("ctx-term-copy")
                            .w_full()
                            .h(px(28.0))
                            .px(px(10.0))
                            .rounded(px(6.5))
                            .flex()
                            .items_center()
                            .justify_between()
                            .cursor_pointer()
                            .hover(|s| s.bg(theme.surface_active))
                            .active(|s| s.opacity(0.75))
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.copy_terminal_selection(cx);
                                this.close_terminal_context_menu();
                                cx.notify();
                            }))
                            .child(
                                div()
                                    .text_size(crate::theme::LayoutMetrics::font_control())
                                    .text_color(theme.text_primary)
                                    .child(tr("common.copy")),
                            )
                            .child(
                                div()
                                    .text_size(crate::theme::LayoutMetrics::font_micro())
                                    .text_color(theme.text_tertiary)
                                    .child(self.settings.shortcuts.display_for(ShortcutAction::Copy)),
                            ),
                    )
                    .child(div().w_full().py(px(2.0)).px(px(6.0)).child(hairline_h(&theme)));
            }

            menu_box = menu_box.child(
                div()
                    .id("ctx-term-paste")
                    .w_full()
                    .h(px(28.0))
                    .px(px(10.0))
                    .rounded(px(6.5))
                    .flex()
                    .items_center()
                    .justify_between()
                    .cursor_pointer()
                    .hover(|s| s.bg(theme.surface_active))
                    .active(|s| s.opacity(0.75))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        window.focus(&this.focus_handle);
                        this.paste_to_terminal(&paste_pane_id, cx);
                        this.close_terminal_context_menu();
                        cx.notify();
                    }))
                    .child(
                        div()
                            .text_size(crate::theme::LayoutMetrics::font_control())
                            .text_color(theme.text_primary)
                            .child(tr("common.paste")),
                    )
                    .child(
                        div()
                            .text_size(crate::theme::LayoutMetrics::font_micro())
                            .text_color(theme.text_tertiary)
                            .child(self.settings.shortcuts.display_for(ShortcutAction::Paste)),
                    ),
            )
            .child(div().w_full().py(px(2.0)).px(px(6.0)).child(hairline_h(&theme)))
            .child(
                div()
                    .id("ctx-term-find")
                    .w_full()
                    .h(px(28.0))
                    .px(px(10.0))
                    .rounded(px(6.5))
                    .flex()
                    .items_center()
                    .justify_between()
                    .cursor_pointer()
                    .hover(|s| s.bg(theme.surface_active))
                    .active(|s| s.opacity(0.75))
                    .on_click(cx.listener(|this, _, window, cx| {
                        window.focus(&this.focus_handle);
                        this.close_terminal_context_menu();
                        this.open_terminal_search();
                        cx.notify();
                    }))
                    .child(
                        div()
                            .text_size(crate::theme::LayoutMetrics::font_control())
                            .text_color(theme.text_primary)
                            .child(tr("menu.term_find")),
                    )
                    .child(
                        div()
                            .text_size(crate::theme::LayoutMetrics::font_micro())
                            .text_color(theme.text_tertiary)
                            .child(
                                self.settings
                                    .shortcuts
                                    .display_for(ShortcutAction::FindInTerminal),
                            ),
                    ),
            );

            root = root.child(
                div()
                    .id("term-ctx-backdrop")
                    .absolute()
                    .inset_0()
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, _: &MouseDownEvent, _, cx| {
                            this.close_terminal_context_menu();
                            cx.notify();
                        }),
                    )
                    .on_mouse_down(
                        MouseButton::Right,
                        cx.listener(|this, _: &MouseDownEvent, _, cx| {
                            this.close_terminal_context_menu();
                            cx.notify();
                        }),
                    )
                    .child(menu_box.with_animation(
                        SharedString::from(format!("term-ctx-pop-{ctx_gen}")),
                        Animation::new(Duration::from_millis(145)).with_easing(ease_out_quint()),
                        move |el, d| el.top(px(y - 5.0 * (1.0 - d))).opacity(d),
                    )),
            );
        }

        // 长按拖拽主机时的精准行间细横条指示线 + 跟随光标右下角的磨砂预览胶囊（不遮挡光标尖与横条）
        if let Some(drag) = self.host_drag.as_ref().filter(|d| d.active) {
            if let Some(line) = drag.insertion_line {
                root = root.child(
                    div()
                        .absolute()
                        .left(px(line.x))
                        .top(px(line.y - 2.5))
                        .w(px(line.width))
                        .h(px(5.0))
                        .px(px(2.0))
                        .flex()
                        .items_center()
                        .child(
                            div()
                                .flex_none()
                                .w(px(5.0))
                                .h(px(5.0))
                                .rounded(px(99.0))
                                .bg(theme.sidebar_text_primary),
                        )
                        .child(
                            div()
                                .flex_1()
                                .h(px(1.5))
                                .rounded(px(1.0))
                                .bg(theme.sidebar_text_primary),
                        ),
                );
            }

            let gx: f32 = drag.current_pos.x.into();
            let gy: f32 = drag.current_pos.y.into();
            let ghost_w = 216.0_f32;
            let left_x = (gx + 14.0).clamp(8.0, (vp_w - ghost_w - 8.0).max(8.0));
            let top_y = (gy + 14.0).clamp(8.0, (vp_h - 42.0).max(8.0));
            root = root.child(
                div()
                    .absolute()
                    .left(px(left_x))
                    .top(px(top_y))
                    .w(px(ghost_w))
                    .h(px(32.0))
                    .px(px(10.0))
                    .rounded(crate::theme::LayoutMetrics::radius_row())
                    .bg(theme.sheet_bg)
                    .border_1()
                    .border_color(theme.border_specular)
                    .opacity(0.94)
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap(px(8.0))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .items_center()
                            .gap(px(7.0))
                            .overflow_hidden()
                            .child(
                                div()
                                    .flex_none()
                                    .w(px(6.5))
                                    .h(px(6.5))
                                    .rounded(px(99.0))
                                    .bg(if drag.connected {
                                        theme.status_connected
                                    } else {
                                        theme.status_idle
                                    }),
                            )
                            .child(
                                div()
                                    .truncate()
                                    .text_size(crate::theme::LayoutMetrics::font_body())
                                    .text_color(theme.text_primary)
                                    .child(drag.host_name.clone()),
                            ),
                    )
                    .child(
                        div()
                            .flex_none()
                            .text_size(crate::theme::LayoutMetrics::font_micro())
                            .text_color(theme.text_tertiary)
                            .child(drag.host_ip.clone()),
                    ),
            );
        }

        // 密钥库首次设置主密码 / 解锁磨砂弹窗
        if let Some(vmodal) = self.vault_modal.clone() {
            let m_gen = self.modal_anim_gen;
            root = root.child(
                div()
                    .id("vault-modal-backdrop")
                    .absolute()
                    .inset_0()
                    .flex()
                    .items_center()
                    .justify_center()
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, _: &MouseDownEvent, _, cx| {
                            this.close_vault_modal();
                            cx.notify();
                        }),
                    )
                    .child(
                        div()
                            .child(render_vault_modal(&vmodal, self.active_input, &theme, cx))
                            .with_animation(
                                SharedString::from(format!("vault-sheet-{m_gen}")),
                                Animation::new(Duration::from_millis(220))
                                    .with_easing(ease_out_quint()),
                                |el, d| el.mt(px(14.0 * (1.0 - d))).opacity(d),
                            ),
                    )
                    .with_animation(
                        SharedString::from(format!("vault-bg-{m_gen}")),
                        Animation::new(Duration::from_millis(180)).with_easing(ease_out_quint()),
                        move |el, d| {
                            el.bg(gpui::Rgba {
                                r: 0.0,
                                g: 0.0,
                                b: 0.0,
                                a: 0.28 * d,
                            })
                        },
                    ),
            );
        }

        // 全局同步冲突 / 首次绑定决策弹窗（挂接于根视图最顶层，首页/终端/SFTP/设置任意页面均直接弹出）
        if let Some(conflict_state) = self.sync_conflict_modal.clone() {
            let m_gen = self.modal_anim_gen;
            let scrim_alpha = if is_dark { 0.44 } else { 0.24 };
            root = root.child(
                div()
                    .id("sync-conflict-modal-backdrop")
                    .absolute()
                    .inset_0()
                    .flex()
                    .items_center()
                    .justify_center()
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|_, _: &MouseDownEvent, _, cx| {
                            // 点击背景遮罩不误关，避免错过跨设备冲突决策
                            cx.stop_propagation();
                        }),
                    )
                    .child(
                        div()
                            .child(render_sync_conflict_modal(
                                &conflict_state,
                                self.active_input,
                                &theme,
                                cx,
                            ))
                            .with_animation(
                                SharedString::from(format!("sync-conflict-sheet-{m_gen}")),
                                Animation::new(Duration::from_millis(220))
                                    .with_easing(ease_out_quint()),
                                |el, d| el.mt(px(14.0 * (1.0 - d))).opacity(d),
                            ),
                    )
                    .with_animation(
                        SharedString::from(format!("sync-conflict-bg-{m_gen}")),
                        Animation::new(Duration::from_millis(180)).with_easing(ease_out_quint()),
                        move |el, d| {
                            el.bg(gpui::Rgba {
                                r: 0.0,
                                g: 0.0,
                                b: 0.0,
                                a: scrim_alpha * d,
                            })
                        },
                    ),
            );
        }

        if self.show_new_host_modal {
            let m_gen = self.modal_anim_gen;
            root = root.child(
                div()
                    .id("new-host-modal-backdrop")
                    .absolute()
                    .inset_0()
                    .flex()
                    .items_center()
                    .justify_center()
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, _: &MouseDownEvent, _, cx| {
                            if this.active_dropdown.is_some() {
                                this.close_dropdown();
                            } else {
                                this.close_new_host_modal();
                            }
                            cx.notify();
                        }),
                    )
                    .child(
                        div()
                            .child({
                                let jump_label = self.selected_jump_host_label();
                                render_new_host_modal(
                                    &self.new_host_draft,
                                    jump_label.as_deref(),
                                    self.editing_host_id.is_some(),
                                    self.active_input,
                                    &self.dropdown_anchors,
                                    &theme,
                                    cx,
                                )
                            })
                            .with_animation(
                                SharedString::from(format!("modal-sheet-{m_gen}")),
                                Animation::new(Duration::from_millis(220))
                                    .with_easing(ease_out_quint()),
                                |el, d| el.mt(px(14.0 * (1.0 - d))).opacity(d),
                            ),
                    )
                    .with_animation(
                        SharedString::from(format!("modal-bg-{m_gen}")),
                        Animation::new(Duration::from_millis(180)).with_easing(ease_out_quint()),
                        move |el, d| {
                            el.bg(gpui::Rgba {
                                r: 0.0,
                                g: 0.0,
                                b: 0.0,
                                a: 0.28 * d,
                            })
                        },
                    ),
            );
        }

        // 顶层悬浮磨砂下拉菜单（150ms 丝滑弹入，永不被滚动容器或边框裁切）
        if let Some(dd) = self.active_dropdown.clone() {
            let dd_gen = self.dropdown_anim_gen;
            let options = self.dropdown_options(dd.kind);
            let item_h = 28.0_f32;
            let anchor_bounds = self.dropdown_anchors.borrow().get(&dd.kind).copied();
            let menu_w = anchor_bounds
                .map(|b| f32::from(b.size.width).max(dd.width))
                .unwrap_or_else(|| dd.width.max(128.0));
            let menu_h = (options.len() as f32) * (item_h + 2.0) + 10.0;
            let (x, y) = if let Some(anchor) = anchor_bounds {
                let ax: f32 = anchor.origin.x.into();
                let ay_bottom: f32 = anchor.bottom().into();
                let ay_top: f32 = anchor.top().into();
                let x = ax.clamp(12.0, (vp_w - menu_w - 12.0).max(12.0));
                let y = if ay_bottom + 4.0 + menu_h > vp_h - 12.0 {
                    (ay_top - menu_h - 4.0).max(12.0)
                } else {
                    ay_bottom + 4.0
                };
                (x, y)
            } else {
                let raw_x: f32 = dd.position.x.into();
                let raw_y: f32 = dd.position.y.into();
                let x = (raw_x - menu_w * 0.5).clamp(12.0, (vp_w - menu_w - 12.0).max(12.0));
                let y = if raw_y + 18.0 + menu_h > vp_h - 12.0 {
                    (raw_y - menu_h - 14.0).max(12.0)
                } else {
                    raw_y + 18.0
                };
                (x, y)
            };

            let mut menu_box = div()
                .id("global-dropdown-menu")
                .absolute()
                .left(px(x))
                .w(px(menu_w))
                .p(px(4.0))
                .flex()
                .flex_col()
                .gap(px(2.0))
                .rounded(px(10.0))
                .bg(theme.sheet_bg)
                .border_1()
                .border_color(theme.border_specular)
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|_, _: &MouseDownEvent, _, cx| {
                        cx.stop_propagation();
                    }),
                );

            for (idx, (label, is_selected)) in options.into_iter().enumerate() {
                let kind = dd.kind;
                let right_indicator: gpui::AnyElement = if kind == DropdownKind::TerminalFont {
                    if let Some(spec) = FONT_CATALOG.get(idx) {
                        if let Some(&progress) = self.font_downloading.get(spec.id) {
                            circular_progress_ring(progress, 13.5, &theme).into_any_element()
                        } else if !spec.is_system && !fonts::is_font_downloaded(spec) {
                            icon_svg("icons/cloud-download.svg", 13.5, theme.text_tertiary)
                                .into_any_element()
                        } else if is_selected {
                            div()
                                .text_size(crate::theme::LayoutMetrics::font_caption())
                                .text_color(theme.text_primary)
                                .child("✓")
                                .into_any_element()
                        } else {
                            div().into_any_element()
                        }
                    } else {
                        div().into_any_element()
                    }
                } else {
                    div()
                        .text_size(crate::theme::LayoutMetrics::font_caption())
                        .text_color(theme.text_primary)
                        .child(if is_selected { "✓" } else { "" })
                        .into_any_element()
                };

                menu_box = menu_box.child(
                    div()
                        .id(("dd-option", idx))
                        .w_full()
                        .h(px(item_h))
                        .px(px(10.0))
                        .rounded(px(6.5))
                        .flex()
                        .items_center()
                        .justify_between()
                        .gap(px(8.0))
                        .cursor_pointer()
                        .bg(if is_selected {
                            theme.surface_pill
                        } else {
                            gpui::transparent_black()
                        })
                        .hover(|s| s.bg(theme.surface_active))
                        .active(|s| s.opacity(0.76))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.select_dropdown_index(kind, idx);
                            cx.notify();
                        }))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .text_size(crate::theme::LayoutMetrics::font_control())
                                .text_color(if is_selected {
                                    theme.text_primary
                                } else {
                                    theme.text_secondary
                                })
                                .child(label),
                        )
                        .child(right_indicator),
                );
            }

            let animated_menu = menu_box.with_animation(
                SharedString::from(format!("dd-pop-{dd_gen}")),
                Animation::new(Duration::from_millis(150)).with_easing(ease_out_quint()),
                move |el, d| el.top(px(y - 6.0 * (1.0 - d))).opacity(d),
            );

            root = root.child(
                div()
                    .id("global-dropdown-backdrop")
                    .absolute()
                    .inset_0()
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, _: &MouseDownEvent, _, cx| {
                            this.close_dropdown();
                            cx.notify();
                        }),
                    )
                    .on_mouse_down(
                        MouseButton::Right,
                        cx.listener(|this, _: &MouseDownEvent, _, cx| {
                            this.close_dropdown();
                            cx.notify();
                        }),
                    )
                    .child(animated_menu),
            );
        }

        // 全局顶部居中 Toast 通知叠层（位于全部页面与弹窗之上，多条叠加、各自定时消失且支持手动关闭）
        if !self.toasts.is_empty() {
            root = root.child(render_toast_stack(&self.toasts, is_dark, &theme, cx));
        }

        root
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes_macos_option_meta_and_shift_enter_keys() {
        // macOS Option+b 物理键名是 "b"，但 key_char 会变成 "∫"，必须编码为 \x1bb
        let opt_b = gpui::Keystroke {
            modifiers: gpui::Modifiers {
                alt: true,
                ..Default::default()
            },
            key: "b".into(),
            key_char: Some("∫".into()),
        };
        assert_eq!(
            encode_terminal_special_keystroke(&opt_b, TermMode::empty(), false, false),
            Some(vec![0x1b, b'b'])
        );

        let opt_f = gpui::Keystroke {
            modifiers: gpui::Modifiers {
                alt: true,
                ..Default::default()
            },
            key: "f".into(),
            key_char: Some("ƒ".into()),
        };
        assert_eq!(
            encode_terminal_special_keystroke(&opt_f, TermMode::empty(), false, false),
            Some(vec![0x1b, b'f'])
        );

        // Shift+Enter：传统终端发 \x1b\r，Kitty 增强键盘协议发 \x1b[13;2u
        let shift_enter = gpui::Keystroke {
            modifiers: gpui::Modifiers {
                shift: true,
                ..Default::default()
            },
            key: "enter".into(),
            key_char: None,
        };
        assert_eq!(
            encode_terminal_special_keystroke(&shift_enter, TermMode::empty(), false, false),
            Some(b"\x1b\r".to_vec())
        );
        assert_eq!(
            encode_terminal_special_keystroke(
                &shift_enter,
                TermMode::DISAMBIGUATE_ESC_CODES,
                false,
                false
            ),
            Some(b"\x1b[13;2u".to_vec())
        );
    }

    #[test]
    fn escapes_posix_paths_safely() {
        assert_eq!(shell_escape_posix_path("/tmp/a.log"), "/tmp/a.log");
        assert_eq!(
            shell_escape_posix_path("/Users/me/My File's.txt"),
            "'/Users/me/My File'\\''s.txt'"
        );
    }
}
