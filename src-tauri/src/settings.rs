//! 应用设置：持久化到 app_data_dir/settings.json。

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Settings {
    /// 同步目录（Syncthing/坚果云等同步盘里的一个文件夹）
    pub remote_dir: String,
    /// 自动同步间隔（分钟；0 = 关闭自动同步）
    pub auto_sync_interval_min: u32,
    /// 同步后弹系统通知
    pub notify_on_sync: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            remote_dir: String::new(),
            auto_sync_interval_min: 0,
            notify_on_sync: true,
        }
    }
}

impl Settings {
    pub fn load(path: &Path) -> Self {
        std::fs::read(path)
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default()
    }

    pub fn save(&self, path: &Path) -> Result<(), String> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        let bytes = serde_json::to_vec_pretty(self).map_err(|e| e.to_string())?;
        std::fs::write(path, bytes).map_err(|e| e.to_string())
    }
}

/// fcitx5 用户数据目录（XDG_DATA_HOME 兼容）。
pub fn fcitx5_data_dir() -> PathBuf {
    let data_home = std::env::var("XDG_DATA_HOME")
        .ok()
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
            PathBuf::from(home).join(".local/share")
        });
    data_home.join("fcitx5/pinyin")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        let s = Settings {
            remote_dir: "/mnt/sync/fcitx5".into(),
            auto_sync_interval_min: 30,
            notify_on_sync: false,
        };
        s.save(&path).unwrap();
        assert_eq!(Settings::load(&path), s);
    }

    #[test]
    fn load_missing_file_gives_defaults() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(Settings::load(&dir.path().join("none.json")), Settings::default());
    }
}
