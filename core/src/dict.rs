//! `user.dict`（拼音用户词典）的文本层合并。
//!
//! libime 的 user.dict 二进制内部是 DATrie（双数组前缀树），自实现树构建
//! 成本高且易错。libime 官方自带 `libime_pinyindict` 工具做二进制↔文本互转
//! （Debian/Ubuntu 包 `libime-bin`），因此 MVP 采取「文本层合并」策略：
//!
//! 1. `libime_pinyindict -d <bin> <txt>` 转文本
//! 2. Rust 在文本层做合并（并集、词频取 max）
//! 3. `libime_pinyindict <txt> <bin>` 转回二进制
//!
//! 文本格式（实测，与 libime `saveText` 一致）：每行 `词 拼音 词频`，
//! 拼音音节间用 `'` 连接，词频为 float。

use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Debug, thiserror::Error)]
pub enum DictError {
    #[error("libime_pinyindict not found (install libime-bin or set LIBIME_PINYINDICT)")]
    ToolMissing,
    #[error("libime_pinyindict failed: {0}")]
    ToolFailed(String),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("bad dict text line {line}: {text:?}")]
    BadLine { line: usize, text: String },
}

#[derive(Debug, Clone, PartialEq)]
pub struct DictEntry {
    pub word: String,
    /// 全拼，音节以 `'` 连接（libime 文本格式原样保留）
    pub pinyin: String,
    pub freq: f32,
}

pub fn parse_text(text: &str) -> Result<Vec<DictEntry>, DictError> {
    let mut entries = Vec::new();
    for (i, raw) in text.lines().enumerate() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let mut parts = line.split_whitespace();
        let (word, pinyin, freq) = match (parts.next(), parts.next(), parts.next()) {
            (Some(w), Some(p), Some(f)) if parts.next().is_none() => (w, p, f),
            _ => {
                return Err(DictError::BadLine {
                    line: i + 1,
                    text: line.to_string(),
                })
            }
        };
        let freq: f32 = freq.parse().map_err(|_| DictError::BadLine {
            line: i + 1,
            text: line.to_string(),
        })?;
        entries.push(DictEntry {
            word: word.to_string(),
            pinyin: pinyin.to_string(),
            freq,
        });
    }
    Ok(entries)
}

pub fn to_text(entries: &[DictEntry]) -> String {
    let mut out = String::with_capacity(entries.len() * 24);
    for e in entries {
        out.push_str(&e.word);
        out.push(' ');
        out.push_str(&e.pinyin);
        out.push(' ');
        out.push_str(&fmt_freq(e.freq));
        out.push('\n');
    }
    out
}

/// 频率格式化：整数频值不带小数点，与 libime 输出习惯一致。
fn fmt_freq(f: f32) -> String {
    if f.fract() == 0.0 && f.is_finite() {
        format!("{}", f as i64)
    } else {
        format!("{f}")
    }
}

/// 合并：按 (word, pinyin) 并集；两边都有的词频取 max；
/// base 顺序优先，newer 独有词条按 newer 顺序追加。
pub fn merge(base: &[DictEntry], newer: &[DictEntry]) -> Vec<DictEntry> {
    let mut key = |e: &DictEntry| (e.word.clone(), e.pinyin.clone());
    let mut result: Vec<DictEntry> = Vec::with_capacity(base.len() + newer.len());
    let mut newer_map: std::collections::HashMap<(String, String), f32> =
        std::collections::HashMap::with_capacity(newer.len());
    for e in newer {
        let k = key(e);
        newer_map
            .entry(k)
            .and_modify(|f| *f = f.max(e.freq))
            .or_insert(e.freq);
    }
    let mut covered = std::collections::HashSet::new();
    for e in base {
        let k = key(e);
        covered.insert(k.clone());
        let freq = newer_map.get(&k).map_or(e.freq, |n| e.freq.max(*n));
        result.push(DictEntry {
            word: e.word.clone(),
            pinyin: e.pinyin.clone(),
            freq,
        });
    }
    for e in newer {
        let k = key(e);
        if !covered.contains(&k) {
            covered.insert(k);
            result.push(e.clone());
        }
    }
    result
}

fn resolve_tool() -> Result<PathBuf, DictError> {
    if let Ok(p) = std::env::var("LIBIME_PINYINDICT") {
        let path = PathBuf::from(p);
        return if path.exists() { Ok(path) } else { Err(DictError::ToolMissing) };
    }
    // PATH 查找
    let path_var = std::env::var("PATH").unwrap_or_default();
    for dir in std::env::split_paths(&path_var) {
        let candidate = dir.join("libime_pinyindict");
        if candidate.is_file() {
            return Ok(candidate);
        }
    }
    Err(DictError::ToolMissing)
}

fn run_tool(args: &[&Path]) -> Result<(), DictError> {
    let tool = resolve_tool()?;
    let out = Command::new(&tool).args(args).output().map_err(|e| {
        DictError::ToolFailed(format!("{}: {e}", tool.display()))
    })?;
    if !out.status.success() {
        return Err(DictError::ToolFailed(format!(
            "{} {}",
            tool.display(),
            String::from_utf8_lossy(&out.stderr)
        )));
    }
    Ok(())
}

