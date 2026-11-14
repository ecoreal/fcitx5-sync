//! `user.history`（HistoryBigram）解析与序列化。
//!
//! 格式（libime historybigram.cpp，全部大端）：
//! `[magic u32 = 0x000fc315][version u32][zstd frame]`
//! 解压后顺序写 3 个 pool（poolSize = {128, 8192, 65536}），每个 pool：
//! `[count u32]` + count × 句子；每句 `[nstrings u32]` + nstrings ×
//! `[len u32][bytes]`。字符串内词与拼音码用 `\x02` 分隔（v4 起存码）。
//!
//! libime `save()` 将 recent_ 反序写出（新→旧），load 按文件顺序逐句 add
//! 恢复权重。因此合并时应保持「旧在前、新在后」的顺序语义。

use crate::container;

/// 三个 pool 的容量上限（与 libime 构造函数一致）
pub const POOL_SIZES: [u32; 3] = [128, 8192, 65536];

#[derive(Debug, thiserror::Error)]
pub enum HistoryError {
    #[error("container error: {0}")]
    Container(#[from] container::ContainerError),
    #[error("unexpected end of payload at offset {offset}: {need}")]
    Truncated { offset: usize, need: &'static str },
    #[error("pool count {0} exceeds capacity {1}")]
    PoolOverflow(u32, u32),
}

/// 一句话：词（或词\x02码）原始字节序列。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sentence {
    /// 每个元素的原始字节（含可能的 `\x02` 码分隔）
    pub strings: Vec<Vec<u8>>,
}

impl Sentence {
    pub fn view(&self) -> Vec<String> {
        self.strings
            .iter()
            .map(|s| String::from_utf8_lossy(s).into_owned())
            .collect()
    }
}

/// 单个 pool 的句子列表。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Pool {
    pub sentences: Vec<Sentence>,
}

/// 完整 history 文件。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HistoryFile {
    pub version: u32,
    pub pools: Vec<Pool>,
}

impl HistoryFile {
    pub fn parse(bytes: &[u8]) -> Result<Self, HistoryError> {
        let container = container::history::parse(bytes)?;
        Self::from_payload(container.version, &container.payload)
    }

    fn from_payload(version: u32, payload: &[u8]) -> Result<Self, HistoryError> {
        let mut pos = 0usize;
        let mut pools = Vec::with_capacity(POOL_SIZES.len());
        for (i, &capacity) in POOL_SIZES.iter().enumerate() {
            let count = read_u32(payload, &mut pos, "pool count")?;
            if count > capacity {
                return Err(HistoryError::PoolOverflow(count, capacity));
            }
            let mut sentences = Vec::with_capacity(count.min(1024) as usize);
            for _ in 0..count {
                let nstrings = read_u32(payload, &mut pos, "sentence nstrings")?;
                let mut strings = Vec::with_capacity(nstrings.min(64) as usize);
                for _ in 0..nstrings {
                    let len = read_u32(payload, &mut pos, "string length")?;
                    if len as usize > payload.len() - pos {
                        return Err(HistoryError::Truncated {
                            offset: pos,
                            need: "string bytes",
                        });
                    }
                    strings.push(payload[pos..pos + len as usize].to_vec());
                    pos += len as usize;
                }
                sentences.push(Sentence { strings });
            }
            pools.push(Pool { sentences });
            let _ = i;
        }
        if pos != payload.len() {
            // 尾部多余字节：格式未知扩展，宁可报错也不静默丢弃
            return Err(HistoryError::Truncated {
                offset: pos,
                need: "end of payload",
            });
        }
        Ok(Self { version, pools })
    }

    /// 序列化回文件字节流（zstd 默认级别，与 libime 一致）。
    pub fn encode(&self) -> Result<Vec<u8>, HistoryError> {
        let mut payload = Vec::new();
        for pool in &self.pools {
            payload.extend_from_slice(&(pool.sentences.len() as u32).to_be_bytes());
            for sentence in &pool.sentences {
                payload.extend_from_slice(&(sentence.strings.len() as u32).to_be_bytes());
                for s in &sentence.strings {
                    payload.extend_from_slice(&(s.len() as u32).to_be_bytes());
                    payload.extend_from_slice(s);
                }
            }
        }
        let container = container::Container {
            magic: container::history::MAGIC,
            version: self.version,
            payload,
        };
        Ok(container.encode()?)
    }

    /// 合并两份 history：句子并集（按字符串序列去重），新文件中独有的句子
    /// 追加在对应 pool 末尾（保持「旧在前、新在后」）。超出 pool 容量的句子
    /// 截断丢弃（fcitx5 自身也按 pool 容量淘汰旧句）。
    pub fn merge(&self, newer: &HistoryFile) -> HistoryFile {
        let mut pools = Vec::with_capacity(self.pools.len());
        for (i, (base, inc)) in self.pools.iter().zip(newer.pools.iter()).enumerate() {
            let mut seen: Vec<&Sentence> = base.sentences.iter().collect();
            let mut merged: Vec<Sentence> = base.sentences.clone();
            for sentence in &inc.sentences {
                if !seen.iter().any(|s| *s == sentence) {
                    seen.push(sentence);
                    merged.push(sentence.clone());
                }
            }
            let cap = POOL_SIZES.get(i).copied().unwrap_or(u32::MAX) as usize;
            if merged.len() > cap {
                // 保留最新的 cap 条（尾部）
                merged.drain(0..merged.len() - cap);
            }
            pools.push(Pool { sentences: merged });
        }
        HistoryFile {
            version: self.version,
            pools,
        }
    }
}

