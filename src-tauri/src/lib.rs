mod config;
mod elevate;
mod pty;
mod sessions;

use std::sync::Arc;

use serde::Serialize;
use serde_json::Value;
use tauri::{AppHandle, Emitter, Manager, State};

use pty::{Registry, Sink, Spawned};
use sessions::ProjectInfo;

#[derive(Serialize, Clone)]
struct Output {
    id: u32,
    data: String,
}

#[derive(Serialize, Clone)]
struct Exit {
    id: u32,
    code: Option<u32>,
}

// Передача вывода экземпляров во фронтенд событиями окна
struct EventSink(AppHandle);

impl Sink for EventSink {
    fn output(&self, id: u32, data: String) {
        let _ = self.0.emit("pty-output", Output { id, data });
    }
    fn exit(&self, id: u32, code: Option<u32>) {
        let _ = self.0.emit("pty-exit", Exit { id, code });
    }
}

#[tauri::command]
fn spawn(
    app: AppHandle,
    reg: State<'_, Registry>,
    program: String,
    args: Vec<String>,
    cwd: Option<String>,
    cols: u16,
    rows: u16,
) -> Result<Spawned, String> {
    let args = elevate::expand_args(&args, &cwd_or_home(cwd.as_deref()));
    reg.spawn(Arc::new(EventSink(app)), &program, &args, cwd.as_deref(), cols, rows)
}

// Каталог проекта, а без него - домашний каталог пользователя
fn cwd_or_home(cwd: Option<&str>) -> String {
    cwd.map(str::to_owned)
        .or_else(|| dirs::home_dir().map(|h| h.to_string_lossy().into_owned()))
        .unwrap_or_default()
}

#[tauri::command]
fn launch_elevated(program: String, args: Vec<String>, cwd: Option<String>) -> Result<(), String> {
    let dir = cwd_or_home(cwd.as_deref());
    elevate::launch(&program, &elevate::expand_args(&args, &dir), Some(&dir))
}

#[tauri::command]
fn write(reg: State<'_, Registry>, id: u32, data: String) -> Result<(), String> {
    reg.write(id, data.as_bytes())
}

#[tauri::command]
fn resize(reg: State<'_, Registry>, id: u32, cols: u16, rows: u16) -> Result<(), String> {
    reg.resize(id, cols, rows)
}

#[tauri::command]
fn kill(reg: State<'_, Registry>, id: u32) -> Result<(), String> {
    reg.kill(id)
}

#[tauri::command]
async fn list_projects() -> Vec<ProjectInfo> {
    sessions::list_projects()
}

fn config_path(app: &AppHandle) -> Result<std::path::PathBuf, String> {
    let exe_dir = std::env::current_exe().ok().and_then(|p| p.parent().map(|d| d.to_path_buf()));
    let legacy = app.path().app_config_dir().ok().map(|d| d.join("config.json"));
    config::locate(exe_dir, dirs::home_dir(), legacy).ok_or("не найден домашний каталог".into())
}

#[tauri::command]
fn get_config(app: AppHandle) -> Result<Value, String> {
    Ok(config::load(&config_path(&app)?))
}

#[tauri::command]
fn set_config(app: AppHandle, value: Value) -> Result<(), String> {
    config::save(&config_path(&app)?, &value)
}

// Картинка иконки команды из файла как data URL для фронтенда
#[tauri::command]
fn read_icon(path: String) -> Result<String, String> {
    let p = std::path::Path::new(&path);
    let mime = match p.extension().and_then(|e| e.to_str()).map(|e| e.to_ascii_lowercase()).as_deref() {
        Some("png") => "image/png",
        Some("svg") => "image/svg+xml",
        Some("ico") => "image/x-icon",
        Some("jpg") | Some("jpeg") => "image/jpeg",
        _ => return Err("неизвестный формат иконки".into()),
    };
    let bytes = std::fs::read(p).map_err(|e| e.to_string())?;
    Ok(format!("data:{mime};base64,{}", config::base64(&bytes)))
}

// Положение и размер окна хранятся рядом с config.json (абсолютный путь заменяет каталог плагина)
fn window_state_path() -> std::path::PathBuf {
    let exe_dir = std::env::current_exe().ok().and_then(|p| p.parent().map(|d| d.to_path_buf()));
    config::locate(exe_dir, dirs::home_dir(), None)
        .and_then(|p| p.parent().map(|d| d.join("window-state.json")))
        .unwrap_or_else(|| "window-state.json".into())
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(
            tauri_plugin_window_state::Builder::default()
                .with_filename(window_state_path().to_string_lossy())
                .build(),
        )
        .manage(Registry::default())
        .invoke_handler(tauri::generate_handler![
            spawn,
            write,
            resize,
            kill,
            list_projects,
            get_config,
            set_config,
            read_icon,
            launch_elevated
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
