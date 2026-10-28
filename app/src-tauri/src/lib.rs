//! musicReveal 桌面应用：Tauri 外壳，把 `engine` 暴露成命令与 `snapshot` 事件，
//! 把 `player` 暴露成 `player_*` 命令与 `player` 事件。

pub mod engine;
pub mod player;

use engine::{AddFolderResult, Engine, RealConverter, Snapshot};
use player::{Player, PlayerState};
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
async fn player_play(p: State<'_, Player>, ids: Vec<String>, index: u32) -> R<()> {
    p.play(ids, index as usize)
}

#[tauri::command]
async fn player_toggle(p: State<'_, Player>) -> R<()> {
    p.toggle();
    Ok(())
}

#[tauri::command]
async fn player_next(p: State<'_, Player>) -> R<()> {
    p.next();
    Ok(())
}

#[tauri::command]
async fn player_prev(p: State<'_, Player>) -> R<()> {
    p.prev();
    Ok(())
}

#[tauri::command]
async fn player_seek(p: State<'_, Player>, ms: u64) -> R<()> {
    p.seek(ms);
    Ok(())
}

#[tauri::command]
async fn player_set_volume(p: State<'_, Player>, volume: f32) -> R<()> {
    p.set_volume(volume);
    Ok(())
}

#[tauri::command]
async fn player_set_shuffle(p: State<'_, Player>, on: bool) -> R<()> {
    p.set_shuffle(on);
    Ok(())
}

#[tauri::command]
async fn player_set_repeat(p: State<'_, Player>, mode: String) -> R<()> {
    p.set_repeat(mode.parse()?);
    Ok(())
}

#[tauri::command]
async fn get_player(p: State<'_, Player>) -> R<PlayerState> {
    Ok(p.state())
}

#[tauri::command]
async fn open_settings(app: AppHandle) -> R<()> {
    show_settings(&app).map_err(|e| format!("无法打开设置：{e}"))
}

/// 设置窗口：已开就聚焦，不重复开。
fn show_settings(app: &AppHandle) -> tauri::Result<()> {
    if let Some(w) = app.get_webview_window("settings") {
        w.show()?;
        return w.set_focus();
    }
    tauri::WebviewWindowBuilder::new(
        app,
        "settings",
        tauri::WebviewUrl::App("index.html?view=settings".into()),
    )
    .title("设置")
    .inner_size(520.0, 440.0)
    .resizable(false)
    .build()
    .map(drop)
}

/// 标准菜单，应用菜单里加「设置…」⌘,。
fn menu(app: &AppHandle) -> tauri::Result<tauri::menu::Menu<tauri::Wry>> {
    use tauri::menu::{Menu, MenuItem, PredefinedMenuItem};
    let menu = Menu::default(app)?;
    if let Some(first) = menu.items()?.first().and_then(|i| i.as_submenu().cloned()) {
        // 「关于」与分隔线之后
        first.insert_items(
            &[
                &MenuItem::with_id(app, "settings", "设置…", true, Some("CmdOrCtrl+,"))?,
                &PredefinedMenuItem::separator(app)?,
            ],
            2,
        )?;
    }
    Ok(menu)
}

fn show_main(app: &AppHandle) {
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.show();
        let _ = w.set_focus();
    }
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
            let data_dir = data.clone();
            let eng = Engine::new(
                data,
                Arc::new(RealConverter::new(engine::home())),
                Box::new(move |s| {
                    let _ = h.emit("snapshot", s);
                }),
            );
            app.manage(eng.clone());
            let h = app.handle().clone();
            let player = Player::new(
                data_dir,
                Box::new(move |s| {
                    let _ = h.emit("player", s);
                }),
            );
            if let Err(e) = player.enable_media_controls() {
                eprintln!("{e}");
            }
            app.manage(player);
            // 不在 setup 里同步等：命令会先等 start 完成
            tauri::async_runtime::spawn(async move { eng.start().await });
            Ok(())
        })
        .menu(menu)
        .on_menu_event(|app, ev| {
            if ev.id() == "settings" {
                let _ = show_settings(app);
            }
        })
        .on_window_event(|w, ev| {
            // 关主窗口只是隐藏：播放与转换照常进行，点 Dock 图标再显示，⌘Q 才退出
            if let tauri::WindowEvent::CloseRequested { api, .. } = ev {
                if w.label() == "main" {
                    api.prevent_close();
                    let _ = w.hide();
                }
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
            player_play,
            player_toggle,
            player_next,
            player_prev,
            player_seek,
            player_set_volume,
            player_set_shuffle,
            player_set_repeat,
            get_player,
            open_settings,
            open_auth_settings,
            open_qq_music,
            pick_folder,
            dismiss_notice,
        ])
        .build(tauri::generate_context!())
        .expect("musicReveal 启动失败")
        .run(|app, ev| match ev {
            // ⌘Q、Dock 退出、系统注销走 applicationWillTerminate → RunEvent::Exit，
            // 无法阻止；在返回前先停播，再同步取消当前任务并清理半成品。
            RunEvent::Exit => {
                app.state::<Player>().stop();
                tauri::async_runtime::block_on(app.state::<Engine>().shutdown());
            }
            #[cfg(target_os = "macos")]
            RunEvent::Reopen { .. } => show_main(app),
            _ => {}
        });
}
