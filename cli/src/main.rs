//! canopy-cli: Canopy in the terminal.
//!
//! Folders are drawn as boxes side by side, each linked to the folder it came
//! from. The top line is a numbered path strip (a visible pushd/popd stack):
//! Space + number jumps back to any folder in it.
//!
//! Like the Windows app, this never deletes anything.

use std::{
    collections::HashMap,
    fs,
    io::{self, IsTerminal, Write},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::atomic::{AtomicBool, Ordering},
    time::{Duration, Instant},
};

use ratatui::{
    backend::CrosstermBackend,
    crossterm::{
        cursor,
        event::{
            self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEvent, KeyEventKind,
            KeyModifiers, KeyboardEnhancementFlags, MouseButton, MouseEvent, MouseEventKind,
            PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
        },
        execute,
        terminal::{
            disable_raw_mode, enable_raw_mode, supports_keyboard_enhancement, EnterAlternateScreen,
            LeaveAlternateScreen,
        },
    },
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph},
    Frame, Terminal,
};
use unicode_width::UnicodeWidthChar;

const PANE_W: u16 = 32;
const GAP: u16 = 3;
const DOUBLE_CLICK: Duration = Duration::from_millis(400);

const USAGE: &str = "\
canopy-cli: folders side by side, with a numbered path strip

Usage: canopy-cli [--print-dir] [FOLDER]

  FOLDER        where to start (default: the current folder)
  --print-dir   when you quit with q, print the folder you were in,
                so your shell can cd there (see docs/terminal-version.md)
  -h, --help    show this
  -V, --version show the version

Inside, press ? for keys.";

// ---------------------------------------------------------------- model

#[derive(Clone, Debug)]
struct Entry {
    name: String,
    is_dir: bool,
    size: Option<u64>,
    hidden: bool,
}

struct Pane {
    path: PathBuf,
    entries: Vec<Entry>,
    error: Option<String>,
    /// Index into the pane's *visible* list.
    sel: usize,
    top: usize,
    filter: String,
}

impl Pane {
    fn new(path: PathBuf) -> Self {
        let mut p = Pane {
            path,
            entries: Vec::new(),
            error: None,
            sel: 0,
            top: 0,
            filter: String::new(),
        };
        p.reload();
        p
    }

    fn reload(&mut self) {
        match read_entries(&self.path) {
            Ok(e) => {
                self.entries = e;
                self.error = None;
            }
            Err(e) => {
                self.entries.clear();
                self.error = Some(e);
            }
        }
    }
}

struct PendingDrop {
    src: PathBuf,
    dest: PathBuf,
}

#[derive(Clone, Copy)]
enum InputKind {
    Rename,
    NewFolder,
}

enum Mode {
    Normal,
    Leader,
    Filter,
    Input(InputKind, String),
    Confirm(PendingDrop),
    MarkSet,
    MarkJump,
    Help,
}

struct PaneGeo {
    idx: usize,
    rect: Rect,
    list_y: u16,
    list_h: u16,
    top: usize,
    vis: Vec<usize>,
}

#[derive(Default)]
struct Geo {
    strip: Vec<(Rect, usize)>,
    panes: Vec<PaneGeo>,
    page: usize,
}

struct App {
    panes: Vec<Pane>,
    focus: usize,
    scroll: usize,
    show_hidden: bool,
    mode: Mode,
    count: Option<usize>,
    msg: String,
    clip: Option<(PathBuf, bool)>,
    marks: HashMap<char, PathBuf>,
    quit: bool,
    print_on_quit: bool,
    geo: Geo,
    last_click: Option<(Instant, usize, usize)>,
    drag: Option<(usize, usize)>,
    dragging: bool,
}

impl App {
    fn new(start: &Path) -> Self {
        let mut a = App {
            panes: Vec::new(),
            focus: 0,
            scroll: 0,
            show_hidden: false,
            mode: Mode::Normal,
            count: None,
            msg: "Space + number jumps along the path strip · ? for keys".into(),
            clip: None,
            marks: HashMap::new(),
            quit: false,
            print_on_quit: false,
            geo: Geo::default(),
            last_click: None,
            drag: None,
            dragging: false,
        };
        a.open_path(start);
        a
    }

    // ---- queries

    /// Name of the folder in pane `i` that leads to pane `i + 1`.
    fn link_name(&self, i: usize) -> Option<String> {
        self.panes
            .get(i + 1)
            .and_then(|p| p.path.file_name())
            .map(|n| n.to_string_lossy().into_owned())
    }

    /// Indices into `entries` that pane `i` shows, honouring hidden files and
    /// the filter. The linking folder is always shown.
    fn visible(&self, i: usize) -> Vec<usize> {
        let p = &self.panes[i];
        let link = self.link_name(i);
        let f = p.filter.to_lowercase();
        p.entries
            .iter()
            .enumerate()
            .filter(|(_, e)| {
                let linked = link.as_deref() == Some(e.name.as_str());
                (self.show_hidden || !e.hidden || linked)
                    && (f.is_empty() || linked || e.name.to_lowercase().contains(&f))
            })
            .map(|(k, _)| k)
            .collect()
    }

    fn selected_in(&self, i: usize) -> Option<&Entry> {
        let vis = self.visible(i);
        vis.get(self.panes[i].sel)
            .map(|&k| &self.panes[i].entries[k])
    }

    /// Full path of the focused pane's selected entry, and whether it's a folder.
    fn selected(&self) -> Option<(PathBuf, bool)> {
        let i = self.focus;
        self.selected_in(i)
            .map(|e| (self.panes[i].path.join(&e.name), e.is_dir))
    }

    fn select_name(&mut self, i: usize, name: &str) {
        let vis = self.visible(i);
        if let Some(pos) = vis
            .iter()
            .position(|&k| self.panes[i].entries[k].name == name)
        {
            self.panes[i].sel = pos;
        }
    }

    fn clamp_sel(&mut self, i: usize) {
        let n = self.visible(i).len();
        let p = &mut self.panes[i];
        if p.sel >= n {
            p.sel = n.saturating_sub(1);
        }
    }

    // ---- navigation

    /// Replace the canvas with the chain of folders from the drive root to `path`.
    fn open_path(&mut self, path: &Path) {
        let chain = chain_for(path);
        self.panes = chain.into_iter().map(Pane::new).collect();
        for i in 0..self.panes.len().saturating_sub(1) {
            if let Some(n) = self.link_name(i) {
                self.select_name(i, &n);
            }
        }
        self.focus = self.panes.len() - 1;
        self.scroll = 0;
    }

    fn focus_pane(&mut self, j: usize) {
        if j >= self.panes.len() || j == self.focus {
            return;
        }
        self.focus = j;
        if let Some(l) = self.link_name(j) {
            self.select_name(j, &l);
        }
    }

    fn jump(&mut self, j: usize) {
        if j < self.panes.len() {
            self.focus_pane(j);
        } else {
            self.msg = format!("There's no {} in the path strip", label(j).unwrap_or('?'));
        }
    }

    fn move_sel(&mut self, delta: isize) {
        let n = self.visible(self.focus).len();
        if n == 0 {
            return;
        }
        let p = &mut self.panes[self.focus];
        p.sel = (p.sel as isize + delta).clamp(0, n as isize - 1) as usize;
    }

