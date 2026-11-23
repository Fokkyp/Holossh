//! 主工作区视图（严格对应 Figma 01 / 03 / 04 / 05 / 08 画板）
//!
//! 整窗一体化布局：264px 左侧磨砂侧栏 + 1px 极细分割线 + 右侧统一标题栏与无底栏终端/SFTP区域（或未选主机初始空状态）。

pub mod empty_state;
pub mod sftp_panel;
pub mod sidebar;
pub mod terminal_element;
pub mod terminal_pane;
pub mod titlebar;
