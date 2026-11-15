//! fcitx5-sync 桌面壳：托盘 + 命令。所有同步逻辑在 `fcitx5-sync-core`，
//! 本 crate 只做参数组装、状态管理与 UI 事件。

mod settings;

use fcitx5_sync_core::dict;
use fcitx5_sync_core::engine;
use fcitx5_sync_core::pipeline::{self, PullReport, PushReport, SyncJob};
use serde::Serialize;
use settings::Settings;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};
use tauri::{
    menu::{MenuBuilder, MenuItem},
    tray::TrayIconBuilder,
    Emitter, Manager, WindowEvent,
};
use tempfile::TempDir;
use tauri_plugin_notification::NotificationExt;

/// 应用状态：设置 + 最近一次同步记录。
struct AppState {
    settings_path: PathBuf,
    settings: Mutex<Settings>,
    last_push: Mutex<Option<String>>,
    last_pull: Mutex<Option<String>>,
    /// 串行化同步核心：自动调度与手动触发可能并发，避免同时写本机文件
    sync_lock: Mutex<()>,
    /// 是否已有自动同步调度线程存活（防止重复启动）
    auto_sync_running: AtomicBool,
}

#[derive(Clone, Serialize)]
struct Status {
    fcitx5_running: bool,
    dict_tool_found: bool,
    remote_dir: String,
    auto_sync_interval_min: u32,
    notify_on_sync: bool,
    dict_exists: bool,
    history_exists: bool,
    last_push: Option<String>,
    last_pull: Option<String>,
}

fn now_hms() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    format!("{}:{:02}:{:02}", (secs / 3600 + 8) % 24, (secs / 60) % 60, secs % 60) // 近似本地时间(UTC+8)
}

fn build_job(app: &tauri::AppHandle, workdir: &TempDir) -> Result<SyncJob, String> {
    let data_dir = settings::fcitx5_data_dir();
    let backup_dir = app
        .path()
        .app_data_dir()
        .map_err(|e| e.to_string())?
        .join("backups");
    Ok(SyncJob {
        dict_path: data_dir.join("user.dict"),
        history_path: data_dir.join("user.history"),
        backup_dir,
        workdir: workdir.path().to_path_buf(),
    })
}

/// 执行推送/拉取并更新状态；不发事件、不弹通知，由调用方决定。
/// 手动路径与自动调度共用；`sync_lock` 保证同一时刻只有一轮在写本机文件。
fn run_sync_inner(app: &tauri::AppHandle, kind: &str) -> Result<String, String> {
    let state = app.state::<AppState>();
    let _syncing = state.sync_lock.lock().unwrap();
    let remote_dir = {
        let s = state.settings.lock().unwrap().clone();
        s.remote_dir
    };
    if remote_dir.is_empty() {
        return Err("请先在设置里填同步目录".into());
    }
    let transport = fcitx5_sync_core::transport::DirTransport::new(PathBuf::from(&remote_dir));
    let workdir = TempDir::new().map_err(|e| e.to_string())?;
    let job = build_job(app, &workdir)?;

    let summary = match kind {
        "push" => {
            let report: PushReport = pipeline::push_to(&transport, &job).map_err(|e| e.to_string())?;
            let text = format!("已推送 {}", report.pushed.join("、"));
            state.last_push.lock().unwrap().replace(now_hms());
            text
        }
        "pull" => {
            let report: PullReport = pipeline::pull_from(&transport, &job).map_err(|e| e.to_string())?;
            let text = if report.applied {
                let n = report.backups.len();
                if report.reloaded {
                    format!("已写回本机（{} 个备份），并已通知 fcitx5 重载", n)
                } else if engine::fcitx5_running() {
                    format!("已写回本机（{} 个备份）；fcitx5 重载失败，建议重启输入法", n)
                } else {
                    format!("已写回本机（{} 个备份）", n)
                }
            } else {
                "远端与本机一致，无需更新".into()
            };
            state.last_pull.lock().unwrap().replace(now_hms());
            text
        }
        _ => return Err("unknown sync kind".into()),
    };
    Ok(summary)
}