    fn open_selected(&mut self) {
        let Some((path, is_dir)) = self.selected() else {
            return;
        };
        let name = file_name(&path);
        if !is_dir {
            open_external(&path, false);
            self.msg = format!("Opened {name}");
            return;
        }
        let i = self.focus;
        if self.panes.get(i + 1).is_some_and(|p| p.path == path) {
            // Walking back down a folder that's still open (dimmed).
            self.focus = i + 1;
            return;
        }
        self.panes.truncate(i + 1);
        self.panes.push(Pane::new(path));
        self.select_name(i, &name);
        self.focus = i + 1;
    }

    fn back(&mut self) {
        if self.focus > 0 {
            let j = self.focus - 1;
            self.focus_pane(j);
            return;
        }
        let first = self.panes[0].path.clone();
        match first.parent() {
            Some(parent) if parent != first => {
                self.panes.insert(0, Pane::new(parent.to_path_buf()));
                self.select_name(0, &file_name(&first));
                self.msg = format!("Added {}", pane_title(parent));
            }
            _ => self.msg = "Already at the top of the drive".into(),
        }
    }

    fn reload_all(&mut self) {
        if let Some(bad) = self.panes.iter().skip(1).position(|p| !p.path.is_dir()) {
            self.panes.truncate(bad + 1);
        }
        if self.focus >= self.panes.len() {
            self.focus = self.panes.len() - 1;
        }
        for i in 0..self.panes.len() {
            let keep = self.selected_in(i).map(|e| e.name.clone());
            self.panes[i].reload();
            if i != self.focus {
                if let Some(l) = self.link_name(i) {
                    self.select_name(i, &l);
                    continue;
                }
            }
            if let Some(k) = keep {
                self.select_name(i, &k);
            }
            self.clamp_sel(i);
        }
    }

    // ---- file operations (never delete)

    fn transfer(&mut self, src: &Path, dest_dir: &Path, want_move: bool) {
        let name = file_name(src);
        if src.is_dir() && dest_dir.starts_with(src) {
            self.msg = "Can't put a folder inside itself".into();
            return;
        }
        if want_move && src.parent() == Some(dest_dir) {
            self.msg = format!("{name} is already in that folder");
            return;
        }
        let target = unique_target(dest_dir, &name);
        let shown = file_name(&target);
        let where_ = pane_title(dest_dir);
        let res = if want_move {
            match fs::rename(src, &target) {
                Ok(()) => Ok(format!("Moved {name} to {where_}")),
                Err(e) if crosses_devices(&e) => copy_any(src, &target)
                    .map(|_| format!("Copied {name} to {where_} (other drive, original kept)")),
                Err(e) => Err(e),
            }
        } else {
            copy_any(src, &target).map(|_| format!("Copied {name} to {where_}"))
        };
        match res {
            Ok(m) if shown != name => self.msg = format!("{m} as {shown}"),
            Ok(m) => self.msg = m,
            Err(e) => self.msg = format!("Couldn't do that: {e}"),
        }
        self.reload_all();
    }

    fn paste(&mut self) {
        let Some((src, cut)) = self.clip.clone() else {
            self.msg = "Nothing to paste: press y (copy) or x (cut) on a file first".into();
            return;
        };
        if !src.exists() {
            self.clip = None;
            self.msg = format!("{} isn't there any more", file_name(&src));
            return;
        }
        let dest = self.panes[self.focus].path.clone();
        self.transfer(&src, &dest, cut);
        if cut {
            self.clip = None;
        }
    }

    fn finish_input(&mut self, kind: InputKind, buf: String) {
        let name = buf.trim().to_string();
        if name.is_empty() || name.contains(['/', '\\']) || name == "." || name == ".." {
            self.msg = "That name won't work".into();
            return;
        }
        let f = self.focus;
        match kind {
            InputKind::Rename => {
                let Some((path, _)) = self.selected() else {
                    return;
                };
                let old = file_name(&path);
                let new = path.with_file_name(&name);
                if new.exists() && !old.eq_ignore_ascii_case(&name) {
                    self.msg = format!("{name} already exists");
                    return;
                }
                match fs::rename(&path, &new) {
                    Ok(()) => {
                        if self.link_name(f).as_deref() == Some(old.as_str()) {
                            self.panes.truncate(f + 1);
                        }
                        self.reload_all();
                        self.select_name(f, &name);
                        self.msg = format!("Renamed to {name}");
                    }
                    Err(e) => self.msg = format!("Couldn't rename: {e}"),
                }
            }
            InputKind::NewFolder => {
                let dir = self.panes[f].path.join(&name);
                match fs::create_dir(&dir) {
                    Ok(()) => {
                        self.reload_all();
                        self.select_name(f, &name);
                        self.msg = format!("Made {name}");
                    }
                    Err(e) => self.msg = format!("Couldn't make it: {e}"),
                }
            }
        }
    }

    // ---- input

    fn on_key(&mut self, k: KeyEvent) {
        let mode = std::mem::replace(&mut self.mode, Mode::Normal);
        match mode {
            Mode::Normal => self.key_normal(k),
            Mode::Help => {}
            Mode::Leader => {
                if let KeyCode::Char(c) = k.code {
                    match index_for_label(c) {
                        Some(i) => self.jump(i),
                        None => self.msg = "Jump cancelled".into(),
                    }
                }
            }
            Mode::MarkSet => {
                if let KeyCode::Char(c) = k.code {
                    if c.is_alphanumeric() {
                        let p = self.panes[self.focus].path.clone();
                        self.msg =
                            format!("Marked {} as {c}  (' then {c} comes back)", pane_title(&p));
                        self.marks.insert(c, p);
                    }
                }
            }
            Mode::MarkJump => {
                if let KeyCode::Char(c) = k.code {
                    match self.marks.get(&c).cloned() {
                        Some(p) if p.is_dir() => self.open_path(&p),
                        Some(_) => self.msg = format!("Mark {c}'s folder is gone"),
                        None => self.msg = format!("No mark {c} yet (m then {c} sets one)"),
                    }
                }
            }
            Mode::Filter => self.key_filter(k),
            Mode::Input(kind, mut buf) => match k.code {
                KeyCode::Esc => self.msg = "Cancelled".into(),
                KeyCode::Enter => self.finish_input(kind, buf),
                KeyCode::Backspace => {
                    buf.pop();
                    self.mode = Mode::Input(kind, buf);
                }
                KeyCode::Char(c) if !k.modifiers.contains(KeyModifiers::CONTROL) => {
                    buf.push(c);
                    self.mode = Mode::Input(kind, buf);
                }
                _ => self.mode = Mode::Input(kind, buf),
            },
            Mode::Confirm(d) => match k.code {
                KeyCode::Char('m') | KeyCode::Char('M') => self.transfer(&d.src, &d.dest, true),
                KeyCode::Char('c') | KeyCode::Char('C') => self.transfer(&d.src, &d.dest, false),
                KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('n') => {
                    self.msg = "Nothing changed".into()
                }
                _ => self.mode = Mode::Confirm(d),
            },
        }
    }

