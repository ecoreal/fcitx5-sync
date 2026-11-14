//! fcitx5-sync-core：fcitx5 用户数据同步的纯 Rust 核心。
//!
//! 分层：container（外层容器）→ dict/history（具体格式）→ merge（合并策略）
//! → watcher/transport（文件监听与同步通道）。全部不依赖 Tauri，可独立测试。

pub mod container;
pub mod history;

#[cfg(test)]
mod tests {
    #[test]
    fn it_works() {
        assert!(true);
    }
}