/// 手动推送/拉取入口（托盘菜单 / 面板按钮）：执行后发 `sync-done` 事件并按设置弹通知。
fn run_sync(app: &tauri::AppHandle, kind: &str) -> Result<String, String> {
    let summary = run_sync_inner(app, kind)?;
    let _ = app.emit("sync-done", kind);
    let notify = app
        .state::<AppState>()
        .settings
        .lock()
        .unwrap()
        .notify_on_sync;
    if notify {
        app.notification()
            .builder()
            .title("Fcitx5 Sync")
            .body(&summary)
            .show()
            .ok();
    }
    Ok(summary)
}

/// 一轮双向同步（自动调度专用入口）：先 pull（远端合并写回本机，含备份与重载），
/// 再把超集 push 回远端。整轮只发一次 `sync-done`、按设置只弹一次通知；
/// 未配置同步目录、或本机与远端都还没有词库数据时静默跳过本轮。
/// pull 失败则本轮中止（下一轮重试）；错误通过 `sync-error` 事件上报前端。
fn sync_bidirectional(app: &tauri::AppHandle) -> Result<String, String> {
    let (remote_dir, notify) = {
        let s = app.state::<AppState>().settings.lock().unwrap().clone();
        (s.remote_dir, s.notify_on_sync)
    };
    if remote_dir.is_empty() {
        return Ok(String::new()); // 未配置同步目录：本轮静默跳过
    }
    let data_dir = settings::fcitx5_data_dir();
    let remote = PathBuf::from(&remote_dir);
    let has_local = data_dir.join("user.dict").exists() || data_dir.join("user.history").exists();
    let has_remote = remote.join("user.dict").exists() || remote.join("user.history").exists();
    if !has_local && !has_remote {
        return Ok(String::new()); // 两边都无数据：无事可同步，静默跳过
    }

    let pull_summary = run_sync_inner(app, "pull")?;
    let push_summary = run_sync_inner(app, "push")?;
    let summary = format!("自动同步：{pull_summary}；{push_summary}");
    let _ = app.emit("sync-done", "auto");
    if notify {
        app.notification()
            .builder()
            .title("Fcitx5 Sync")
            .body(&summary)
            .show()
            .ok();
    }
    Ok(summary)
}

/// 自动同步调度：确保有一个后台线程按 `auto_sync_interval_min` 分钟执行一轮双向同步。
/// interval 为 0（关闭）时不启动；已有调度线程存活时也不重复启动
/// （存活线程每秒重读设置，会自行适应变更）。
fn spawn_auto_sync(app: &tauri::AppHandle) {
    let state = app.state::<AppState>();
    if state.settings.lock().unwrap().auto_sync_interval_min == 0 {
        return; // 关闭状态：不启动线程
    }
    if state.auto_sync_running.swap(true, Ordering::SeqCst) {
        return; // 已有调度线程在跑
    }
    let handle = app.clone();
    let spawned = std::thread::Builder::new()
        .name("auto-sync".into())
        .spawn(move || auto_sync_loop(handle));
    if spawned.is_err() {
        // 线程没起出来：归还「无调度线程」状态，下次设置变更可重试
        state.auto_sync_running.store(false, Ordering::SeqCst);
        eprintln!("启动自动同步线程失败: {:?}", spawned.err());
    }
}

/// 调度循环：每秒重读设置——interval 从 >0 改为 0 时当秒退出；间隔变更即时
/// 影响下一轮时刻；目录、通知开关等设置变更均在下一轮生效。
fn auto_sync_loop(app: tauri::AppHandle) {
    // 把线程启动时刻视为「上一轮结束」：首轮在启动后 interval 分钟执行
    let mut last_round = Instant::now();
    loop {
        let interval_min = app
            .state::<AppState>()
            .settings
            .lock()
            .unwrap()
            .auto_sync_interval_min;
        if interval_min == 0 {
            break; // 设置改为 0：线程退出，不再有下一轮
        }
        let due = last_round + Duration::from_secs(u64::from(interval_min) * 60);
        if Instant::now() >= due {
            if let Err(e) = sync_bidirectional(&app) {
                let _ = app.emit("sync-error", e);
            }
            last_round = Instant::now();
        } else {
            std::thread::sleep(Duration::from_secs(1));
        }
    }
    app.state::<AppState>()
        .auto_sync_running
        .store(false, Ordering::SeqCst);
}

