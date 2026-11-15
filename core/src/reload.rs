//! 同步落盘后的重载触发。
//!
//! libime 源码与 fcitx5-android #545 维护者均确认：fcitx5 空闲时/退出时会把
//! 内存中的用户数据全量重写（safeSave + rename），同步工具写入的文件如果不
//! 让 fcitx5 重载，下一次自动保存就会把同步结果覆盖掉。
//!
//! 因此每次 apply 之后调用 fcitx5 的 D-Bus `ReloadConfig`。若 fcitx5 未运行
//! 则无需重载（文件就是最新事实）；若 D-Bus 调用失败则返回错误，由上层决
//! 定提示用户手动重启输入法。

use zbus::Connection;

#[derive(Debug, thiserror::Error)]
pub enum ReloadError {
    #[error("dbus error: {0}")]
    Zbus(#[from] zbus::Error),
    #[error("no session bus (DBUS_SESSION_BUS_ADDRESS unset)")]
    NoSessionBus,
}

/// fcitx5 的 D-Bus 名称 / 控制器路径与接口（fcitx5 src/lib/fcitx/dbus 测试
/// 与文档一致）。
const BUS: &str = "org.fcitx.Fcitx5";
const PATH: &str = "/controller";
const IFACE: &str = "org.fcitx.Fcitx.Controller1";

/// 触发 fcitx5 重载配置与词典。
pub async fn reload_config() -> Result<(), ReloadError> {
    if std::env::var("DBUS_SESSION_BUS_ADDRESS").is_err() {
        return Err(ReloadError::NoSessionBus);
    }
    let conn = Connection::session().await?;
    conn.call_method(Some(BUS), PATH, Some(IFACE), "ReloadConfig", &())
        .await?;
    Ok(())
}

/// 同步管线的阻塞版入口（管线全程是同步代码）。
pub fn reload_config_blocking() -> Result<(), ReloadError> {
    async_io::block_on(reload_config())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 会话总线上有 fcitx5 才能真正验证；CI 与无 fcitx5 环境自动跳过。
    #[test]
    fn reload_on_real_bus() {
        if std::env::var("DBUS_SESSION_BUS_ADDRESS").is_err() {
            return;
        }
        if !crate::engine::fcitx5_running() {
            return;
        }
        let result = async_io::block_on(reload_config());
        assert!(result.is_ok(), "reload failed: {result:?}");
    }
}