    fn key_filter(&mut self, k: KeyEvent) {
        let f = self.focus;
        match k.code {
            KeyCode::Esc => {
                self.panes[f].filter.clear();
                self.clamp_sel(f);
                return;
            }
            KeyCode::Enter => return,
            KeyCode::Up => self.move_sel(-1),
            KeyCode::Down => self.move_sel(1),
            KeyCode::Backspace => {
                self.panes[f].filter.pop();
                self.panes[f].sel = 0;
            }
            KeyCode::Char(c) if !k.modifiers.contains(KeyModifiers::CONTROL) => {
                self.panes[f].filter.push(c);
                self.panes[f].sel = 0;
            }
            _ => {}
        }
        self.mode = Mode::Filter;
    }

    fn key_normal(&mut self, k: KeyEvent) {
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        let alt = k.modifiers.contains(KeyModifiers::ALT);
        self.msg.clear();

        if let KeyCode::Char(c) = k.code {
            if let Some(d) = c.to_digit(10) {
                if ctrl || alt {
                    self.count = None;
                    self.jump(d as usize);
                    return;
                }
                if d != 0 || self.count.is_some() {
                    let n = self
                        .count
                        .unwrap_or(0)
                        .saturating_mul(10)
                        .saturating_add(d as usize);
                    self.count = Some(n.min(9999));
                    return;
                }
            }
        }
        let count = self.count.take().unwrap_or(1) as isize;
        let page = self.geo.page.max(1) as isize;
        let f = self.focus;

        match k.code {
            KeyCode::Char('c') if ctrl => self.quit = true,
            KeyCode::Char('r') if ctrl => {
                self.reload_all();
                self.msg = "Refreshed".into();
            }
            KeyCode::Up | KeyCode::Char('k') => self.move_sel(-count),
            KeyCode::Down | KeyCode::Char('j') => self.move_sel(count),
            KeyCode::PageUp => self.move_sel(-page * count),
            KeyCode::PageDown => self.move_sel(page * count),
            KeyCode::Home if ctrl => self.jump(0),
            KeyCode::End if ctrl => self.jump(self.panes.len() - 1),
            KeyCode::Home => self.panes[f].sel = 0,
            KeyCode::End => {
                let last = self.visible(f).len().saturating_sub(1);
                self.panes[f].sel = last;
            }
            KeyCode::Enter | KeyCode::Right | KeyCode::Char('l') => self.open_selected(),
            KeyCode::Left | KeyCode::Backspace | KeyCode::Char('h') => self.back(),
            KeyCode::Char(' ') => self.mode = Mode::Leader,
            KeyCode::Char('/') => self.mode = Mode::Filter,
            KeyCode::Char('c') => {
                let p = self
                    .selected()
                    .map(|(p, _)| p)
                    .unwrap_or_else(|| self.panes[f].path.clone());
                copy_to_clipboard(&p.display().to_string());
                self.msg = format!("Copied path: {}", p.display());
            }
            KeyCode::Char('o') => {
                match self.selected() {
                    Some((p, _)) => open_external(&p, true),
                    None => open_external(&self.panes[f].path, false),
                }
                self.msg = "Showing it in your file manager".into();
            }
            KeyCode::Char('y') | KeyCode::Char('x') => {
                if let Some((p, _)) = self.selected() {
                    let cut = k.code == KeyCode::Char('x');
                    self.msg = format!(
                        "{} {}: go to a folder and press p",
                        if cut { "Cut" } else { "Copied" },
                        file_name(&p)
                    );
                    self.clip = Some((p, cut));
                }
            }
            KeyCode::Char('p') => self.paste(),
            KeyCode::Char('r') | KeyCode::F(2) => {
                if let Some(name) = self.selected_in(f).map(|e| e.name.clone()) {
                    self.mode = Mode::Input(InputKind::Rename, name);
                }
            }
            KeyCode::Char('n') => self.mode = Mode::Input(InputKind::NewFolder, String::new()),
            KeyCode::Char('.') => {
                let keep = self.selected_in(f).map(|e| e.name.clone());
                self.show_hidden = !self.show_hidden;
                if let Some(k) = keep {
                    self.select_name(f, &k);
                }
                self.clamp_sel(f);
                self.msg = if self.show_hidden {
                    "Showing hidden files"
                } else {
                    "Hiding hidden files"
                }
                .into();
            }
            KeyCode::F(5) => {
                self.reload_all();
                self.msg = "Refreshed".into();
            }
            KeyCode::Char('m') => self.mode = Mode::MarkSet,
            KeyCode::Char('\'') => self.mode = Mode::MarkJump,
            KeyCode::Char('?') => self.mode = Mode::Help,
            KeyCode::Char('q') => {
                self.quit = true;
                self.print_on_quit = true;
            }
            KeyCode::Esc => {
                if !self.panes[f].filter.is_empty() {
                    self.panes[f].filter.clear();
                    self.clamp_sel(f);
                }
            }
            _ => {}
        }
    }

    /// Which pane (and visible row) is at screen position x, y.
    fn hit(&self, x: u16, y: u16) -> Option<(usize, Option<usize>)> {
        let g = self.geo.panes.iter().find(|g| contains(g.rect, x, y))?;
        let row = if y >= g.list_y && y < g.list_y + g.list_h {
            let v = g.top + (y - g.list_y) as usize;
            (v < g.vis.len()).then_some(v)
        } else {
            None
        };
        Some((g.idx, row))
    }

    fn on_mouse(&mut self, m: MouseEvent) {
        let (x, y) = (m.column, m.row);
        match m.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                match self.mode {
                    Mode::Normal | Mode::Leader | Mode::Help => self.mode = Mode::Normal,
                    _ => return,
                }
                if let Some(&(_, i)) = self.geo.strip.iter().find(|(r, _)| contains(*r, x, y)) {
                    self.jump(i);
                    return;
                }
                let Some((pi, row)) = self.hit(x, y) else {
                    return;
                };
                self.focus_pane(pi);
                let Some(v) = row else { return };
                self.panes[pi].sel = v;
                let now = Instant::now();
                let double = matches!(self.last_click,
                    Some((t, p, r)) if p == pi && r == v && now.duration_since(t) < DOUBLE_CLICK);
                if double {
                    self.last_click = None;
                    self.open_selected();
                } else {
                    self.last_click = Some((now, pi, v));
                    self.drag = Some((pi, v));
                    self.dragging = false;
                }
            }
            MouseEventKind::Drag(MouseButton::Left) => {
                if self.drag.is_some() {
                    self.dragging = true;
                    self.msg = "Drop it on a folder or a pane".into();
                }
            }
            MouseEventKind::Up(MouseButton::Left) => {
                let drag = self.drag.take();
                if self.dragging {
                    self.dragging = false;
                    if let Some((sp, sv)) = drag {
                        self.drop_at(sp, sv, x, y);
                    }
                }
            }
            MouseEventKind::ScrollDown | MouseEventKind::ScrollUp => {
                if let Some((pi, _)) = self.hit(x, y) {
                    self.focus_pane(pi);
                    self.move_sel(if m.kind == MouseEventKind::ScrollDown {
                        3
                    } else {
                        -3
                    });
                }
            }
            _ => {}
        }
    }

    fn drop_at(&mut self, sp: usize, sv: usize, x: u16, y: u16) {
        let Some(&k) = self.visible(sp).get(sv) else {
            return;
        };
        let src = self.panes[sp].path.join(&self.panes[sp].entries[k].name);
        let Some((pi, row)) = self.hit(x, y) else {
            self.msg = "Dropped outside the panes: nothing changed".into();
            return;
        };
        let vis = self.visible(pi);
        let dest = match row
            .and_then(|v| vis.get(v))
            .map(|&k| &self.panes[pi].entries[k])
        {
            Some(e) if e.is_dir => self.panes[pi].path.join(&e.name),
            _ => self.panes[pi].path.clone(),
        };
        if dest == src || src.parent() == Some(dest.as_path()) {
            self.msg = "Same folder: nothing changed".into();
            return;
        }
        self.mode = Mode::Confirm(PendingDrop { src, dest });
    }
}