fn read_u32(buf: &[u8], pos: &mut usize, what: &'static str) -> Result<u32, HistoryError> {
    if *pos + 4 > buf.len() {
        return Err(HistoryError::Truncated {
            offset: *pos,
            need: what,
        });
    }
    let v = u32::from_be_bytes([buf[*pos], buf[*pos + 1], buf[*pos + 2], buf[*pos + 3]]);
    *pos += 4;
    Ok(v)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn sentence(words: &[&str]) -> Sentence {
        Sentence {
            strings: words.iter().map(|w| w.as_bytes().to_vec()).collect(),
        }
    }

    fn sample(version: u32) -> HistoryFile {
        HistoryFile {
            version,
            pools: vec![
                Pool {
                    sentences: vec![sentence(&["包含"]), sentence(&["的", "shide"])],
                },
                Pool {
                    sentences: vec![sentence(&["开源", "项目", "kaiyuanxiangmu"])],
                },
                Pool::default(),
            ],
        }
    }

    #[test]
    fn roundtrip_v3_v4() {
        for version in [3u32, 4] {
            let file = sample(version);
            let bytes = file.encode().unwrap();
            assert_eq!(&bytes[..4], &container::history::MAGIC.to_be_bytes());
            assert_eq!(&bytes[4..8], &version.to_be_bytes());
            assert_eq!(&bytes[8..12], &0x28b52ffdu32.to_be_bytes()); // zstd magic
            let parsed = HistoryFile::parse(&bytes).unwrap();
            assert_eq!(parsed, file);
        }
    }

    #[test]
    fn merge_unions_and_keeps_order() {
        let base = HistoryFile {
            version: 3,
            pools: vec![
                Pool {
                    sentences: vec![sentence(&["旧句"]), sentence(&["共同句"])],
                },
                Pool::default(),
                Pool::default(),
            ],
        };
        let newer = HistoryFile {
            version: 3,
            pools: vec![
                Pool {
                    sentences: vec![sentence(&["共同句"]), sentence(&["新句"])],
                },
                Pool::default(),
                Pool::default(),
            ],
        };
        let merged = base.merge(&newer);
        assert_eq!(
            merged.pools[0].sentences,
            vec![sentence(&["旧句"]), sentence(&["共同句"]), sentence(&["新句"])]
        );
    }

    #[test]
    fn merge_respects_pool_capacity() {
        let mk = |n: u32, prefix: &str| -> HistoryFile {
            HistoryFile {
                version: 3,
                pools: vec![
                    Pool {
                        sentences: (0..n)
                            .map(|i| {
                                sentence(&[Box::leak(format!("{prefix}{i}").into_boxed_str())])
                            })
                            .collect(),
                    },
                    Pool::default(),
                    Pool::default(),
                ],
            }
        };
        // 两边各 120 条且互不相同，并集 240 条应被截断到 pool 容量 128
        let merged = mk(120, "base").merge(&mk(120, "other"));
        assert_eq!(merged.pools[0].sentences.len(), POOL_SIZES[0] as usize);
    }

    #[test]
    fn pool_overflow_rejected() {
        let mut payload = Vec::new();
        payload.extend_from_slice(&200u32.to_be_bytes()); // 超过 pool0 容量 128
        let mut file_bytes = Vec::new();
        file_bytes.extend_from_slice(&container::history::MAGIC.to_be_bytes());
        file_bytes.extend_from_slice(&3u32.to_be_bytes());
        let mut enc = zstd::stream::Encoder::new(&mut file_bytes, 0).unwrap();
        enc.write_all(&payload).unwrap();
        enc.finish().unwrap();
        assert!(matches!(
            HistoryFile::parse(&file_bytes),
            Err(HistoryError::PoolOverflow(200, 128))
        ));
    }

    /// 真实文件冒烟测试：仅当本机存在 user.dict 时运行，不提交任何真实数据。
    #[test]
    fn real_file_smoke() {
        let path = std::env::var("FCITX5_USER_HISTORY").ok();
        let Some(path) = path else { return };
        let Ok(bytes) = std::fs::read(&path) else { return };
        let file = HistoryFile::parse(&bytes).expect("parse real user.history");
        assert_eq!(file.pools.len(), 3);
        let total: usize = file.pools.iter().map(|p| p.sentences.len()).sum();
        assert!(total > 0, "real history should contain sentences");
        let re = file.encode().unwrap();
        let reparsed = HistoryFile::parse(&re).unwrap();
        assert_eq!(reparsed, file);
    }
}
