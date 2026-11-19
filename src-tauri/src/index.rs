// A local index of every file and folder on the drives, so folder sizes
// show at once and search answers as you type. Built by walking each drive
// in the background; nothing leaves the machine.
//
// Later: read the NTFS master file table and change journal directly (what
// "Everything" does) to build it in seconds and keep it live.

use serde::Serialize;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use walkdir::WalkDir;

pub struct Entry {
    pub path: Box<str>,
    pub name_lower: Box<str>,
    pub size: u64,
    pub dir: bool,
}

#[derive(Default)]
struct Data {
    entries: Vec<Entry>,
    dir_sizes: HashMap<Box<str>, u64>,
}

#[derive(Default)]
pub struct Index {
    data: RwLock<Data>,
    scanned: AtomicUsize,
    running: AtomicUsize,
    started: AtomicBool,
    busy: Mutex<()>,
}

#[derive(Serialize)]
pub struct Status {
    pub scanned: usize,
    pub indexed: usize,
    pub building: bool,
    pub started: bool,
}

pub struct Hit {
    pub path: String,
    pub size: u64,
    pub dir: bool,
}

/// Paths compare case-insensitively on Windows, and "C:\" and "C:" are the same.
pub fn key(path: &str) -> String {
    let mut k = path.replace('/', "\\").to_lowercase();
    while k.len() > 3 && k.ends_with('\\') {
        k.pop();
    }
    if k.len() == 2 && k.ends_with(':') {
        k.push('\\');
    }
    k
}

impl Index {
    pub fn status(&self) -> Status {
        Status {
            scanned: self.scanned.load(Ordering::Relaxed),
            indexed: self.data.read().map(|d| d.entries.len()).unwrap_or(0),
            building: self.running.load(Ordering::Relaxed) > 0,
            started: self.started.load(Ordering::Relaxed),
        }
    }

    pub fn dir_size(&self, path: &str) -> Option<u64> {
        let d = self.data.read().ok()?;
        d.dir_sizes.get(key(path).as_str()).copied()
    }

    /// (Re)build the index: one background thread per drive.
    pub fn start(self: Arc<Self>, drives: Vec<String>) {
        let Ok(_guard) = self.busy.try_lock() else { return };
        if self.running.load(Ordering::Relaxed) > 0 {
            return;
        }
        self.started.store(true, Ordering::Relaxed);
        self.scanned.store(0, Ordering::Relaxed);
        if let Ok(mut d) = self.data.write() {
            *d = Data::default();
        }
        for drive in drives {
            self.running.fetch_add(1, Ordering::Relaxed);
            let me = self.clone();
            std::thread::spawn(move || {
                me.walk_drive(&drive);
                me.running.fetch_sub(1, Ordering::Relaxed);
            });
        }
    }

    fn walk_drive(&self, root: &str) {
        // contents_first: a folder comes after everything inside it, so its
        // total size is known when we reach it. acc[k] = running total of the
        // folder currently open at depth k.
        let mut acc: Vec<u64> = vec![0; 64];
        let mut entries: Vec<Entry> = Vec::with_capacity(1 << 16);
        let mut sizes: Vec<(Box<str>, u64)> = Vec::with_capacity(1 << 14);

        for item in WalkDir::new(root).follow_links(false).contents_first(true) {
            let Ok(e) = item else { continue };
            let depth = e.depth();
            if acc.len() < depth + 2 {
                acc.resize(depth + 2, 0);
            }
            let is_dir = e.file_type().is_dir();
            let size = if is_dir {
                let total = acc[depth + 1];
                acc[depth + 1] = 0;
                total
            } else {
                e.metadata().map(|m| m.len()).unwrap_or(0)
            };
            if depth > 0 {
                acc[depth] += size;
            }
            let path = e.path().to_string_lossy();
            if is_dir {
                sizes.push((key(&path).into_boxed_str(), size));
            }
            if depth > 0 {
                entries.push(Entry {
                    name_lower: e.file_name().to_string_lossy().to_lowercase().into_boxed_str(),
                    path: path.into_owned().into_boxed_str(),
                    size,
                    dir: is_dir,
                });
            }
            self.scanned.fetch_add(1, Ordering::Relaxed);

            // publish in batches so search works while the walk continues
            if entries.len() >= 50_000 {
                self.publish(&mut entries, &mut sizes);
            }
        }
        self.publish(&mut entries, &mut sizes);
    }

    fn publish(&self, entries: &mut Vec<Entry>, sizes: &mut Vec<(Box<str>, u64)>) {
        if let Ok(mut d) = self.data.write() {
            d.entries.append(entries);
            d.dir_sizes.extend(sizes.drain(..));
        }
    }

    /// Every word of the query must be in the name. Exact and prefix matches
    /// come first, then folders, then shorter paths.
    pub fn search(&self, query: &str, limit: usize) -> Vec<Hit> {
        let q = query.trim().to_lowercase();
        if q.is_empty() {
            return vec![];
        }
        let words: Vec<&str> = q.split_whitespace().collect();
        let Ok(d) = self.data.read() else { return vec![] };
        let mut hits: Vec<(u8, &Entry)> = Vec::new();
        for e in &d.entries {
            if words.iter().all(|w| e.name_lower.contains(w)) {
                let score = if *e.name_lower == *q {
                    0
                } else if e.name_lower.starts_with(words[0]) {
                    1
                } else {
                    2
                };
                hits.push((score, e));
                if hits.len() >= 50_000 {
                    break;
                }
            }
        }
        hits.sort_by(|a, b| {
            a.0.cmp(&b.0)
                .then_with(|| b.1.dir.cmp(&a.1.dir))
                .then_with(|| a.1.path.len().cmp(&b.1.path.len()))
        });
        hits.into_iter()
            .take(limit)
            .map(|(_, e)| Hit {
                path: e.path.to_string(),
                size: e.size,
                dir: e.dir,
            })
            .collect()
    }
}