// ---------------------------------------------------------------- drawing

fn draw(f: &mut Frame, app: &mut App) {
    let area = f.area();
    app.geo = Geo::default();
    if area.height < 6 || area.width < 24 {
        f.render_widget(Paragraph::new("Make the window bigger"), area);
        return;
    }
    draw_strip(f, app, Rect { height: 1, ..area });
    let canvas = Rect {
        y: area.y + 1,
        height: area.height - 2,
        ..area
    };
    draw_canvas(f, app, canvas);
    draw_status(
        f,
        app,
        Rect {
            y: area.y + area.height - 1,
            height: 1,
            ..area
        },
    );
    if matches!(app.mode, Mode::Help) {
        draw_help(f, area);
    }
}

fn draw_strip(f: &mut Frame, app: &mut App, area: Rect) {
    let leader = matches!(app.mode, Mode::Leader);
    let dim = Style::new().fg(Color::DarkGray);
    let items: Vec<(String, String)> = app
        .panes
        .iter()
        .enumerate()
        .map(|(i, p)| {
            let l = label(i).map(|c| format!("[{c}]")).unwrap_or_default();
            (l, truncate(&pane_title(&p.path), 24))
        })
        .collect();
    let widths: Vec<usize> = items.iter().map(|(l, n)| width(l) + 1 + width(n)).collect();
    let total = |s: usize| -> usize {
        widths[s..].iter().sum::<usize>() + 3 * (widths.len() - s - 1) + if s > 0 { 2 } else { 0 }
    };
    let mut start = 0;
    while start + 1 < items.len() && start < app.focus && total(start) > area.width as usize {
        start += 1;
    }

    let mut spans = Vec::new();
    let mut x = area.x;
    if start > 0 {
        spans.push(Span::styled("… ", dim));
        x += 2;
    }
    for (i, (l, n)) in items.iter().enumerate().skip(start) {
        if i > start {
            spans.push(Span::styled(" › ", dim));
            x = x.saturating_add(3);
        }
        let fwd = i > app.focus;
        let lstyle = if leader {
            Style::new()
                .fg(Color::Black)
                .bg(Color::Yellow)
                .add_modifier(Modifier::BOLD)
        } else if fwd {
            dim
        } else {
            Style::new().fg(Color::Yellow)
        };
        let nstyle = if i == app.focus {
            Style::new()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD | Modifier::UNDERLINED)
        } else if fwd {
            dim
        } else {
            Style::new()
        };
        spans.push(Span::styled(l.clone(), lstyle));
        spans.push(Span::raw(" "));
        spans.push(Span::styled(n.clone(), nstyle));
        let w = widths[i] as u16;
        if x < area.x + area.width {
            let w = w.min(area.x + area.width - x);
            app.geo.strip.push((
                Rect {
                    x,
                    y: area.y,
                    width: w,
                    height: 1,
                },
                i,
            ));
        }
        x = x.saturating_add(w);
    }
    f.render_widget(Paragraph::new(Line::from(spans)), area);
}

fn draw_canvas(f: &mut Frame, app: &mut App, area: Rect) {
    let pw = PANE_W.min(area.width);
    let fit = (((area.width + GAP) / (pw + GAP)).max(1)) as usize;
    let len = app.panes.len();
    if app.focus < app.scroll {
        app.scroll = app.focus;
    }
    if app.focus >= app.scroll + fit {
        app.scroll = app.focus + 1 - fit;
    }
    app.scroll = app.scroll.min(len.saturating_sub(fit));

    let mut prev: Option<(Rect, Option<u16>, usize)> = None;
    for i in app.scroll..len.min(app.scroll + fit) {
        let rect = Rect {
            x: area.x + (i - app.scroll) as u16 * (pw + GAP),
            y: area.y,
            width: pw,
            height: area.height,
        };
        let link_y = draw_pane(f, app, i, rect);
        if let Some((prect, Some(ly), pi)) = prev {
            let st = if pi + 1 > app.focus {
                Style::new().fg(Color::DarkGray)
            } else {
                Style::new().fg(Color::Yellow)
            };
            let gx = prect.x + prect.width;
            let ty = rect.y;
            let buf = f.buffer_mut();
            buf.set_string(gx, ly, "─", st);
            buf.set_string(gx + 1, ly, "┘", st);
            for yy in ty + 1..ly {
                buf.set_string(gx + 1, yy, "│", st);
            }
            buf.set_string(gx + 1, ty, "┌", st);
            buf.set_string(gx + 2, ty, "─", st);
        }
        prev = Some((rect, link_y, i));
    }
}

