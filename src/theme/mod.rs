//! 主题系统模块
//!
//! 严格 1:1 映射 Figma 文件 `HoloSSH / Theme` 变量集合（Dark / Light 双模式）。

pub mod tokens;

pub use tokens::{AppearanceMode, LayoutMetrics, ThemeTokens};
