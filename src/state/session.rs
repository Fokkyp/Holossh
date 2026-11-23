//! 会话标签页与递归分屏终端窗格树（对应 01/03/05 画板）

/// 分屏方向
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SplitAxis {
    Horizontal,
    Vertical,
}

/// 单个纯 SSH 终端窗格元数据（标题统一采用 `主机名 · 当前目录`）
#[derive(Debug, Clone)]
pub struct TerminalPaneState {
    pub pane_id: String,
    pub host_id: String,
    pub host_name: String,
    pub current_dir: String,
    pub connected: bool,
}

impl TerminalPaneState {
    pub fn title(&self) -> String {
        format!("{} · {}", self.host_name, self.current_dir)
    }
}

/// 递归分屏布局树（对应 05 分屏终端视图，支持中线拖拽调整比例）
#[derive(Debug, Clone)]
pub enum SplitNode {
    Leaf(TerminalPaneState),
    Split {
        axis: SplitAxis,
        ratio: f32,
        first: Box<SplitNode>,
        second: Box<SplitNode>,
    },
}

impl SplitNode {
    pub fn collect_pane_ids(&self, out: &mut Vec<String>) {
        match self {
            Self::Leaf(pane) => out.push(pane.pane_id.clone()),
            Self::Split { first, second, .. } => {
                first.collect_pane_ids(out);
                second.collect_pane_ids(out);
            }
        }
    }

    pub fn update_pane(&mut self, pane_id: &str, f: &mut impl FnMut(&mut TerminalPaneState)) {
        match self {
            Self::Leaf(pane) => {
                if pane.pane_id == pane_id {
                    f(pane);
                }
            }
            Self::Split { first, second, .. } => {
                first.update_pane(pane_id, f);
                second.update_pane(pane_id, f);
            }
        }
    }

    pub fn find_pane(&self, pane_id: &str) -> Option<&TerminalPaneState> {
        match self {
            Self::Leaf(pane) if pane.pane_id == pane_id => Some(pane),
            Self::Leaf(_) => None,
            Self::Split { first, second, .. } => {
                first.find_pane(pane_id).or_else(|| second.find_pane(pane_id))
            }
        }
    }

    /// 在指定 `target_id` 窗格处按 `axis` 拆分出新窗格
    pub fn split_pane(
        &mut self,
        target_id: &str,
        axis: SplitAxis,
        new_pane: TerminalPaneState,
    ) -> bool {
        match self {
            Self::Leaf(pane) if pane.pane_id == target_id => {
                let existing = pane.clone();
                *self = Self::Split {
                    axis,
                    ratio: 0.5,
                    first: Box::new(Self::Leaf(existing)),
                    second: Box::new(Self::Leaf(new_pane)),
                };
                true
            }
            Self::Leaf(_) => false,
            Self::Split { first, second, .. } => {
                first.split_pane(target_id, axis, new_pane.clone())
                    || second.split_pane(target_id, axis, new_pane)
            }
        }
    }

    /// 关闭指定窗格并提升兄弟节点
    pub fn close_pane(&mut self, target_id: &str) -> bool {
        match self {
            Self::Leaf(_) => false,
            Self::Split { first, second, .. } => {
                if matches!(first.as_ref(), Self::Leaf(p) if p.pane_id == target_id) {
                    *self = second.as_ref().clone();
                    return true;
                }
                if matches!(second.as_ref(), Self::Leaf(p) if p.pane_id == target_id) {
                    *self = first.as_ref().clone();
                    return true;
                }
                first.close_pane(target_id) || second.close_pane(target_id)
            }
        }
    }

    pub fn pane_count(&self) -> usize {
        match self {
            Self::Leaf(_) => 1,
            Self::Split { first, second, .. } => first.pane_count() + second.pane_count(),
        }
    }

    pub fn ratio_at_path(&self, path: &str) -> Option<f32> {
        match self {
            Self::Leaf(_) => None,
            Self::Split {
                ratio,
                first,
                second,
                ..
            } => {
                let mut chars = path.chars();
                match chars.next() {
                    None => Some(*ratio),
                    Some('0') => first.ratio_at_path(chars.as_str()),
                    Some('1') => second.ratio_at_path(chars.as_str()),
                    _ => None,
                }
            }
        }
    }

