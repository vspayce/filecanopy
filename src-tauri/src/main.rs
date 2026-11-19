// Canopy: folders side by side on a canvas, linked to where they came from.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod index;
mod ops;

use serde::Serialize;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::UNIX_EPOCH;
use tauri::State;

pub struct AppState {
    pub index: Arc<index::Index>,
}

#[derive(Serialize)]
pub struct Item {
    pub name: String,
    pub path: String,
    pub dir: bool,
    pub size: Option<u64>,
    pub modified: Option<u64>,
    pub hidden: bool,
}

#[cfg(windows)]
fn is_hidden(name: &str, meta: &std::fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;
    const HIDDEN: u32 = 0x2;
    const SYSTEM: u32 = 0x4;
    name.starts_with('.') || meta.file_attributes() & (HIDDEN | SYSTEM) != 0
}

#[cfg(not(windows))]
fn is_hidden(name: &str, _meta: &std::fs::Metadata) -> bool {
    name.starts_with('.')
}

fn modified_ms(meta: &std::fs::Metadata) -> Option<u64> {
    meta.modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as u64)
}

#[tauri::command]
fn drives() -> Vec<String> {
    #[cfg(windows)]
    {
        (b'A'..=b'Z')
            .map(|c| format!("{}:\\", c as char))
            .filter(|d| Path::new(d).exists())
            .collect()
    }
    #[cfg(not(windows))]
    {
        vec!["/".to_string()]
    }
}

#[tauri::command]
fn home() -> String {
    std::env::var("USERPROFILE")
        .or_else(|_| std::env::var("HOME"))
        .unwrap_or_else(|_| "C:\\".into())
}

#[tauri::command]
fn list_dir(path: String, state: State<AppState>) -> Result<Vec<Item>, String> {
    let rd = std::fs::read_dir(&path).map_err(|e| e.to_string())?;
    let mut out = Vec::new();
    for entry in rd.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        let p = entry.path();
        // symlink_metadata so a junction isn't followed just to list it
        let Ok(meta) = entry.metadata().or_else(|_| std::fs::symlink_metadata(&p)) else {
            continue;
        };
        let dir = meta.is_dir();
        let ps = p.to_string_lossy().to_string();
        let size = if dir { state.index.dir_size(&ps) } else { Some(meta.len()) };
        out.push(Item {
            hidden: is_hidden(&name, &meta),
            modified: modified_ms(&meta),
            name,
            path: ps,
            dir,
            size,
        });
    }
    out.sort_by(|a, b| {
        b.dir
            .cmp(&a.dir)
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });
    Ok(out)
}

#[tauri::command]
fn index_status(state: State<AppState>) -> index::Status {
    state.index.status()
}

#[tauri::command]
fn search(query: String, limit: Option<usize>, state: State<AppState>) -> Vec<Item> {
    state
        .index
        .search(&query, limit.unwrap_or(300))
        .into_iter()
        .map(|h| Item {
            name: Path::new(&h.path)
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_else(|| h.path.clone()),
            path: h.path,
            dir: h.dir,
            size: Some(h.size),
            modified: None,
            hidden: false,
        })
        .collect()
}

#[tauri::command]
fn open_path(path: String) -> Result<(), String> {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // "start" via cmd handles any file type with its default app
        std::process::Command::new("cmd")
            .raw_arg(format!("/C start \"\" \"{}\"", path))
            .creation_flags(0x08000000) // CREATE_NO_WINDOW
            .spawn()
            .map_err(|e| e.to_string())?;
    }
    #[cfg(not(windows))]
    {
        std::process::Command::new("xdg-open")
            .arg(&path)
            .spawn()
            .map_err(|e| e.to_string())?;
    }
    Ok(())
}

#[tauri::command]
fn reveal(path: String) -> Result<(), String> {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        std::process::Command::new("explorer")
            .raw_arg(format!("/select,\"{}\"", path))
            .spawn()
            .map_err(|e| e.to_string())?;
    }
    #[cfg(not(windows))]
    {
        let parent = PathBuf::from(&path)
            .parent()
            .map(|p| p.to_path_buf())
            .unwrap_or_default();
        std::process::Command::new("xdg-open")
            .arg(parent)
            .spawn()
            .map_err(|e| e.to_string())?;
    }
    Ok(())
}

#[tauri::command]
fn transfer(paths: Vec<String>, dest: String, mode: String) -> Vec<ops::Done> {
    ops::transfer(&paths, &dest, &mode)
}

#[tauri::command]
fn undo_moves(moves: Vec<ops::Done>) -> Vec<String> {
    ops::undo_moves(&moves)
}

#[tauri::command]
fn new_folder(dir: String) -> Result<String, String> {
    let p = ops::unique_dest(Path::new(&dir), "New folder");
    std::fs::create_dir(&p).map_err(|e| e.to_string())?;
    Ok(p.to_string_lossy().to_string())
}

#[tauri::command]
fn rename(path: String, new_name: String) -> Result<String, String> {
    if new_name.is_empty() || new_name.contains(['\\', '/', ':', '*', '?', '"', '<', '>', '|']) {
        return Err("That name has characters Windows doesn't allow.".into());
    }
    let src = PathBuf::from(&path);
    let dst = src.with_file_name(&new_name);
    if dst.exists() {
        return Err("Something with that name already exists here.".into());
    }
    std::fs::rename(&src, &dst).map_err(|e| e.to_string())?;
    Ok(dst.to_string_lossy().to_string())
}

#[tauri::command]
fn reindex(state: State<AppState>) {
    state.index.clone().start(drives());
}

fn main() {
    let idx = Arc::new(index::Index::default());
    idx.clone().start(drives());

    tauri::Builder::default()
        .plugin(tauri_plugin_drag::init())
        .manage(AppState { index: idx })
        .invoke_handler(tauri::generate_handler![
            drives,
            home,
            list_dir,
            index_status,
            search,
            open_path,
            reveal,
            transfer,
            undo_moves,
            new_folder,
            rename,
            reindex
        ])
        .run(tauri::generate_context!())
        .expect("error while running Canopy");
}
