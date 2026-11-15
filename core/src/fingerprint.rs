//! 被同步文件的变更指纹。
//!
//! 设计说明：不用 inotify。fcitx5 保存用户数据走 safeSave（临时文件 +
//! rename 原子替换），会换掉被监听文件的 inode，inotify 监听静默失效；
//! 目录轮询 mtime 简单且对该行为免疫。

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

/// 单个文件的指纹：mtime 纳秒 + 长度。
pub type Stamp = (SystemTime, u64);

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Fingerprint {
    stamps: BTreeMap<PathBuf, Stamp>,
}

impl Fingerprint {
    /// 扫描给定文件列表，缺失的文件不会出现在指纹里。
    pub fn scan(paths: &[PathBuf]) -> Fingerprint {
        let mut stamps = BTreeMap::new();
        for path in paths {
            if let Ok(meta) = std::fs::metadata(path) {
                stamps.insert(
                    path.clone(),
                    (meta.modified().unwrap_or(SystemTime::UNIX_EPOCH), meta.len()),
                );
            }
        }
        Fingerprint { stamps }
    }

    /// 返回相对 `baseline` 发生变化（含新增/删除）的文件。
    pub fn changed_since(&self, baseline: &Fingerprint) -> Vec<PathBuf> {
        let mut changed = Vec::new();
        for (path, stamp) in &self.stamps {
            if baseline.stamps.get(path) != Some(stamp) {
                changed.push(path.clone());
            }
        }
        for path in baseline.stamps.keys() {
            if !self.stamps.contains_key(path) {
                changed.push(path.clone());
            }
        }
        changed
    }

    pub fn get(&self, path: &Path) -> Option<&Stamp> {
        self.stamps.get(path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn detects_changes_additions_and_removals() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a.txt");
        let b = dir.path().join("b.txt");
        std::fs::write(&a, "hello").unwrap();

        let f1 = Fingerprint::scan(&[a.clone(), b.clone()]);
        assert!(f1.get(&a).is_some());
        assert!(f1.get(&b).is_none()); // 不存在的不进指纹

        // 修改 a、新增 b
        let mut f = std::fs::OpenOptions::new().append(true).open(&a).unwrap();
        f.write_all(b" world").unwrap();
        drop(f);
        std::fs::write(&b, "new file").unwrap();

        let f2 = Fingerprint::scan(&[a.clone(), b.clone()]);
        let mut changed = f2.changed_since(&f1);
        changed.sort();
        assert_eq!(changed, vec![a, b]);
    }

    #[test]
    fn identical_content_same_second_is_no_change() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("same.txt");
        std::fs::write(&a, "stable").unwrap();
        let f1 = Fingerprint::scan(&[a.clone()]);
        let f2 = Fingerprint::scan(&[a.clone()]);
        assert!(f2.changed_since(&f1).is_empty());
    }
}