    pub fn update_host_name(&mut self, host_id: &str, new_name: &str) {
        match self {
            Self::Leaf(pane) => {
                if pane.host_id == host_id {
                    pane.host_name = new_name.to_string();
                }
            }
            Self::Split { first, second, .. } => {
                first.update_host_name(host_id, new_name);
                second.update_host_name(host_id, new_name);
            }
        }
    }

    /// 计算路径节点沿目标方向占父工作区的比例（保证嵌套分屏中线拖拽 1:1 贴手）
    pub fn span_fraction_at_path(&self, path: &str, target_axis: SplitAxis) -> f32 {
        match self {
            Self::Leaf(_) => 1.0,
            Self::Split {
                axis,
                ratio,
                first,
                second,
            } => {
                let mut chars = path.chars();
                match chars.next() {
                    None => 1.0,
                    Some('0') => {
                        let factor = if *axis == target_axis {
                            ratio.clamp(0.20, 0.80)
                        } else {
                            1.0
                        };
                        factor * first.span_fraction_at_path(chars.as_str(), target_axis)
                    }
                    Some('1') => {
                        let factor = if *axis == target_axis {
                            1.0 - ratio.clamp(0.20, 0.80)
                        } else {
                            1.0
                        };
                        factor * second.span_fraction_at_path(chars.as_str(), target_axis)
                    }
                    _ => 1.0,
                }
            }
        }
    }

    pub fn set_ratio_at_path(&mut self, path: &str, new_ratio: f32) -> bool {
        match self {
            Self::Leaf(_) => false,
            Self::Split {
                ratio,
                first,
                second,
                ..
            } => {
                let mut chars = path.chars();
                match chars.next() {
                    None => {
                        *ratio = new_ratio.clamp(0.20, 0.80);
                        true
                    }
                    Some('0') => first.set_ratio_at_path(chars.as_str(), new_ratio),
                    Some('1') => second.set_ratio_at_path(chars.as_str(), new_ratio),
                    _ => false,
                }
            }
        }
    }
}

/// 顶部单个会话标签页
#[derive(Debug, Clone)]
pub struct SessionTab {
    pub id: String,
    pub host_id: String,
    pub title: String,
    pub connected: bool,
    pub has_bell: bool,
    pub root_pane: SplitNode,
    pub focused_pane_id: String,
    pub split_active: bool,
    pub sftp_open: bool,
}

impl SessionTab {
    pub fn new_for_host(tab_id: String, host_id: String, host_name: String) -> Self {
        let pane_id = format!("{tab_id}-pane-1");
        let pane = TerminalPaneState {
            pane_id: pane_id.clone(),
            host_id: host_id.clone(),
            host_name: host_name.clone(),
            current_dir: "~".into(),
            connected: false,
        };
        Self {
            id: tab_id,
            host_id,
            title: host_name,
            connected: false,
            has_bell: false,
            root_pane: SplitNode::Leaf(pane),
            focused_pane_id: pane_id,
            split_active: false,
            sftp_open: false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_and_close_panes_preserves_tree() {
        let mut tab = SessionTab::new_for_host("t1".into(), "h1".into(), "demo".into());
        assert_eq!(tab.root_pane.pane_count(), 1);
        let p2 = TerminalPaneState {
            pane_id: "t1-pane-2".into(),
            host_id: "h1".into(),
            host_name: "demo".into(),
            current_dir: "~".into(),
            connected: false,
        };
        assert!(tab.root_pane.split_pane("t1-pane-1", SplitAxis::Horizontal, p2));
        assert_eq!(tab.root_pane.pane_count(), 2);
        assert!(tab.root_pane.set_ratio_at_path("", 0.65));
        assert!((tab.root_pane.ratio_at_path("").unwrap() - 0.65).abs() < 0.001);

        let p3 = TerminalPaneState {
            pane_id: "t1-pane-3".into(),
            host_id: "h1".into(),
            host_name: "demo".into(),
            current_dir: "~".into(),
            connected: false,
        };
        assert!(tab.root_pane.split_pane("t1-pane-2", SplitAxis::Vertical, p3));
        assert_eq!(tab.root_pane.pane_count(), 3);

        assert!(tab.root_pane.close_pane("t1-pane-2"));
        assert_eq!(tab.root_pane.pane_count(), 2);
        assert!(tab.root_pane.find_pane("t1-pane-3").is_some());
        assert!(tab.root_pane.close_pane("t1-pane-3"));
        assert_eq!(tab.root_pane.pane_count(), 1);
    }
}
