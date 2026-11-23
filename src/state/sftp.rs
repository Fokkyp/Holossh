//! SFTP 文件面板状态（对应 04 画板：无搜索框、无文件图标、名称/大小/修改时间/权限四列、底部进度条）

/// 远程目录单条文件/目录记录
#[derive(Debug, Clone)]
pub struct SftpEntry {
    pub name: String,
    pub is_dir: bool,
    pub size_display: String,
    pub modified_display: String,
    pub permissions: String,
    pub is_hidden: bool,
}

/// 传输方向（上传 / 下载）
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransferDirection {
    Upload,
    Download,
}

/// 底部实时传输任务（支持多文件并发独立跟踪与取消）
#[derive(Debug, Clone)]
pub struct TransferTask {
    pub id: u64,
    pub direction: TransferDirection,
    pub file_name: String,
    pub transferred_mb: f32,
    pub total_mb: f32,
    pub progress: f32,
    pub done: bool,
}

/// 当前打开的 SFTP 面板状态
#[derive(Debug, Clone)]
pub struct SftpPanelState {
    pub current_path: String,
    pub path_segments: Vec<String>,
    pub entries: Vec<SftpEntry>,
    pub selected_name: Option<String>,
    pub transfers: Vec<TransferTask>,
    pub transfers_expanded: bool,
    pub loading: bool,
}

impl SftpPanelState {
    pub fn upsert_transfer(&mut self, task: TransferTask) {
        if let Some(existing) = self.transfers.iter_mut().find(|t| t.id == task.id) {
            *existing = task;
        } else {
            self.transfers.push(task);
        }
        if self.transfers.len() <= 1 {
            self.transfers_expanded = false;
        }
    }

    pub fn remove_transfer(&mut self, id: u64) {
        self.transfers.retain(|t| t.id != id);
        if self.transfers.len() <= 1 {
            self.transfers_expanded = false;
        }
    }

    pub fn clear_transfers(&mut self) {
        self.transfers.clear();
        self.transfers_expanded = false;
    }

    pub fn set_remote_path(&mut self, path: &str) {
        let normalized = if path.is_empty() { "/" } else { path };
        self.current_path = normalized.to_string();
        let mut segs = vec!["/".to_string()];
        for part in normalized.split('/').filter(|p| !p.is_empty()) {
            segs.push(part.to_string());
        }
        self.path_segments = segs;
    }

    pub fn path_for_crumb(&self, idx: usize) -> String {
        if idx == 0 {
            if self.path_segments.first().map(|s| s.as_str()) == Some("~") {
                return "~".to_string();
            }
            return "/".to_string();
        }
        let joined = self.path_segments[1..=idx.min(self.path_segments.len() - 1)].join("/");
        format!("/{joined}")
    }

    pub fn join_child(&self, name: &str) -> String {
        let clean = name.trim_end_matches('/');
        if clean == ".." {
            if self.path_segments.len() <= 1 {
                return "/".to_string();
            }
            return self.path_for_crumb(self.path_segments.len() - 2);
        }
        if self.current_path == "/" {
            format!("/{clean}")
        } else {
            format!("{}/{clean}", self.current_path.trim_end_matches('/'))
        }
    }
}

impl Default for SftpPanelState {
    fn default() -> Self {
        Self {
            current_path: "~".into(),
            path_segments: vec!["~".into()],
            entries: Vec::new(),
            selected_name: None,
            transfers: Vec::new(),
            transfers_expanded: false,
            loading: false,
        }
    }
}
