//! musicReveal 桌面应用：Tauri 外壳，把 `engine` 暴露成命令与 `snapshot` 事件。

pub mod engine;
pub mod library;

use engine::{AddFolderResult, Engine, RealConverter, Snapshot};
use std::sync::Arc;
use tauri::{AppHandle, Emitter, Manager, RunEvent, State};

type R<T> = Result<T, String>;

#[tauri::command]
async fn get_snapshot(e: State<'_, Engine>) -> R<Snapshot> {
    e.ready().await;
    Ok(e.snapshot())
}

#[tauri::command]
async fn complete_onboarding(
    e: State<'_, Engine>,
    folders: Vec<String>,
    output_dir: String,
    format: String,
) -> R<()> {
    e.ready().await;
    e.complete_onboarding(folders, output_dir, &format)
}

#[tauri::command]
async fn add_watch_folder(e: State<'_, Engine>, path: String) -> R<AddFolderResult> {
    e.ready().await;
    e.add_watch_folder(&path)
}

#[tauri::command]
async fn remove_watch_folder(e: State<'_, Engine>, path: String) -> R<()> {
    e.ready().await;
    e.remove_watch_folder(&path);
    Ok(())
}

#[tauri::command]
async fn convert_existing(e: State<'_, Engine>, path: String) -> R<()> {
    e.ready().await;
    e.convert_existing(&path);
    Ok(())
}

#[tauri::command]
async fn ignore_existing(e: State<'_, Engine>, path: String) -> R<()> {
    e.ready().await;
    e.ignore_existing(&path);
    Ok(())
}

#[tauri::command]
async fn set_output_dir(e: State<'_, Engine>, path: String) -> R<()> {
    e.ready().await;
    e.set_output_dir(&path)
}

#[tauri::command]
async fn set_format(e: State<'_, Engine>, format: String) -> R<()> {
    e.ready().await;
    e.set_format(&format)
}

#[tauri::command]
async fn add_files(e: State<'_, Engine>, paths: Vec<String>) -> R<()> {
    e.ready().await;
    e.add_files(paths);
    Ok(())
}

#[tauri::command]
async fn retry_job(e: State<'_, Engine>, id: String) -> R<()> {
    e.ready().await;
    e.retry_job(&id);
    Ok(())
}

#[tauri::command]
async fn remove_job(e: State<'_, Engine>, id: String) -> R<()> {
    e.ready().await;
    e.remove_job(&id);
    Ok(())
}

#[tauri::command]
async fn reveal_in_finder(e: State<'_, Engine>, id: String) -> R<()> {
    e.ready().await;
    e.reveal_in_finder(&id)
}

#[tauri::command]
async fn play_preview(e: State<'_, Engine>, id: String) -> R<()> {
    e.ready().await;
    e.play_preview(&id)
}

#[tauri::command]
async fn stop_preview(e: State<'_, Engine>) -> R<()> {
    e.ready().await;
    e.stop_preview();
    Ok(())
}

#[tauri::command]
async fn open_auth_settings(e: State<'_, Engine>) -> R<()> {
    e.ready().await;
    e.open_auth_settings()
}

#[tauri::command]
async fn open_qq_music(e: State<'_, Engine>) -> R<()> {
    e.ready().await;
    e.open_qq_music()
}

#[tauri::command]
async fn dismiss_notice(e: State<'_, Engine>, id: String) -> R<()> {
    e.ready().await;
    e.dismiss_notice(&id);
    Ok(())
}

#[tauri::command]
async fn pick_folder(app: tauri::AppHandle) -> R<Option<String>> {
    use tauri_plugin_dialog::DialogExt;
    let (tx, rx) = tokio::sync::oneshot::channel();
    app.dialog().file().pick_folder(move |p| {
        let _ = tx.send(p);
    });
    Ok(rx
        .await
        .ok()
        .flatten()
        .and_then(|p| p.into_path().ok())
        .map(|p| p.to_string_lossy().into_owned()))
}

/// 真机取证用的环境变量，只在 debug 构建里存在。
#[cfg(debug_assertions)]
mod dev {
    pub fn var(name: &str) -> Option<String> {
        std::env::var(name).ok().filter(|v| !v.is_empty())
    }

    pub fn background() -> bool {
        var("MUSICREVEAL_BACKGROUND").as_deref() == Some("1")
    }

    /// MUSICREVEAL_THEME=light|dark：只改本应用窗口，不动系统外观。
    pub fn apply(app: &mut tauri::App) {
        use tauri::Manager;
        let theme = match var("MUSICREVEAL_THEME").as_deref() {
            Some("dark") => Some(tauri::Theme::Dark),
            Some("light") => Some(tauri::Theme::Light),
            _ => None,
        };
        if theme.is_some() {
            for w in app.webview_windows().values() {
                let _ = w.set_theme(theme);
            }
        }
        // 窗口照常显示，但不抢前台、不进 Dock
        if background() {
            app.set_activation_policy(tauri::ActivationPolicy::Accessory);
        }
    }
}

pub fn run() {
    let builder = tauri::Builder::default();
    // 启动时不 activateIgnoringOtherApps：别的应用在前台时不把它挤下去
    #[cfg(debug_assertions)]
    let builder = builder.activate_ignoring_other_apps(!dev::background());
    builder
        .plugin(tauri_plugin_dialog::init())
        .setup(|app| {
            #[allow(unused_mut)]
            let mut data = app.path().app_data_dir()?;
            #[cfg(debug_assertions)]
            {
                if let Some(d) = dev::var("MUSICREVEAL_DATA_DIR") {
                    data = d.into();
                }
                dev::apply(app);
            }
            app.asset_protocol_scope()
                .allow_directory(data.join("covers"), true)?;
            let h = app.handle().clone();
            let eng = Engine::new(
                data,
                Arc::new(RealConverter::new(engine::home())),
                Box::new(move |s| {
                    let _ = h.emit("snapshot", s);
                }),
            );
            app.manage(eng.clone());
            // 不在 setup 里同步等：命令会先等 start 完成
            tauri::async_runtime::spawn(async move { eng.start().await });
            Ok(())
        })
        .on_window_event(|w, ev| {
            // 关窗即退出：先取消当前任务并清理半成品
            if let tauri::WindowEvent::CloseRequested { api, .. } = ev {
                api.prevent_close();
                let _ = w.hide();
                quit(w.app_handle().clone());
            }
        })
        .invoke_handler(tauri::generate_handler![
            get_snapshot,
            complete_onboarding,
            add_watch_folder,
            remove_watch_folder,
            convert_existing,
            ignore_existing,
            set_output_dir,
            set_format,
            add_files,
            retry_job,
            remove_job,
            reveal_in_finder,
            play_preview,
            stop_preview,
            open_auth_settings,
            open_qq_music,
            pick_folder,
            dismiss_notice,
        ])
        .build(tauri::generate_context!())
        .expect("musicReveal 启动失败")
        .run(|app, ev| {
            // ⌘Q、Dock 退出、系统注销走 applicationWillTerminate → RunEvent::Exit，
            // 无法阻止；在返回前同步取消当前任务并清理半成品（关窗路径已做过，再调是空操作）。
            if let RunEvent::Exit = ev {
                tauri::async_runtime::block_on(app.state::<Engine>().shutdown());
            }
        });
}

fn quit(app: AppHandle) {
    tauri::async_runtime::spawn(async move {
        app.state::<Engine>().shutdown().await;
        app.exit(0);
    });
}
