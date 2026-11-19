// Copy and move, the way Explorer decides: dragging onto the same drive
// moves, onto another drive copies. Never overwrites: a name that's taken
// gets " (2)", " (3)"... Moves can be undone.

use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Serialize, Deserialize, Clone)]
pub struct Done {
    pub from: String,
    pub to: String,
    pub op: String, // "move" | "copy" | "error"
    #[serde(default)]
    pub error: Option<String>,
}

fn drive_of(p: &Path) -> String {
    let s = p.to_string_lossy().to_lowercase();
    if s.starts_with("\\\\") {
        // \\server\share
        s.splitn(5, '\\').take(4).collect::<Vec<_>>().join("\\")
    } else {
        s.chars().take(2).collect()
    }
}

pub fn unique_dest(dir: &Path, name: &str) -> PathBuf {
    let first = dir.join(name);
    if !first.exists() {
        return first;
    }
    let (stem, ext) = match name.rfind('.') {
        Some(i) if i > 0 => (&name[..i], &name[i..]),
        _ => (name, ""),
    };
    (2..)
        .map(|n| dir.join(format!("{stem} ({n}){ext}")))
        .find(|p| !p.exists())
        .unwrap()
}

fn copy_recursive(src: &Path, dst: &Path) -> std::io::Result<()> {
    let meta = fs::symlink_metadata(src)?;
    if meta.is_dir() {
        fs::create_dir_all(dst)?;
        for entry in fs::read_dir(src)? {
            let entry = entry?;
            copy_recursive(&entry.path(), &dst.join(entry.file_name()))?;
        }
    } else {
        fs::copy(src, dst)?;
    }
    Ok(())
}

pub fn transfer(paths: &[String], dest: &str, mode: &str) -> Vec<Done> {
    let dest_dir = PathBuf::from(dest);
    let mut out = Vec::new();
    for p in paths {
        let src = PathBuf::from(p);
        let fail = |e: String| Done { from: p.clone(), to: String::new(), op: "error".into(), error: Some(e) };
        let Some(name) = src.file_name().map(|n| n.to_string_lossy().to_string()) else {
            out.push(fail("Can't move a whole drive.".into()));
            continue;
        };
        if src.parent().map(|par| par == dest_dir).unwrap_or(false) {
            continue; // dropped back where it already is
        }
        if dest_dir.starts_with(&src) {
            out.push(fail(format!("Can't put \"{name}\" inside itself.")));
            continue;
        }
        let same_drive = drive_of(&src) == drive_of(&dest_dir);
        let do_move = match mode {
            "move" => true,
            "copy" => false,
            _ => same_drive,
        };
        let target = unique_dest(&dest_dir, &name);
        let result = if do_move {
            fs::rename(&src, &target).or_else(|_| {
                // across drives a rename fails: copy, then remove the original
                copy_recursive(&src, &target)?;
                if src.is_dir() { fs::remove_dir_all(&src) } else { fs::remove_file(&src) }
            })
        } else {
            copy_recursive(&src, &target)
        };
        match result {
            Ok(()) => out.push(Done {
                from: p.clone(),
                to: target.to_string_lossy().to_string(),
                op: if do_move { "move" } else { "copy" }.into(),
                error: None,
            }),
            Err(e) => out.push(fail(e.to_string())),
        }
    }
    out
}

/// Put moved items back. Returns error messages, if any.
pub fn undo_moves(moves: &[Done]) -> Vec<String> {
    let mut errors = Vec::new();
    for m in moves.iter().filter(|m| m.op == "move") {
        let back = PathBuf::from(&m.from);
        if back.exists() {
            errors.push(format!("\"{}\" exists again, so it was left where it is.", m.from));
            continue;
        }
        let r = fs::rename(&m.to, &back).or_else(|_| {
            copy_recursive(Path::new(&m.to), &back)?;
            let to = Path::new(&m.to);
            if to.is_dir() { fs::remove_dir_all(to) } else { fs::remove_file(to) }
        });
        if let Err(e) = r {
            errors.push(format!("{}: {}", m.to, e));
        }
    }
    errors
}
