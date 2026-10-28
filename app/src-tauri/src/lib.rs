//! musicReveal 桌面应用：Tauri 外壳，把 `engine` 暴露成命令与 `snapshot` 事件。

pub mod engine;

use engine::{AddFolderResult, Engine, RealConverter, Snapshot};
use std::sync::Arc;
use tauri::{Emitter, Manager, State};

type R<T> = Result<T, String>;

#[tauri::command]
async fn get_snapshot(e: State<'_, Engine>) -> R<Snapshot> {
    Ok(e.snapshot())
}

#[tauri::command]
async fn complete_onboarding(
    e: State<'_, Engine>,
    folders: Vec<String>,
    output_dir: String,
    format: String,
) -> R<()> {
    e.complete_onboarding(folders, output_dir, &format)
}

#[tauri::command]
async fn add_watch_folder(e: State<'_, Engine>, path: String) -> R<AddFolderResult> {
    e.add_watch_folder(&path)
}

#[tauri::command]
async fn remove_watch_folder(e: State<'_, Engine>, path: String) -> R<()> {
    e.remove_watch_folder(&path);
    Ok(())
}

#[tauri::command]
async fn convert_existing(e: State<'_, Engine>, path: String) -> R<()> {
    e.convert_existing(&path);
    Ok(())
}

#[tauri::command]
async fn ignore_existing(e: State<'_, Engine>, path: String) -> R<()> {
    e.ignore_existing(&path);
    Ok(())
}

#[tauri::command]
async fn set_output_dir(e: State<'_, Engine>, path: String) -> R<()> {
    e.set_output_dir(&path)
}

#[tauri::command]
async fn set_format(e: State<'_, Engine>, format: String) -> R<()> {
    e.set_format(&format)
}

#[tauri::command]
async fn add_files(e: State<'_, Engine>, paths: Vec<String>) -> R<()> {
    e.add_files(paths);
    Ok(())
}

#[tauri::command]
async fn retry_job(e: State<'_, Engine>, id: String) -> R<()> {
    e.retry_job(&id);
    Ok(())
}

#[tauri::command]
async fn remove_job(e: State<'_, Engine>, id: String) -> R<()> {
    e.remove_job(&id);
    Ok(())
}

#[tauri::command]
async fn reveal_in_finder(e: State<'_, Engine>, id: String) -> R<()> {
    e.reveal_in_finder(&id)
}

#[tauri::command]
async fn play_preview(e: State<'_, Engine>, id: String) -> R<()> {
    e.play_preview(&id)
}

#[tauri::command]
async fn stop_preview(e: State<'_, Engine>) -> R<()> {
    e.stop_preview();
    Ok(())
}

#[tauri::command]
async fn open_auth_settings(e: State<'_, Engine>) -> R<()> {
    e.open_auth_settings()
}

#[tauri::command]
async fn dismiss_notice(e: State<'_, Engine>, id: String) -> R<()> {
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

pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .setup(|app| {
            let data = app.path().app_data_dir()?;
            let h = app.handle().clone();
            let eng = Engine::new(
                data,
                Arc::new(RealConverter::new(engine::home())),
                Box::new(move |s| {
                    let _ = h.emit("snapshot", s);
                }),
            );
            tauri::async_runtime::block_on(eng.start());
            app.manage(eng);
            Ok(())
        })
        .on_window_event(|w, ev| {
            // 关窗即退出：先取消当前任务并清理半成品
            if let tauri::WindowEvent::CloseRequested { api, .. } = ev {
                api.prevent_close();
                let _ = w.hide();
                let app = w.app_handle().clone();
                tauri::async_runtime::spawn(async move {
                    app.state::<Engine>().shutdown().await;
                    app.exit(0);
                });
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
            pick_folder,
            dismiss_notice,
        ])
        .run(tauri::generate_context!())
        .expect("musicReveal 启动失败");
}