/// 二进制 → 文本（调用 libime_pinyindict -d）。
pub fn dump_binary(bin: &Path, txt: &Path) -> Result<(), DictError> {
    run_tool(&[Path::new("-d"), bin, txt])
}

/// 文本 → 二进制（调用 libime_pinyindict）。
pub fn create_binary(txt: &Path, bin: &Path) -> Result<(), DictError> {
    run_tool(&[txt, bin])
}

/// 完整合并管线：两个二进制 dict → 合并后的二进制。
/// `base_bin`/`newer_bin` 为输入，`out_bin` 为输出；中间文件写在
/// `workdir` 下，调用方负责清理（一般用 tempdir）。
pub fn merge_binary(
    base_bin: &Path,
    newer_bin: &Path,
    out_bin: &Path,
    workdir: &Path,
) -> Result<(), DictError> {
    let base_txt = workdir.join("base.txt");
    let newer_txt = workdir.join("newer.txt");
    let merged_txt = workdir.join("merged.txt");
    dump_binary(base_bin, &base_txt)?;
    dump_binary(newer_bin, &newer_txt)?;
    let base = parse_text(&std::fs::read_to_string(&base_txt)?)?;
    let newer = parse_text(&std::fs::read_to_string(&newer_txt)?)?;
    let merged = merge(&base, &newer);
    std::fs::write(&merged_txt, to_text(&merged))?;
    create_binary(&merged_txt, out_bin)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(w: &str, p: &str, f: f32) -> DictEntry {
        DictEntry {
            word: w.into(),
            pinyin: p.into(),
            freq: f,
        }
    }

    #[test]
    fn parse_and_format_roundtrip() {
        let text = "你好 ni'hao 3\n世界 shi'jie 0\n开发者 kai'fa'zhe 12.5\n";
        let entries = parse_text(text).unwrap();
        assert_eq!(entries.len(), 3);
        assert_eq!(entries[0], entry("你好", "ni'hao", 3.0));
        assert_eq!(entries[2], entry("开发者", "kai'fa'zhe", 12.5));
        let rebuilt = to_text(&entries);
        assert_eq!(parse_text(&rebuilt).unwrap(), entries);
    }

    #[test]
    fn parse_rejects_bad_line() {
        let err = parse_text("你好 ni'hao").unwrap_err();
        assert!(matches!(err, DictError::BadLine { line: 1, .. }));
    }

    #[test]
    fn parse_skips_comments_and_blank() {
        let entries = parse_text("# 注释\n\n你好 ni'hao 1\n").unwrap();
        assert_eq!(entries.len(), 1);
    }

    #[test]
    fn merge_unions_and_max_freq() {
        let base = vec![entry("你好", "ni'hao", 3.0), entry("旧词", "jiu'ci", 1.0)];
        let newer = vec![entry("你好", "ni'hao", 7.0), entry("新词", "xin'ci", 2.0)];
        let merged = merge(&base, &newer);
        assert_eq!(merged.len(), 3);
        assert_eq!(merged[0], entry("你好", "ni'hao", 7.0)); // 取 max
        assert_eq!(merged[1], entry("旧词", "jiu'ci", 1.0)); // base 独有保序
        assert_eq!(merged[2], entry("新词", "xin'ci", 2.0)); // newer 独有追加
    }

    /// 本机装了 libime-bin 且设置环境变量才跑：真实字典 dump → 解析 → 计数冒烟。
    #[test]
    fn real_dict_dump_smoke() {
        let Ok(path) = std::env::var("FCITX5_USER_DICT") else { return };
        let bin = Path::new(&path);
        if !bin.exists() || resolve_tool().is_err() {
            return; // 无真实文件或无工具的环境跳过
        }
        let dir = tempfile::tempdir().unwrap();
        let txt = dir.path().join("real.txt");
        dump_binary(bin, &txt).unwrap();
        let entries = parse_text(&std::fs::read_to_string(&txt).unwrap()).unwrap();
        assert!(!entries.is_empty(), "real dict should have entries");
    }

    /// 真实词库完整 merge 管线 round-trip：dump → merge(自并) → create → dump。
    /// 验证 create_binary 产出的二进制可被 libime 重新读取（fcitx5 加载能力的
    /// 真实验证在真机验收做，此处为管线完整性冒烟）。
    #[test]
    fn real_dict_merge_roundtrip() {
        let Ok(path) = std::env::var("FCITX5_USER_DICT") else { return };
        let bin = Path::new(&path);
        if !bin.exists() || resolve_tool().is_err() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("merged.dict");
        merge_binary(bin, bin, &out, dir.path()).unwrap();
        let txt = dir.path().join("out.txt");
        dump_binary(&out, &txt).unwrap();
        let original = parse_text(&{
            let t = dir.path().join("orig.txt");
            dump_binary(bin, &t).unwrap();
            std::fs::read_to_string(&t).unwrap()
        })
        .unwrap();
        let result = parse_text(&std::fs::read_to_string(&txt).unwrap()).unwrap();
        assert_eq!(result.len(), original.len(), "merge with self keeps all entries");
    }
}