/// Draws pane `i`; returns the screen row of its linking folder, if visible.
fn draw_pane(f: &mut Frame, app: &mut App, i: usize, rect: Rect) -> Option<u16> {
    let focused = i == app.focus;
    let fwd = i > app.focus;
    let vis = app.visible(i);
    let link = app.link_name(i);
    let inner_h = rect.height.saturating_sub(2) as usize;
    if focused {
        app.geo.page = inner_h;
    }

    let target = if focused {
        Some(app.panes[i].sel)
    } else {
        link.as_ref()
            .and_then(|l| vis.iter().position(|&k| &app.panes[i].entries[k].name == l))
    };
    {
        let p = &mut app.panes[i];
        if let Some(t) = target {
            if t < p.top {
                p.top = t;
            } else if inner_h > 0 && t >= p.top + inner_h {
                p.top = t + 1 - inner_h;
            }
        }
        p.top = p.top.min(vis.len().saturating_sub(inner_h));
    }

    let p = &app.panes[i];
    let border = if focused {
        Style::new().fg(Color::Cyan)
    } else if fwd {
        Style::new().fg(Color::DarkGray)
    } else {
        Style::new().fg(Color::Gray)
    };
    let mut title = pane_title(&p.path);
    if !p.filter.is_empty() {
        title = format!("{title}  /{}", p.filter);
    }
    let title = truncate(
        &format!(" {title} "),
        (rect.width as usize).saturating_sub(2),
    );
    let tstyle = if focused {
        border.add_modifier(Modifier::BOLD)
    } else {
        border
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(border)
        .title(Span::styled(title, tstyle));
    let inner = block.inner(rect);
    f.render_widget(block, rect);

    let iw = inner.width as usize;
    let mut lines: Vec<Line> = Vec::new();
    let mut link_y = None;
    if let Some(err) = &p.error {
        lines.push(Line::styled(
            truncate(&format!("Can't open: {err}"), iw),
            Style::new().fg(Color::Red),
        ));
    } else if vis.is_empty() {
        let t = if p.filter.is_empty() {
            "(empty)"
        } else {
            "(no matches)"
        };
        lines.push(Line::styled(t, Style::new().fg(Color::DarkGray)));
    }
    for (r, &k) in vis.iter().enumerate().skip(p.top).take(inner_h) {
        let e = &p.entries[k];
        let is_link = link.as_deref() == Some(e.name.as_str());
        if is_link {
            link_y = Some(inner.y + (r - p.top) as u16);
        }
        let icon = if is_link {
            "▶ "
        } else if e.is_dir {
            "▸ "
        } else {
            "  "
        };
        let size = if e.is_dir {
            String::new()
        } else {
            e.size.map(human).unwrap_or_default()
        };
        let room = iw.saturating_sub(2 + if size.is_empty() { 0 } else { size.len() + 1 });
        let name = truncate(&e.name, room);
        let pad = iw.saturating_sub(2 + width(&name) + size.len());
        let text = format!("{icon}{name}{}{size}", " ".repeat(pad));

        let mut st = if e.is_dir {
            Style::new().fg(Color::Blue).add_modifier(Modifier::BOLD)
        } else {
            Style::new()
        };
        if e.hidden {
            st = st.add_modifier(Modifier::DIM);
        }
        if is_link {
            st = Style::new().fg(Color::Yellow).add_modifier(Modifier::BOLD);
        }
        if fwd {
            st = Style::new().fg(Color::DarkGray);
        }
        if focused && r == p.sel {
            st = st.add_modifier(Modifier::REVERSED);
        }
        lines.push(Line::styled(text, st));
    }
    f.render_widget(Paragraph::new(lines), inner);

    let top = p.top;
    app.geo.panes.push(PaneGeo {
        idx: i,
        rect,
        list_y: inner.y,
        list_h: inner.height,
        top,
        vis,
    });
    link_y
}

fn draw_status(f: &mut Frame, app: &App, area: Rect) {
    let hint = " ? help ";
    let text = match &app.mode {
        Mode::Leader => "Jump: press a number from the strip (Esc cancels)".to_string(),
        Mode::Filter => format!(
            "/{}▏  Enter keeps it · Esc clears",
            app.panes[app.focus].filter
        ),
        Mode::Input(InputKind::Rename, b) => format!("Rename to: {b}▏  Enter · Esc cancels"),
        Mode::Input(InputKind::NewFolder, b) => format!("New folder: {b}▏  Enter · Esc cancels"),
        Mode::Confirm(d) => format!(
            "Put {} into {}?   m = move   c = copy   Esc = cancel",
            file_name(&d.src),
            pane_title(&d.dest)
        ),
        Mode::MarkSet => "Mark this folder: press a letter".to_string(),
        Mode::MarkJump => {
            let mut ks: Vec<_> = app.marks.keys().collect();
            ks.sort();
            let list: String = ks.into_iter().collect();
            if list.is_empty() {
                "No marks yet (m + letter sets one) · Esc".to_string()
            } else {
                format!("Go to mark: {list}")
            }
        }
        Mode::Help => "Press any key to close help".to_string(),
        Mode::Normal => match app.count {
            Some(n) => format!("{n}  (now ↑ or ↓ to move {n})"),
            None => app.msg.clone(),
        },
    };
    let left_w = area.width.saturating_sub(hint.len() as u16);
    let st = match app.mode {
        Mode::Normal => Style::new().fg(Color::Gray),
        _ => Style::new().fg(Color::Yellow).add_modifier(Modifier::BOLD),
    };
    f.render_widget(
        Paragraph::new(Span::styled(
            truncate(&format!(" {text}"), left_w as usize),
            st,
        )),
        Rect {
            width: left_w,
            ..area
        },
    );
    f.render_widget(
        Paragraph::new(Span::styled(hint, Style::new().fg(Color::DarkGray))),
        Rect {
            x: area.x + left_w,
            width: area.width - left_w,
            ..area
        },
    );
}

const HELP: &[&str] = &[
    "↑ ↓  j k         move (type a number first: 3 ↓ moves 3)",
    "→ Enter l        open folder beside this one, or open file",
    "← Backspace h    back one folder (at the top: add the parent)",
    "Space + number   jump to that folder in the path strip",
    "Ctrl+Home / End  start / end of the path",
    "Ctrl or Alt + #  jump too, in terminals that send those keys",
    "/                filter this folder · Esc clears",
    "c                copy path to the clipboard",
    "o                show in Explorer / Finder",
    "y  x  p          copy, cut, paste into this folder",
    "r or F2          rename      n   new folder",
    "m + letter       mark folder · ' + letter goes to it",
    ".                hidden files on/off · F5 refresh",
    "Mouse            click, double-click to open, wheel scrolls,",
    "                 drag a file onto a folder or pane to move/copy",
    "q                quit                Canopy never deletes files",
];

fn draw_help(f: &mut Frame, area: Rect) {
    let w = 66.min(area.width.saturating_sub(2));
    let h = (HELP.len() as u16 + 2).min(area.height.saturating_sub(2));
    let r = Rect {
        x: area.x + (area.width - w) / 2,
        y: area.y + (area.height - h) / 2,
        width: w,
        height: h,
    };
    f.render_widget(Clear, r);
    let lines: Vec<Line> = HELP.iter().map(|l| Line::raw(*l)).collect();
    f.render_widget(
        Paragraph::new(lines).block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::new().fg(Color::Cyan))
                .title(" Canopy keys "),
        ),
        r,
    );
}

// ---------------------------------------------------------------- helpers

fn read_entries(path: &Path) -> Result<Vec<Entry>, String> {
    let rd = fs::read_dir(path).map_err(|e| e.to_string())?;
    let mut v = Vec::new();
    for de in rd.flatten() {
        let name = de.file_name().to_string_lossy().into_owned();
        let md = fs::metadata(de.path()).or_else(|_| de.metadata());
        let (is_dir, size, attr_hidden) = match md {
            Ok(m) => (m.is_dir(), (!m.is_dir()).then(|| m.len()), hidden_attr(&m)),
            Err(_) => (false, None, false),
        };
        v.push(Entry {
            hidden: attr_hidden || name.starts_with('.'),
            name,
            is_dir,
            size,
        });
    }
    v.sort_by(|a, b| {
        b.is_dir
            .cmp(&a.is_dir)
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });
    Ok(v)
}

#[cfg(windows)]
fn hidden_attr(m: &fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;
    m.file_attributes() & 0x2 != 0
}

#[cfg(not(windows))]
fn hidden_attr(_m: &fs::Metadata) -> bool {
    false
}

/// Every folder from the drive root down to `path`.
fn chain_for(path: &Path) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = path
        .ancestors()
        .filter(|p| !p.as_os_str().is_empty())
        .map(Path::to_path_buf)
        .collect();
    v.reverse();
    if v.is_empty() {
        v.push(path.to_path_buf());
    }
    v
}

