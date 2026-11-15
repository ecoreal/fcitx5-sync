# fcitx5-sync

[![CI](https://github.com/ecoreal/fcitx5-sync/actions/workflows/ci.yml/badge.svg)](https://github.com/ecoreal/fcitx5-sync/actions/workflows/ci.yml)
[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)

fcitx5 词库多机同步：把你的拼音用户词典（自造词 + 词频）与输入历史在多台 Linux 机器之间合并同步，任意一台机器的最新积累都能出现在其他所有机器上。

## 这是什么

一个 Tauri 2 托盘应用，同步 fcitx5 的两个用户数据文件：

- `user.dict` —— 拼音用户词典（你打过的词和词频）
- `user.history` —— 输入历史（整句/词条的频率上下文，影响整句输入体验）

支持手动推送 / 拉取，也可以按设定间隔自动双向同步。远端可以是任何会被多机共享的目录：Syncthing、坚果云同步文件夹、U 盘皆可。

## 为什么

fcitx5 官方至今没有云同步功能——社区在 [fcitx5-android#545](https://github.com/fcitx5-android/fcitx5-android/issues/545) 提出词库云同步需求，多年悬而未决。实际痛点：

- 换机器 / 重装系统，精心养出来的词库清零；
- 多台机器各自积累的词条与输入历史无法合并，手动拷贝文件只会互相覆盖。

fcitx5-sync 把「合并」做在文件层：不管哪台机器先跑同步，结果都是所有机器数据的**超集**，不会丢词。

## 安装

运行依赖：

- fcitx5（你正在用的）
- `libime-bin` —— 提供 `libime_pinyindict` 工具，用于 `user.dict` 的编解码：

```bash
# Debian / Ubuntu
sudo apt-get install libime-bin
# Arch Linux
sudo pacman -S libime
```

其他发行版：确保 `libime_pinyindict` 在 `PATH` 中，或用环境变量 `LIBIME_PINYINDICT` 指定其路径。

应用本体：从 [Releases](https://github.com/ecoreal/fcitx5-sync/releases) 下载 deb / AppImage / rpm，或按下方「开发命令」从源码构建。

## 使用

- 启动后常驻系统托盘，关闭窗口 = 隐藏到托盘，退出走托盘菜单。
- 托盘菜单：**立即推送**（本机词库合并进同步目录）、**立即拉取**（同步目录写回本机，自动备份并通知 fcitx5 重载）、打开面板、退出。
- 面板「设置」里填**同步目录**——放进 Syncthing / 坚果云同步范围的一个文件夹路径（如 `/home/xxx/SyncDrive/fcitx5`），所有机器填同一个；再设**自动同步间隔**（分钟，0 = 关闭）。
- 自动同步开启后，应用按间隔在后台执行双向同步（先拉取合并写回本机，再把超集推回远端），每轮结束弹系统通知（可在设置里关闭）。
- 首次使用：在词库最全的那台机器点一次「立即推送」把数据种进同步目录；其余新机器点「立即拉取」即可接管词库。

## 工作原理

远端目录里就两个文件：`user.dict` 与 `user.history`（与 fcitx5 本地文件同名同格式）。同步的核心是把「覆盖拷贝」换成「三层合并」：

- **`user.dict` 文本层合并** —— libime 的词典二进制是 DATrie 双数组前缀树，直接改二进制不现实。借助官方 `libime_pinyindict` 把二进制转回文本（每行 `词 拼音 词频`），在文本层做并集（同词词频取 max），再转回二进制。
- **`user.history` 原生合并** —— 纯 Rust 解析 HistoryBigram 格式（`magic + version + zstd` 容器，3 个 pool，大端序），按句子并集合并并保持 libime 的顺序语义，不依赖外部工具。
- **安全落盘** —— 写回本机前先把原文件备份到应用数据目录（带时间戳，可回滚）；写入用临时文件 + rename 原子替换，与 fcitx5 自身的 safeSave 语义一致，同步盘拷走的一定是完整文件。
- **D-Bus ReloadConfig** —— 写回成功后通过 fcitx5 的 D-Bus 接口触发重载。这一步不可省：fcitx5 在空闲 / 退出时会把内存中的用户数据全量重写回磁盘，不重载的话下一次自动保存就会把同步结果覆盖掉。

双向同步一轮 = 先 pull（远端 ∪ 本机 → 写回本机）再 push（超集 → 远端）。任何一台机器跑完一轮，远端就是全体机器的超集。

## 架构

```
core/       纯 Rust 核心（不依赖 Tauri，可独立测试）
  container/ dict/ history/   格式层：容器解码、dict 文本层、history 原生解析
  pipeline/                   push / pull 两条完整同步流
  transport/                  同步通道：目录通道（可用）；WebDAV（已实现待接 UI）
  engine/ fingerprint/ reload/  原子写+备份、变更指纹、D-Bus 重载
src-tauri/  薄壳：托盘、设置持久化、push/pull 命令、自动同步调度、系统通知
src/        React 前端（面板）
```

core 是纯函数式管线（输入路径集合 → 输出报告），不做任何状态管理；壳只做参数组装、状态管理与 UI 事件。所有合并逻辑都在 core 里用临时目录 + 合成数据测试覆盖。

## 开发命令

| 命令 | 说明 |
|---|---|
| `npm run tauri dev` | 开发窗口 |
| `npm run tauri build` | 打包 |
| `npx vitest run` | 前端测试 |
| `npx tsc -b` | 前端类型检查 |
| `cargo test --workspace` | Rust 测试（core + 壳；dict 管线用例需要 `libime-bin`） |

Linux 构建依赖（Debian/Ubuntu）：

```bash
sudo apt-get install -y build-essential pkg-config libx11-dev libasound2-dev libudev-dev \
  libwebkit2gtk-4.1-dev libgtk-3-dev libayatana-appindicator3-dev librsvg2-dev patchelf
```

CI（GitHub Actions）：前端矩阵（ubuntu/windows）跑类型检查 + 测试 + 构建；rust job 装 `libime-bin` 后跑 `cargo test --workspace`；`v*` tag 触发三平台发布。

## 路线图

- [ ] 同步前 diff 预览——推送 / 拉取前先看这次会合并进哪些词条
- [ ] WebDAV 通道接入设置 UI——core 已内置 `WebDavTransport`（坚果云原生协议），只差壳上的配置界面
- [ ] Windows / macOS 支持——覆盖 fcitx5 在非 Linux 平台（如 fcitx5-android 数据迁移）的使用场景

## License

MIT