#[tauri::command]
fn get_status(app: tauri::AppHandle) -> Status {
    let state = app.state::<AppState>();
    let s = state.settings.lock().unwrap().clone();
    let data_dir = settings::fcitx5_data_dir();
    Status {
        fcitx5_running: engine::fcitx5_running(),
        dict_tool_found: dict::resolve_tool_path().is_some(),
        remote_dir: s.remote_dir,
        auto_sync_interval_min: s.auto_sync_interval_min,
        notify_on_sync: s.notify_on_sync,
        dict_exists: data_dir.join("user.dict").exists(),
        history_exists: data_dir.join("user.history").exists(),
        last_push: state.last_push.lock().unwrap().clone(),
        last_pull: state.last_pull.lock().unwrap().clone(),
    }
}

#[tauri::command]
async fn push_now(app: tauri::AppHandle) -> Result<String, String> {
    // 阻塞式同步核心放线程池，避免卡 UI
    let res = tauri::async_runtime::spawn_blocking(move || run_sync(&app, "push"))
        .await
        .map_err(|e| format!("任务失败: {e}"))?;
    res
}

#[tauri::command]
async fn pull_now(app: tauri::AppHandle) -> Result<String, String> {
    let res = tauri::async_runtime::spawn_blocking(move || run_sync(&app, "pull"))
        .await
        .map_err(|e| format!("任务失败: {e}"))?;
    res
}

#[tauri::command]
fn get_settings(app: tauri::AppHandle) -> Settings {
    app.state::<AppState>().settings.lock().unwrap().clone()
}

#[tauri::command]
fn set_settings(app: tauri::AppHandle, settings: Settings) -> Result<(), String> {
    let state = app.state::<AppState>();
    let path = state.settings_path.clone();
    settings.save(&path)?;
    *state.settings.lock().unwrap() = settings;
    // 从关闭改为开启（或已是开启但无线程）时补启调度线程；
    // 已有线程存活时由它自行重读设置，下一轮生效
    spawn_auto_sync(&app);
    Ok(())
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_notification::init())
        .setup(|app| {
            let settings_path = app
                .path()
                .app_data_dir()
                .expect("no app data dir")
                .join("settings.json");
            let initial = Settings::load(&settings_path);
            app.manage(AppState {
                settings_path,
                settings: Mutex::new(initial),
                last_push: Mutex::new(None),
                last_pull: Mutex::new(None),
                sync_lock: Mutex::new(()),
                auto_sync_running: AtomicBool::new(false),
            });

            // 自动同步调度：interval > 0 时启动后台线程；每轮重读设置，改 0 即退出
            spawn_auto_sync(app.handle());

            // 托盘菜单
            let push = MenuItem::with_id(app, "push", "立即推送（本机 → 同步目录）", true, None::<&str>)?;
            let pull = MenuItem::with_id(app, "pull", "立即拉取（同步目录 → 本机）", true, None::<&str>)?;
            let show = MenuItem::with_id(app, "show", "打开面板", true, None::<&str>)?;
            let quit = MenuItem::with_id(app, "quit", "退出", true, None::<&str>)?;
            let menu = MenuBuilder::new(app).items(&[&push, &pull, &show, &quit]).build()?;

            TrayIconBuilder::with_id("main-tray")
                .icon(app.default_window_icon().expect("no icon").clone())
                .tooltip("Fcitx5 Sync")
                .menu(&menu)
                .on_menu_event(|app, event| match event.id().as_ref() {
                    "push" | "pull" => {
                        let app = app.clone();
                        let kind = event.id().as_ref().to_string();
                        tauri::async_runtime::spawn_blocking(move || {
                            if let Err(e) = run_sync(&app, &kind) {
                                let _ = app.emit("sync-error", e);
                            }
                        });
                    }
                    "show" => {
                        if let Some(w) = app.get_webview_window("main") {
                            let _ = w.show();
                            let _ = w.set_focus();
                        }
                    }
                    "quit" => {
                        app.exit(0);
                    }
                    _ => {}
                })
                .build(app)?;
            Ok(())
        })
        .on_window_event(|window, event| {
            // 关窗 = 隐藏到托盘，真正的退出走托盘菜单
            if let WindowEvent::CloseRequested { api, .. } = event {
                api.prevent_close();
                let _ = window.hide();
            }
        })
        .invoke_handler(tauri::generate_handler![
            get_status,
            push_now,
            pull_now,
            get_settings,
            set_settings
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

#[cfg(test)]
mod tests {
    #[test]
    fn shell_links_core() {
        // 壳编译 + core 依赖可调用
        let _ = fcitx5_sync_core::engine::fcitx5_running();
    }
}