fn file_name(p: &Path) -> String {
    p.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| p.display().to_string())
}

fn pane_title(p: &Path) -> String {
    file_name(p)
}

/// Labels for the path strip: 0–9, then a–z.
fn label(i: usize) -> Option<char> {
    match i {
        0..=9 => char::from_digit(i as u32, 10),
        10..=35 => Some((b'a' + (i - 10) as u8) as char),
        _ => None,
    }
}

fn index_for_label(c: char) -> Option<usize> {
    match c {
        '0'..='9' => Some(c as usize - '0' as usize),
        'a'..='z' => Some(c as usize - 'a' as usize + 10),
        _ => None,
    }
}

fn contains(r: Rect, x: u16, y: u16) -> bool {
    x >= r.x && x < r.x + r.width && y >= r.y && y < r.y + r.height
}

fn width(s: &str) -> usize {
    s.chars().map(|c| c.width().unwrap_or(0)).sum()
}

/// Cut `s` to at most `w` columns, ending in … if it was cut.
fn truncate(s: &str, w: usize) -> String {
    if width(s) <= w {
        return s.to_string();
    }
    if w == 0 {
        return String::new();
    }
    let mut out = String::new();
    let mut used = 0;
    for c in s.chars() {
        let cw = c.width().unwrap_or(0);
        if used + cw > w - 1 {
            break;
        }
        out.push(c);
        used += cw;
    }
    out.push('…');
    out
}

fn human(n: u64) -> String {
    const UNITS: [&str; 5] = ["B", "K", "M", "G", "T"];
    let mut f = n as f64;
    let mut i = 0;
    while f >= 1024.0 && i < 4 {
        f /= 1024.0;
        i += 1;
    }
    if i == 0 {
        format!("{n}B")
    } else if f < 10.0 {
        format!("{f:.1}{}", UNITS[i])
    } else {
        format!("{f:.0}{}", UNITS[i])
    }
}

/// `dir/name`, or `dir/name (2).ext` etc. if that's taken. Never overwrites.
fn unique_target(dir: &Path, name: &str) -> PathBuf {
    let first = dir.join(name);
    if !first.exists() {
        return first;
    }
    let p = Path::new(name);
    let (stem, ext) = match (p.file_stem(), p.extension()) {
        (Some(s), Some(e)) if !name.starts_with('.') || name[1..].contains('.') => (
            s.to_string_lossy().into_owned(),
            format!(".{}", e.to_string_lossy()),
        ),
        _ => (name.to_string(), String::new()),
    };
    (2..)
        .map(|n| dir.join(format!("{stem} ({n}){ext}")))
        .find(|t| !t.exists())
        .expect("ran out of numbers")
}

fn copy_any(src: &Path, dst: &Path) -> io::Result<()> {
    let md = fs::metadata(src)?;
    if md.is_dir() {
        fs::create_dir(dst)?;
        for e in fs::read_dir(src)? {
            let e = e?;
            copy_any(&e.path(), &dst.join(e.file_name()))?;
        }
        Ok(())
    } else {
        fs::copy(src, dst).map(|_| ())
    }
}

fn crosses_devices(e: &io::Error) -> bool {
    if e.kind() == io::ErrorKind::CrossesDevices {
        return true;
    }
    // ERROR_NOT_SAME_DEVICE on Windows, EXDEV elsewhere.
    let code = if cfg!(windows) { 17 } else { 18 };
    e.raw_os_error() == Some(code)
}

fn b64(data: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut s = String::new();
    for ch in data.chunks(3) {
        let b = [ch[0], *ch.get(1).unwrap_or(&0), *ch.get(2).unwrap_or(&0)];
        let n = ((b[0] as u32) << 16) | ((b[1] as u32) << 8) | (b[2] as u32);
        s.push(T[((n >> 18) & 63) as usize] as char);
        s.push(T[((n >> 12) & 63) as usize] as char);
        s.push(if ch.len() > 1 {
            T[((n >> 6) & 63) as usize] as char
        } else {
            '='
        });
        s.push(if ch.len() > 2 {
            T[(n & 63) as usize] as char
        } else {
            '='
        });
    }
    s
}

fn copy_to_clipboard(text: &str) {
    // OSC 52: understood by Windows Terminal, iTerm, kitty, WezTerm, tmux, SSH…
    let mut e = io::stderr();
    let _ = write!(e, "\x1b]52;c;{}\x07", b64(text.as_bytes()));
    let _ = e.flush();

    #[cfg(windows)]
    let helper: Option<(&str, Vec<u8>)> = {
        let mut bytes = vec![0xFF, 0xFE];
        bytes.extend(text.encode_utf16().flat_map(u16::to_le_bytes));
        Some(("clip", bytes))
    };
    #[cfg(target_os = "macos")]
    let helper: Option<(&str, Vec<u8>)> = Some(("pbcopy", text.as_bytes().to_vec()));
    #[cfg(not(any(windows, target_os = "macos")))]
    let helper: Option<(&str, Vec<u8>)> = None;

    if let Some((cmd, bytes)) = helper {
        if let Ok(mut c) = Command::new(cmd)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
        {
            if let Some(mut si) = c.stdin.take() {
                let _ = si.write_all(&bytes);
            }
            let _ = c.wait();
        }
    }
}

/// Open a file with its app, or show it (`reveal`) in Explorer / Finder.
fn open_external(path: &Path, reveal: bool) {
    let mut cmd;
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd = Command::new("explorer");
        if reveal {
            cmd.raw_arg(format!("/select,\"{}\"", path.display()));
        } else {
            cmd.arg(path);
        }
    }
    #[cfg(target_os = "macos")]
    {
        cmd = Command::new("open");
        if reveal {
            cmd.arg("-R");
        }
        cmd.arg(path);
    }
    #[cfg(not(any(windows, target_os = "macos")))]
    {
        cmd = Command::new("xdg-open");
        let target = if reveal {
            path.parent().unwrap_or(path)
        } else {
            path
        };
        cmd.arg(target);
    }
    let _ = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();
}

#[cfg(windows)]
fn resolve(p: PathBuf) -> PathBuf {
    std::path::absolute(&p).unwrap_or(p)
}

#[cfg(not(windows))]
fn resolve(p: PathBuf) -> PathBuf {
    fs::canonicalize(&p).unwrap_or(p)
}

// ---------------------------------------------------------------- terminal

static KEYBOARD_ENHANCED: AtomicBool = AtomicBool::new(false);

fn restore_terminal() {
    let mut e = io::stderr();
    if KEYBOARD_ENHANCED.swap(false, Ordering::SeqCst) {
        let _ = execute!(e, PopKeyboardEnhancementFlags);
    }
    let _ = execute!(e, DisableMouseCapture, LeaveAlternateScreen, cursor::Show);
    let _ = disable_raw_mode();
}

