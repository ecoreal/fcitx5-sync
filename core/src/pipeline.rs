//! 同步管线编排：push（本机 → 远端）与 pull（远端 → 本机）两条完整流。
//!
//! push：拉远端副本 → 双方合并（超集）→ 推回远端。本机不动。
//! pull：拉远端副本 → 双方合并 → 备份后原子写回本机 → D-Bus 重载 fcitx5。
//!
//! dict 合并依赖外部 `libime_pinyindict`（文本层管线，见 dict.rs）；
//! history 合并为纯 Rust（见 history.rs）。远端文件缺失视为空（首次同步）。

use crate::dict;
use crate::engine::{self, EngineError};
use crate::history::{self, HistoryError};
use crate::reload::{self, ReloadError};
use crate::transport::{SyncTransport, TransportError};
use std::path::{Path, PathBuf};

#[derive(Debug, thiserror::Error)]
pub enum PipelineError {
    #[error("dict stage: {0}")]
    Dict(#[from] dict::DictError),
    #[error("history stage: {0}")]
    History(#[from] HistoryError),
    #[error("transport stage: {0}")]
    Transport(#[from] TransportError),
    #[error("engine stage: {0}")]
    Engine(#[from] EngineError),
    #[error("reload stage: {0}")]
    Reload(#[from] ReloadError),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

/// 一次同步涉及的路径集合。
pub struct SyncJob {
    /// 本机 `user.dict`
    pub dict_path: PathBuf,
    /// 本机 `user.history`
    pub history_path: PathBuf,
    /// 覆盖前的备份目录
    pub backup_dir: PathBuf,
    /// 中间文件工作目录（一般传 tempdir）
    pub workdir: PathBuf,
}

/// pull 的结果报告。
#[derive(Debug, Default, Clone)]
pub struct PullReport {
    /// 是否有变更写回本机
    pub applied: bool,
    /// 各文件备份路径（仅当本机原文件存在且内容变化时）
    pub backups: Vec<PathBuf>,
    /// 是否触发了 D-Bus 重载
    pub reloaded: bool,
}

/// push 的结果报告。
#[derive(Debug, Default, Clone)]
pub struct PushReport {
    /// 推送到远端的文件名
    pub pushed: Vec<String>,
}

const DICT_REMOTE: &str = "user.dict";
const HISTORY_REMOTE: &str = "user.history";

/// 尝试拉取远端文件；不存在时返回 None。
fn try_pull(
    transport: &dyn SyncTransport,
    remote: &str,
    dest: &Path,
) -> Result<Option<PathBuf>, PipelineError> {
    match transport.pull(remote, dest) {
        Ok(()) => Ok(Some(dest.to_path_buf())),
        Err(TransportError::Io(e)) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.into()),
    }
}

/// push：合并双方后推远端。本机文件不修改。
pub fn push_to(transport: &dyn SyncTransport, job: &SyncJob) -> Result<PushReport, PipelineError> {
    let mut report = PushReport::default();
    std::fs::create_dir_all(&job.workdir)?;

    // ---- dict ----
    let remote_dict = job.workdir.join("remote.dict");
    let remote_dict = try_pull(transport, DICT_REMOTE, &remote_dict)?;
    if remote_dict.is_some() && dict::resolve_tool_path().is_none() {
        return Err(dict::DictError::ToolMissing.into());
    }
    let merged_dict = job.workdir.join("merged.dict");
    match remote_dict.as_deref() {
        Some(remote) => {
            dict::merge_binary(&job.dict_path, remote, &merged_dict, &job.workdir)?;
        }
        None => {
            std::fs::copy(&job.dict_path, &merged_dict)?;
        }
    }
    transport.push(&merged_dict, DICT_REMOTE)?;
    report.pushed.push(DICT_REMOTE.into());

    // ---- history ----
    let remote_history = job.workdir.join("remote.history");
    let remote_history = try_pull(transport, HISTORY_REMOTE, &remote_history)?;
    let merged_bytes = match remote_history.as_deref() {
        Some(remote) => {
            let base = history::HistoryFile::parse(&std::fs::read(&job.history_path)?)?;
            let newer = history::HistoryFile::parse(&std::fs::read(remote)?)?;
            base.merge(&newer).encode()?
        }
        None => std::fs::read(&job.history_path)?,
    };
    let merged_history = job.workdir.join("merged.history");
    std::fs::write(&merged_history, &merged_bytes)?;
    transport.push(&merged_history, HISTORY_REMOTE)?;
    report.pushed.push(HISTORY_REMOTE.into());

    Ok(report)
}

/// pull：合并双方，备份后原子写回本机，并触发 D-Bus 重载。
pub fn pull_from(
    transport: &dyn SyncTransport,
    job: &SyncJob,
) -> Result<PullReport, PipelineError> {
    let mut report = PullReport::default();
    std::fs::create_dir_all(&job.workdir)?;

    let remote_dict = job.workdir.join("remote.dict");
    let remote_dict = try_pull(transport, DICT_REMOTE, &remote_dict)?;
    let remote_history = job.workdir.join("remote.history");
    let remote_history = try_pull(transport, HISTORY_REMOTE, &remote_history)?;

    if remote_dict.is_none() && remote_history.is_none() {
        return Ok(report); // 远端为空：首次同步前 pull 是无害 no-op
    }

    // ---- dict ----
    if let Some(remote) = remote_dict.as_deref() {
        let remote_bytes = std::fs::read(remote)?;
        let merged: Vec<u8> = match std::fs::read(&job.dict_path) {
            // 双方都有：文本层合并
            Ok(_) => {
                let merged_dict = job.workdir.join("merged.dict");
                dict::merge_binary(&job.dict_path, remote, &merged_dict, &job.workdir)?;
                std::fs::read(&merged_dict)?
            }
            // 本机没有（新装机首次同步）：直接采用远端副本
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => remote_bytes,
            Err(e) => return Err(e.into()),
        };
        let local_bytes = std::fs::read(&job.dict_path).ok();
        if local_bytes.as_deref() != Some(merged.as_slice()) {
            let backup = engine::backup_and_apply(&job.dict_path, &merged, &job.backup_dir)?;
            if let Some(b) = backup {
                report.backups.push(b);
            }
            report.applied = true;
        }
    }

    // ---- history ----
    if let Some(remote) = remote_history.as_deref() {
        let merged_bytes = match std::fs::read(&job.history_path) {
            Ok(local) => {
                let base = history::HistoryFile::parse(&local)?;
                let newer = history::HistoryFile::parse(&std::fs::read(remote)?)?;
                base.merge(&newer).encode()?
            }
            // 本机没有：直接采用远端副本
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => std::fs::read(remote)?,
            Err(e) => return Err(e.into()),
        };
        let local_bytes = std::fs::read(&job.history_path).ok();
        if local_bytes.as_deref() != Some(merged_bytes.as_slice()) {
            let backup =
                engine::backup_and_apply(&job.history_path, &merged_bytes, &job.backup_dir)?;
            if let Some(b) = backup {
                report.backups.push(b);
            }
            report.applied = true;
        }
    }

    // ---- reload ----
    if report.applied && engine::fcitx5_running() {
        match reload::reload_config_blocking() {
            Ok(()) => report.reloaded = true,
            // 重载失败不回滚数据（本地已是超集），向上层报告以便提示用户
            Err(e) => return Err(e.into()),
        }
    }

    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transport::DirTransport;
    use std::io::Write;

    fn make_history_file(words: &[&str]) -> Vec<u8> {
        let file = history::HistoryFile {
            version: crate::container::history::VERSION,
            pools: vec![
                history::Pool {
                    sentences: words
                        .iter()
                        .map(|w| history::Sentence {
                            strings: vec![w.as_bytes().to_vec()],
                        })
                        .collect(),
                },
                history::Pool::default(),
                history::Pool::default(),
            ],
        };
        file.encode().unwrap()
    }

    /// 完整管线端到端（需要 libime_pinyindict，CI 由 ci.yml 安装 libime-bin）：
    /// 用合成数据现场生成真字典二进制，push 超集上远端 → pull 写回本机并备份。
    #[test]
    fn pipeline_push_then_pull() {
        if dict::resolve_tool_path().is_none() {
            return; // 无工具环境跳过
        }
        let local_dir = tempfile::tempdir().unwrap();
        let remote_dir = tempfile::tempdir().unwrap();
        let backup_dir = local_dir.path().join("backups");
        let dict_path = local_dir.path().join("user.dict");
        let history_path = local_dir.path().join("user.history");

        // 生成真实可解析的 user.dict（合成词条，不含真实用户数据）
        let dict_txt = local_dir.path().join("dict.txt");
        std::fs::write(&dict_txt, "你好 ni'hao 3\n世界 shi'jie 0\n").unwrap();
        dict::create_binary(&dict_txt, &dict_path).unwrap();
        std::fs::write(&history_path, make_history_file(&["本机旧句"])).unwrap();

        let transport = DirTransport::new(remote_dir.path());
        // 远端先放一份「另一台机器」的 history
        let remote_seed = remote_dir.path().join("user.history");
        std::fs::write(&remote_seed, make_history_file(&["远端新句"])).unwrap();

        let job = SyncJob {
            dict_path: dict_path.clone(),
            history_path: history_path.clone(),
            backup_dir: backup_dir.clone(),
            workdir: local_dir.path().join("work"),
        };

        // push：合并本机+远端 → 远端应为超集
        let pushed = push_to(&transport, &job).unwrap();
        assert_eq!(pushed.pushed, vec![DICT_REMOTE, HISTORY_REMOTE]);
        let merged_remote =
            history::HistoryFile::parse(&std::fs::read(&remote_seed).unwrap()).unwrap();
        let names: Vec<String> = merged_remote.pools[0]
            .sentences
            .iter()
            .flat_map(|s| s.view())
            .collect();
        assert!(names.contains(&"本机旧句".to_string()));
        assert!(names.contains(&"远端新句".to_string()));

        // pull：远端超集写回本机
        let report = pull_from(&transport, &job).unwrap();
        assert!(report.applied);
        assert!(report.backups.len() >= 1, "至少 history 应有一个备份");

        // 本机 history 应包含双方句子
        let local_now =
            history::HistoryFile::parse(&std::fs::read(&history_path).unwrap()).unwrap();
        let names: Vec<String> = local_now.pools[0]
            .sentences
            .iter()
            .flat_map(|s| s.view())
            .collect();
        assert!(names.contains(&"本机旧句".to_string()));
        assert!(names.contains(&"远端新句".to_string()));

        // 本机 dict 应仍可被 libime 读取且词条守恒
        let dumped = local_dir.path().join("after.txt");
        dict::dump_binary(&dict_path, &dumped).unwrap();
        let entries = dict::parse_text(&std::fs::read_to_string(&dumped).unwrap()).unwrap();
        assert_eq!(entries.len(), 2);
        assert!(entries.iter().any(|e| e.word == "你好"));
    }

    /// 新装机场景：本机无 user.dict / user.history，pull 直接采用远端副本。
    #[test]
    fn pull_adopt_remote_when_local_missing() {
        if dict::resolve_tool_path().is_none() {
            return;
        }
        let local_dir = tempfile::tempdir().unwrap();
        let remote_dir = tempfile::tempdir().unwrap();

        // 远端放一份真实字典与 history
        let dict_txt = remote_dir.path().join("dict.txt");
        std::fs::write(&dict_txt, "远端词 yuan'duan 5\n").unwrap();
        let remote_dict_bin = remote_dir.path().join("user.dict");
        dict::create_binary(&dict_txt, &remote_dict_bin).unwrap();
        let remote_history = remote_dir.path().join("user.history");
        std::fs::write(&remote_history, make_history_file(&["远端句子"])).unwrap();

        let transport = DirTransport::new(remote_dir.path());
        let job = SyncJob {
            dict_path: local_dir.path().join("user.dict"),
            history_path: local_dir.path().join("user.history"),
            backup_dir: local_dir.path().join("bk"),
            workdir: local_dir.path().join("work"),
        };
        let report = pull_from(&transport, &job).unwrap();
        assert!(report.applied);
        assert!(report.backups.is_empty(), "本机原本无文件，不应有备份");
        assert!(job.dict_path.exists() && job.history_path.exists());

        let entries = {
            let dumped = local_dir.path().join("chk.txt");
            dict::dump_binary(&job.dict_path, &dumped).unwrap();
            dict::parse_text(&std::fs::read_to_string(&dumped).unwrap()).unwrap()
        };
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].word, "远端词");
        let local_history =
            history::HistoryFile::parse(&std::fs::read(&job.history_path).unwrap()).unwrap();
        assert_eq!(local_history.pools[0].sentences.len(), 1);
    }

    #[test]
    fn pull_noop_when_remote_empty() {
        let local_dir = tempfile::tempdir().unwrap();
        let remote_dir = tempfile::tempdir().unwrap();
        let history_path = local_dir.path().join("user.history");
        let mut f = std::fs::File::create(&history_path).unwrap();
        f.write_all(&make_history_file(&["唯一句"])).unwrap();
        drop(f);
        let job = SyncJob {
            dict_path: local_dir.path().join("user.dict"),
            history_path,
            backup_dir: local_dir.path().join("bk"),
            workdir: local_dir.path().join("work"),
        };
        let report = pull_from(&DirTransport::new(remote_dir.path()), &job).unwrap();
        assert!(!report.applied);
        assert!(report.backups.is_empty());
        assert!(!report.reloaded);
    }
}
