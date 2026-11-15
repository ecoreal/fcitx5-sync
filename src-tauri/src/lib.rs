//! fcitx5-sync 桌面壳：托盘 + 命令。所有同步逻辑在 `fcitx5-sync-core`，
//! 本 crate 只做参数组装、状态管理与 UI 事件。

mod settings;

use fcitx5_sync_core::dict;
use fcitx5_sync_core::engine;
use fcitx5_sync_core::pipeline::{self, PullReport, PushReport, SyncJob};
use serde::Serialize;
use settings::Settings;
use std::path::PathBuf;
use std::sync::Mutex;
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

/// 执行推送/拉取并更新状态 + 通知。
fn run_sync(app: &tauri::AppHandle, kind: &str) -> Result<String, String> {
    let state = app.state::<AppState>();
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

    let _ = app.emit("sync-done", kind);
    let notify = state.settings.lock().unwrap().notify_on_sync;
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
            });

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