fn run_tui(app: &mut App) -> io::Result<()> {
    // The interface draws on stderr so `--print-dir` can use stdout.
    let mut err = io::stderr();
    enable_raw_mode()?;
    execute!(err, EnterAlternateScreen, EnableMouseCapture)?;
    if matches!(supports_keyboard_enhancement(), Ok(true))
        && execute!(
            err,
            PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES)
        )
        .is_ok()
    {
        KEYBOARD_ENHANCED.store(true, Ordering::SeqCst);
    }
    let prev = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        restore_terminal();
        prev(info);
    }));

    let result = (|| -> io::Result<()> {
        let mut term = Terminal::new(CrosstermBackend::new(io::stderr()))?;
        term.clear()?;
        while !app.quit {
            term.draw(|f| draw(f, app))?;
            match event::read()? {
                Event::Key(k) if k.kind != KeyEventKind::Release => app.on_key(k),
                Event::Mouse(m) => app.on_mouse(m),
                _ => {}
            }
        }
        Ok(())
    })();
    restore_terminal();
    result
}

fn main() {
    let mut print_dir = false;
    let mut start: Option<PathBuf> = None;
    for a in std::env::args_os().skip(1) {
        match a.to_str() {
            Some("--print-dir") => print_dir = true,
            Some("-h") | Some("--help") => {
                println!("{USAGE}");
                return;
            }
            Some("-V") | Some("--version") => {
                println!("canopy-cli {}", env!("CARGO_PKG_VERSION"));
                return;
            }
            Some(s) if s.starts_with("--") => {
                eprintln!("canopy-cli: unknown option {s}\n\n{USAGE}");
                std::process::exit(2);
            }
            _ => start = Some(PathBuf::from(&a)),
        }
    }
    let start = resolve(
        start.unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))),
    );
    if !start.is_dir() {
        eprintln!("canopy-cli: not a folder: {}", start.display());
        std::process::exit(1);
    }
    if !io::stderr().is_terminal() || !io::stdin().is_terminal() {
        eprintln!("canopy-cli needs to run in a terminal window");
        std::process::exit(1);
    }

    let mut app = App::new(&start);
    if let Err(e) = run_tui(&mut app) {
        eprintln!("canopy-cli: {e}");
        std::process::exit(1);
    }
    if print_dir && app.print_on_quit {
        println!("{}", app.panes[app.focus].path.display());
    }
}

