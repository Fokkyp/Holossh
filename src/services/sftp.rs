//! SFTP 元数据格式化工具（权限、大小、本地化修改时间）

use crate::i18n::{tr, trf};
use chrono::{Local, TimeZone};

/// 将 Unix st_mode 权限位格式化为标准 10 字符权限串（如 `drwxr-xr-x` / `-rw-r--r--`）
pub fn format_unix_permissions(mode: Option<u32>, is_dir: bool) -> String {
    let bits = mode.unwrap_or(if is_dir { 0o040755 } else { 0o100644 });
    let file_type = if is_dir || (bits & 0o170000) == 0o040000 {
        'd'
    } else if (bits & 0o170000) == 0o120000 {
        'l'
    } else {
        '-'
    };

    let mut out = String::with_capacity(10);
    out.push(file_type);
    for shift in [6, 3, 0] {
        let triad = (bits >> shift) & 0o7;
        out.push(if triad & 4 != 0 { 'r' } else { '-' });
        out.push(if triad & 2 != 0 { 'w' } else { '-' });
        out.push(if triad & 1 != 0 { 'x' } else { '-' });
    }
    out
}

/// 格式化文件体积
pub fn format_file_size(bytes: u64, is_dir: bool) -> String {
    if is_dir {
        return tr("sftp.dir_label").to_string();
    }
    const KB: f64 = 1024.0;
    const MB: f64 = 1024.0 * 1024.0;
    const GB: f64 = 1024.0 * 1024.0 * 1024.0;
    let b = bytes as f64;
    if b >= GB {
        format!("{:.1} GB", b / GB)
    } else if b >= MB {
        format!("{:.1} MB", b / MB)
    } else if b >= KB {
        format!("{:.1} KB", b / KB)
    } else {
        format!("{bytes} B")
    }
}

/// 格式化远程文件修改时间（今天/昨天/M月D日）
pub fn format_mtime(mtime: Option<u32>) -> String {
    let Some(ts) = mtime else {
        return "—".to_string();
    };
    let Some(dt) = Local.timestamp_opt(ts as i64, 0).single() else {
        return "—".to_string();
    };
    let now = Local::now();
    let days_diff = (now.date_naive() - dt.date_naive()).num_days();
    match days_diff {
        0 => trf("sftp.time_today", &[("time", &dt.format("%H:%M").to_string())]),
        1 => trf("sftp.time_yesterday", &[("time", &dt.format("%H:%M").to_string())]),
        _ => trf(
            "sftp.time_month_day",
            &[
                ("month", &dt.format("%-m").to_string()),
                ("day", &dt.format("%-d").to_string()),
            ],
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_unix_permissions_and_sizes() {
        assert_eq!(format_unix_permissions(Some(0o040755), true), "drwxr-xr-x");
        assert_eq!(format_unix_permissions(Some(0o100644), false), "-rw-r--r--");
        assert_eq!(format_unix_permissions(Some(0o100600), false), "-rw-------");
        assert_eq!(format_file_size(618, false), "618 B");
        assert_eq!(format_file_size(2458, false), "2.4 KB");
        assert_eq!(format_file_size(50_960_793, false), "48.6 MB");
    }
}
