//! 同步引擎的落盘安全层。
//!
//! fcitx5 写用户数据走 safeSave（临时文件 + rename 原子替换），本模块对
//! 同步工具的写回采用同样的原子语义，并在覆盖前把现文件备份到带时间戳的
//! 备份目录——保证任何时候都能一键回滚。

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug, thiserror::Error)]
pub enum EngineError {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

/// fcitx5 是否正在运行（pgrep -x fcitx5）。
pub fn fcitx5_running() -> bool {
    std::process::Command::new("pgrep")
        .args(["-x", "fcitx5"])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// 时间戳字符串，用于备份文件命名（UTC，精确到秒）。
fn timestamp() -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    format!("{}", now.as_secs())
}

/// 把 `new_bytes` 安全写入 `target`：
/// 1. 现文件复制到 `backup_dir/<文件名>.<时间戳>.bak`
/// 2. 同目录写临时文件 + fsync + rename 原子替换
/// 返回备份文件路径（目标不存在时返回 None）。
pub fn backup_and_apply(
    target: &Path,
    new_bytes: &[u8],
    backup_dir: &Path,
) -> Result<Option<PathBuf>, EngineError> {
    std::fs::create_dir_all(backup_dir)?;
    let backup = if target.exists() {
        let name = target
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "unknown".into());
        let backup_path = backup_dir.join(format!("{name}.{}.bak", timestamp()));
        std::fs::copy(target, &backup_path)?;
        Some(backup_path)
    } else {
        None
    };

    let parent = target.parent().unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(parent)?;
    let tmp = parent.join(format!(
        ".{}.tmp{}",
        target
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "unknown".into()),
        std::process::id()
    ));
    {
        use std::io::Write;
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(new_bytes)?;
        f.sync_all()?;
    }
    std::fs::rename(&tmp, target)?;
    Ok(backup)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn apply_creates_backup_and_atomic_replacement() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("user.dict");
        std::fs::write(&target, b"v1").unwrap();
        let backup_dir = dir.path().join("backups");

        let backup = backup_and_apply(&target, b"v2", &backup_dir).unwrap();
        let backup = backup.expect("existing file must be backed up");
        assert!(backup.starts_with(&backup_dir));
        assert!(backup.to_string_lossy().ends_with(".bak"));
        assert_eq!(std::fs::read(&backup).unwrap(), b"v1");
        assert_eq!(std::fs::read(&target).unwrap(), b"v2");
        // 临时文件不能残留
        let leftovers: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().contains(".tmp"))
            .collect();
        assert!(leftovers.is_empty(), "temp files must not be left behind");
    }

    #[test]
    fn apply_to_new_file_has_no_backup() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("fresh.dict");
        let backup = backup_and_apply(&target, b"data", &dir.path().join("bk")).unwrap();
        assert!(backup.is_none());
        assert_eq!(std::fs::read(&target).unwrap(), b"data");
    }

    #[test]
    fn fcitx5_running_does_not_panic() {
        // 依赖环境，只验证调用本身安全（本机 fcitx5 在跑时返回 true）
        let _ = fcitx5_running();
    }
}
