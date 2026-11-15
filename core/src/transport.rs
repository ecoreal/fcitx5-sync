//! 同步通道抽象与实现。
//!
//! MVP 提供两种介质：
//! - `DirTransport`：一个目录作为远端（Syncthing / 坚果云同步盘 / U 盘皆可）
//! - `WebDavTransport`：WebDAV 服务器（坚果云原生协议），需要 `reqwest`
//!
//! Git 通道留待后续版本。

use std::path::{Path, PathBuf};

#[derive(Debug, thiserror::Error)]
pub enum TransportError {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("webdav request failed: {0}")]
    WebDav(String),
    #[error("webdav not configured (set FSYNC_WEBDAV_URL / _USER / _PASS)")]
    WebDavNotConfigured,
}

/// 同步通道。`remote_name` 为远端文件名（如 `user.dict`）。
pub trait SyncTransport: Send + Sync {
    fn name(&self) -> &'static str;
    fn push(&self, local: &Path, remote_name: &str) -> Result<(), TransportError>;
    fn pull(&self, remote_name: &str, local: &Path) -> Result<(), TransportError>;
}

/// 目录通道：远端就是一个普通目录。
pub struct DirTransport {
    pub root: PathBuf,
}

impl DirTransport {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }
}

impl SyncTransport for DirTransport {
    fn name(&self) -> &'static str {
        "dir"
    }

    fn push(&self, local: &Path, remote_name: &str) -> Result<(), TransportError> {
        std::fs::create_dir_all(&self.root)?;
        std::fs::copy(local, self.root.join(remote_name))?;
        Ok(())
    }

    fn pull(&self, remote_name: &str, local: &Path) -> Result<(), TransportError> {
        std::fs::copy(self.root.join(remote_name), local)?;
        Ok(())
    }
}

/// WebDAV 通道（坚果云：server 地址 + 应用密码）。
pub struct WebDavTransport {
    pub url: String,
    pub username: String,
    pub password: String,
}

impl std::fmt::Debug for WebDavTransport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WebDavTransport")
            .field("url", &self.url)
            .field("username", &self.username)
            .field("password", &"<redacted>")
            .finish()
    }
}

impl WebDavTransport {
    /// 从环境变量构造：`FSYNC_WEBDAV_URL` / `FSYNC_WEBDAV_USER` / `FSYNC_WEBDAV_PASS`。
    pub fn from_env() -> Result<Self, TransportError> {
        match (
            std::env::var("FSYNC_WEBDAV_URL"),
            std::env::var("FSYNC_WEBDAV_USER"),
            std::env::var("FSYNC_WEBDAV_PASS"),
        ) {
            (Ok(url), Ok(user), Ok(pass)) if !url.is_empty() => Ok(Self {
                url: url.trim_end_matches('/').into(),
                username: user,
                password: pass,
            }),
            _ => Err(TransportError::WebDavNotConfigured),
        }
    }

    fn client(&self) -> Result<reqwest::blocking::Client, TransportError> {
        reqwest::blocking::Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .map_err(|e| TransportError::WebDav(e.to_string()))
    }
}

impl SyncTransport for WebDavTransport {
    fn name(&self) -> &'static str {
        "webdav"
    }

    fn push(&self, local: &Path, remote_name: &str) -> Result<(), TransportError> {
        let bytes = std::fs::read(local)?;
        let url = format!("{}/{}", self.url, remote_name);
        let resp = self
            .client()?
            .put(&url)
            .basic_auth(&self.username, Some(&self.password))
            .body(bytes)
            .send()
            .map_err(|e| TransportError::WebDav(e.to_string()))?;
        if !resp.status().is_success() {
            return Err(TransportError::WebDav(format!(
                "PUT {url} -> {}",
                resp.status()
            )));
        }
        Ok(())
    }

    fn pull(&self, remote_name: &str, local: &Path) -> Result<(), TransportError> {
        let url = format!("{}/{}", self.url, remote_name);
        let resp = self
            .client()?
            .get(&url)
            .basic_auth(&self.username, Some(&self.password))
            .send()
            .map_err(|e| TransportError::WebDav(e.to_string()))?;
        if !resp.status().is_success() {
            return Err(TransportError::WebDav(format!(
                "GET {url} -> {}",
                resp.status()
            )));
        }
        let bytes = resp
            .bytes()
            .map_err(|e| TransportError::WebDav(e.to_string()))?;
        std::fs::write(local, &bytes)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dir_transport_roundtrip() {
        let local_dir = tempfile::tempdir().unwrap();
        let remote_dir = tempfile::tempdir().unwrap();
        let transport = DirTransport::new(remote_dir.path());

        let local = local_dir.path().join("user.dict");
        std::fs::write(&local, b"payload-1").unwrap();

        transport.push(&local, "user.dict").unwrap();
        assert_eq!(
            std::fs::read(remote_dir.path().join("user.dict")).unwrap(),
            b"payload-1"
        );

        // 远端更新后 pull 回来
        std::fs::write(remote_dir.path().join("user.dict"), b"payload-2").unwrap();
        transport.pull("user.dict", &local).unwrap();
        assert_eq!(std::fs::read(&local).unwrap(), b"payload-2");
    }

    #[test]
    fn webdav_requires_config() {
        // 未配置环境变量时应返回明确的配置缺失错误
        // （edition 2024 起进程环境变量操作为 unsafe；测试进程独占，安全）
        unsafe { std::env::remove_var("FSYNC_WEBDAV_URL") };
        let err = WebDavTransport::from_env().unwrap_err();
        assert!(matches!(err, TransportError::WebDavNotConfigured));
    }
}
