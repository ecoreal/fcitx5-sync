//! fcitx5 用户数据文件的外层容器。
//!
//! 实测结构（2026-11-15，本机 Plasma 6 / fcitx5）：
//! `[magic u32 BE][version u32 BE][zstd frame]`
//! - `user.dict`   magic = 0x000f_c613, version = 2
//! - `user.history` magic = 0x000f_c315, version = 3
//!
//! 字段语义以 libime 源码为准（见 dict.rs / history.rs），本模块只负责容器层。

use std::io::Read;

#[derive(Debug, thiserror::Error)]
pub enum ContainerError {
    #[error("file too short to be an fcitx5 data file: {0} bytes")]
    TooShort(usize),
    #[error("bad magic: expected 0x{expected:08x}, found 0x{found:08x}")]
    BadMagic { expected: u32, found: u32 },
    #[error("zstd decompression failed: {0}")]
    Zstd(#[from] std::io::Error),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Container {
    pub magic: u32,
    pub version: u32,
    /// zstd 解压后的载荷
    pub payload: Vec<u8>,
}

impl Container {
    /// 读取并解压一个 fcitx5 数据文件。
    pub fn parse(bytes: &[u8], expected_magic: u32) -> Result<Self, ContainerError> {
        if bytes.len() < 12 {
            return Err(ContainerError::TooShort(bytes.len()));
        }
        let magic = u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
        if magic != expected_magic {
            return Err(ContainerError::BadMagic {
                expected: expected_magic,
                found: magic,
            });
        }
        let version = u32::from_be_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]);
        let mut payload = Vec::with_capacity(bytes.len() * 4);
        zstd::stream::Decoder::new(&bytes[8..])?.read_to_end(&mut payload)?;
        Ok(Self {
            magic,
            version,
            payload,
        })
    }

    /// 编码回文件字节流（载荷 zstd 压缩，压缩级别与 libime 一致用默认值）。
    pub fn encode(&self) -> Result<Vec<u8>, ContainerError> {
        let mut out = Vec::with_capacity(self.payload.len() / 2 + 8);
        out.extend_from_slice(&self.magic.to_be_bytes());
        out.extend_from_slice(&self.version.to_be_bytes());
        let mut encoder = zstd::stream::Encoder::new(&mut out, 0)?;
        std::io::Write::write_all(&mut encoder, &self.payload)?;
        encoder.finish()?;
        Ok(out)
    }
}

pub mod dict {
    use super::ContainerError;

    /// `user.dict` 的 magic（实测 0x000fc613）
    pub const MAGIC: u32 = 0x000f_c613;
    /// `user.dict` 的版本号（实测 2）
    pub const VERSION: u32 = 2;

    /// 解析 `user.dict`
    pub fn parse(bytes: &[u8]) -> Result<super::Container, ContainerError> {
        super::Container::parse(bytes, MAGIC)
    }
}

pub mod history {
    use super::ContainerError;

    /// `user.history` 的 magic（实测 0x000fc315）
    pub const MAGIC: u32 = 0x000f_c315;
    /// `user.history` 的版本号（实测 3）
    pub const VERSION: u32 = 3;

    /// 解析 `user.history`
    pub fn parse(bytes: &[u8]) -> Result<super::Container, ContainerError> {
        super::Container::parse(bytes, MAGIC)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &[u8] = &[0x28, 0xb5, 0x2f, 0xfd]; // zstd magic，仅占位

    fn make_container() -> Container {
        Container {
            magic: dict::MAGIC,
            version: dict::VERSION,
            payload: b"VACQD!\xe5\xb9\xb3\xe6\x9d\xbf\xe4\xb8\x8a\x00\x00\x00\x00\x00".to_vec(),
        }
    }

    #[test]
    fn roundtrip_container() {
        let c = make_container();
        let bytes = c.encode().unwrap();
        // 编码产物长度必须超过头部 8 字节
        assert!(bytes.len() > 8);
        let parsed = Container::parse(&bytes, dict::MAGIC).unwrap();
        assert_eq!(parsed, c);
    }

    #[test]
    fn rejects_wrong_magic() {
        let c = make_container();
        let bytes = c.encode().unwrap();
        let err = Container::parse(&bytes, history::MAGIC).unwrap_err();
        assert!(matches!(err, ContainerError::BadMagic { .. }));
    }

    #[test]
    fn rejects_too_short() {
        let err = Container::parse(SAMPLE, dict::MAGIC).unwrap_err();
        assert!(matches!(err, ContainerError::TooShort(4)));
    }
}