// ---------------------------------------------------------------- tests

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::backend::TestBackend;
    use std::sync::atomic::AtomicUsize;

    fn tmp() -> PathBuf {
        static N: AtomicUsize = AtomicUsize::new(0);
        let d = std::env::temp_dir().join(format!(
            "canopy-cli-test-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::SeqCst)
        ));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        resolve(d)
    }

    /// root/{alpha/{beta/, inner.txt}, notes.txt, .secret}
    fn sample() -> PathBuf {
        let r = tmp();
        fs::create_dir_all(r.join("alpha").join("beta")).unwrap();
        fs::write(r.join("alpha").join("inner.txt"), "x").unwrap();
        fs::write(r.join("notes.txt"), "hello").unwrap();
        fs::write(r.join(".secret"), "s").unwrap();
        r
    }

    fn screen(app: &mut App) -> String {
        let mut t = Terminal::new(TestBackend::new(130, 24)).unwrap();
        t.draw(|f| draw(f, app)).unwrap();
        let b = t.backend().buffer().clone();
        b.content
            .chunks(b.area.width as usize)
            .map(|row| row.iter().map(|c| c.symbol()).collect::<String>())
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn press(app: &mut App, code: KeyCode) {
        app.on_key(KeyEvent::new(code, KeyModifiers::NONE));
    }

    fn chord(app: &mut App, code: KeyCode, m: KeyModifiers) {
        app.on_key(KeyEvent::new(code, m));
    }

    fn typ(app: &mut App, s: &str) {
        for c in s.chars() {
            press(app, KeyCode::Char(c));
        }
    }

    fn focused(app: &App) -> &Path {
        &app.panes[app.focus].path
    }

    fn select(app: &mut App, name: &str) {
        let f = app.focus;
        app.select_name(f, name);
        assert_eq!(app.selected_in(f).map(|e| e.name.as_str()), Some(name));
    }

    #[test]
    fn starts_with_whole_chain_and_draws_it() {
        let r = sample();
        let mut app = App::new(&r.join("alpha"));
        assert_eq!(focused(&app), r.join("alpha"));
        assert_eq!(app.panes.len(), chain_for(&r).len() + 1);
        let s = screen(&mut app);
        assert!(s.contains("alpha"), "{s}");
        assert!(s.contains("beta"), "{s}");
        assert!(
            s.contains(&format!("[{}]", label(app.focus).unwrap())),
            "{s}"
        );
        assert!(s.contains("inner.txt"), "{s}");
    }

    #[test]
    fn open_back_and_leader_jump_keep_forward_panes() {
        let r = sample();
        let mut app = App::new(&r);
        let base = app.focus;
        select(&mut app, "alpha");
        press(&mut app, KeyCode::Enter);
        assert_eq!(focused(&app), r.join("alpha"));
        select(&mut app, "beta");
        press(&mut app, KeyCode::Enter);
        assert_eq!(focused(&app), r.join("alpha").join("beta"));
        let len = app.panes.len();

        // Space then the label of the root pane.
        press(&mut app, KeyCode::Char(' '));
        press(&mut app, KeyCode::Char(label(base).unwrap()));
        assert_eq!(app.focus, base);
        assert_eq!(app.panes.len(), len, "deeper folders stay (dimmed)");
        assert_eq!(app.selected_in(base).unwrap().name, "alpha");

        // Enter on the same folder walks back down without losing beta.
        press(&mut app, KeyCode::Enter);
        assert_eq!(app.panes.len(), len);

        // A bare digit is a count, not a jump.
        press(&mut app, KeyCode::Char('0'));
        assert_eq!(app.focus, base + 1);

        chord(&mut app, KeyCode::End, KeyModifiers::CONTROL);
        assert_eq!(app.focus, len - 1);
        chord(&mut app, KeyCode::Home, KeyModifiers::CONTROL);
        assert_eq!(app.focus, 0);

        press(&mut app, KeyCode::Char(' '));
        press(&mut app, KeyCode::Char(label(base + 1).unwrap()));
        assert_eq!(app.focus, base + 1);
        press(&mut app, KeyCode::Backspace);
        assert_eq!(app.focus, base);
        chord(
            &mut app,
            KeyCode::Char(char::from_digit((base + 1) as u32 % 10, 10).unwrap()),
            KeyModifiers::ALT,
        );
        if base + 1 < 10 {
            assert_eq!(app.focus, base + 1);
        }
    }

    #[test]
    fn opening_a_different_folder_replaces_forward_panes() {
        let r = sample();
        fs::create_dir(r.join("gamma")).unwrap();
        let mut app = App::new(&r.join("alpha").join("beta"));
        let root = app.focus - 2;
        app.jump(root);
        select(&mut app, "gamma");
        press(&mut app, KeyCode::Enter);
        assert_eq!(focused(&app), r.join("gamma"));
        assert_eq!(app.panes.len(), root + 2);
    }

    #[test]
    fn counts_move_that_many() {
        let r = tmp();
        for i in 0..8 {
            fs::write(r.join(format!("f{i}.txt")), "").unwrap();
        }
        let mut app = App::new(&r);
        typ(&mut app, "3");
        press(&mut app, KeyCode::Down);
        assert_eq!(app.panes[app.focus].sel, 3);
        typ(&mut app, "12");
        press(&mut app, KeyCode::Down);
        assert_eq!(app.panes[app.focus].sel, 7, "clamped to the last file");
        press(&mut app, KeyCode::Char('k'));
        assert_eq!(app.panes[app.focus].sel, 6);
    }

    #[test]
    fn back_at_the_top_adds_the_parent() {
        let r = sample();
        let mut app = App::new(&r.join("alpha"));
        // Cut the chain so alpha is the first pane.
        let k = app.panes.len() - 1;
        app.panes.drain(..k);
        app.focus = 0;
        press(&mut app, KeyCode::Backspace);
        assert_eq!(app.panes[0].path, r);
        assert_eq!(app.selected_in(0).unwrap().name, "alpha");
    }

    #[test]
    fn copy_and_move_never_overwrite_or_delete() {
        let r = sample();
        let mut app = App::new(&r);
        select(&mut app, "notes.txt");
        press(&mut app, KeyCode::Char('y'));
        select(&mut app, "alpha");
        press(&mut app, KeyCode::Enter);
        press(&mut app, KeyCode::Char('p'));
        assert!(r.join("alpha/notes.txt").exists());
        assert!(r.join("notes.txt").exists());

        // Pasting the copy again makes "notes (2).txt".
        app.jump(app.focus - 1);
        select(&mut app, "notes.txt");
        press(&mut app, KeyCode::Char('y'));
        select(&mut app, "alpha");
        press(&mut app, KeyCode::Enter); // alpha is still open: walk back down
        press(&mut app, KeyCode::Char('p'));
        assert!(r.join("alpha/notes (2).txt").exists());

        // Cut + paste moves.
        select(&mut app, "inner.txt");
        press(&mut app, KeyCode::Char('x'));
        select(&mut app, "beta");
        press(&mut app, KeyCode::Enter);
        press(&mut app, KeyCode::Char('p'));
        assert!(r.join("alpha/beta/inner.txt").exists());
        assert!(!r.join("alpha/inner.txt").exists());
    }

    #[test]
    fn moving_a_folder_into_itself_is_refused() {
        let r = sample();
        let mut app = App::new(&r.join("alpha").join("beta"));
        app.transfer(&r.join("alpha"), &r.join("alpha").join("beta"), true);
        assert!(app.msg.contains("inside itself"));
        assert!(r.join("alpha/beta").is_dir());
    }

    #[test]
    fn filter_hidden_rename_newfolder() {
        let r = sample();
        let mut app = App::new(&r);
        let f = app.focus;
        assert!(!app
            .visible(f)
            .iter()
            .any(|&k| app.panes[f].entries[k].name == ".secret"));
        press(&mut app, KeyCode::Char('.'));
        assert!(app
            .visible(f)
            .iter()
            .any(|&k| app.panes[f].entries[k].name == ".secret"));

        press(&mut app, KeyCode::Char('/'));
        typ(&mut app, "NOT");
        assert_eq!(app.visible(f).len(), 1);
        press(&mut app, KeyCode::Esc);
        assert!(app.visible(f).len() > 1);

        select(&mut app, "notes.txt");
        press(&mut app, KeyCode::F(2));
        for _ in 0.."notes.txt".len() {
            press(&mut app, KeyCode::Backspace);
        }
        typ(&mut app, "todo.txt");
        press(&mut app, KeyCode::Enter);
        assert!(r.join("todo.txt").exists());
        assert_eq!(app.selected_in(f).unwrap().name, "todo.txt");

        press(&mut app, KeyCode::Char('n'));
        typ(&mut app, "new stuff");
        press(&mut app, KeyCode::Enter);
        assert!(r.join("new stuff").is_dir());
    }

    #[test]
    fn marks() {
        let r = sample();
        let mut app = App::new(&r.join("alpha").join("beta"));
        typ(&mut app, "mb");
        app.open_path(&r);
        typ(&mut app, "'b");
        assert_eq!(focused(&app), r.join("alpha").join("beta"));
    }

    #[test]
    fn mouse_click_strip_and_drag_drop() {
        let r = sample();
        let mut app = App::new(&r.join("alpha"));
        screen(&mut app);
        let (rect, idx) = app.geo.strip[0];
        let click = |kind, x, y| MouseEvent {
            kind,
            column: x,
            row: y,
            modifiers: KeyModifiers::NONE,
        };
        app.on_mouse(click(
            MouseEventKind::Down(MouseButton::Left),
            rect.x,
            rect.y,
        ));
        assert_eq!(app.focus, idx);

        // Drag notes.txt (in root) onto the alpha pane: asks, then copies.
        let mut app = App::new(&r.join("alpha"));
        screen(&mut app);
        let root = app.focus - 1;
        let rg = app.geo.panes.iter().find(|g| g.idx == root).unwrap();
        let row = rg
            .vis
            .iter()
            .position(|&k| app.panes[root].entries[k].name == "notes.txt")
            .unwrap();
        let (sx, sy) = (rg.rect.x + 3, rg.list_y + (row - rg.top) as u16);
        let ag = app.geo.panes.iter().find(|g| g.idx == app.focus).unwrap();
        let (dx, dy) = (ag.rect.x + 3, ag.rect.y + ag.rect.height - 2);
        app.on_mouse(click(MouseEventKind::Down(MouseButton::Left), sx, sy));
        app.on_mouse(click(MouseEventKind::Drag(MouseButton::Left), dx, dy));
        app.on_mouse(click(MouseEventKind::Up(MouseButton::Left), dx, dy));
        assert!(matches!(app.mode, Mode::Confirm(_)));
        assert!(screen(&mut app).contains("m = move"));
        press(&mut app, KeyCode::Char('c'));
        assert!(r.join("alpha/notes.txt").exists());
        assert!(r.join("notes.txt").exists());
    }

    #[test]
    fn helpers() {
        assert_eq!(b64(b"hello"), "aGVsbG8=");
        assert_eq!(b64(b"hi"), "aGk=");
        assert_eq!(human(512), "512B");
        assert_eq!(human(1536), "1.5K");
        assert_eq!(truncate("abcdef", 4), "abc…");
        assert_eq!(truncate("abc", 4), "abc");
        assert_eq!(label(12), Some('c'));
        assert_eq!(index_for_label('c'), Some(12));
        let r = tmp();
        fs::write(r.join("a.txt"), "").unwrap();
        assert_eq!(unique_target(&r, "a.txt"), r.join("a (2).txt"));
        assert_eq!(unique_target(&r, "b.txt"), r.join("b.txt"));
    }

    #[test]
    fn help_overlay_and_tiny_window() {
        let r = sample();
        let mut app = App::new(&r);
        press(&mut app, KeyCode::Char('?'));
        assert!(screen(&mut app).contains("Canopy keys"));
        press(&mut app, KeyCode::Char('z'));
        assert!(matches!(app.mode, Mode::Normal));
        let mut t = Terminal::new(TestBackend::new(10, 3)).unwrap();
        t.draw(|f| draw(f, &mut app)).unwrap();
    }
}
