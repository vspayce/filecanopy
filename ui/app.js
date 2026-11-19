// Canopy UI: folders as panes on a canvas, each linked to the row it came from.
'use strict';

/* ---------- backend ---------- */
const T = window.__TAURI__;
const invoke = T ? T.core.invoke : mockInvoke;

/* ---------- state ---------- */
const W = 290;          // pane width
const GAP_X = 70;       // horizontal gap between a pane and its branch
const GAP_Y = 26;       // gap between sibling branches
let panes = [];         // {id, path, parent, anchor, el, items, x, y, pinned}
let nextId = 1;
let mode = load('layout', 'right');          // 'right' | 'down'
let showHidden = load('hidden', false);
let selection = new Map();                   // path -> {pane, item}
let lastClicked = null;                      // {pane, index} for shift-ranges
const cam = { x: 40, y: 30, z: 1 };

const $ = (s) => document.querySelector(s);
const viewport = $('#viewport'), world = $('#world'), svg = $('#links');

function load(k, d) { try { const v = localStorage.getItem('canopy.' + k); return v == null ? d : JSON.parse(v); } catch { return d; } }
function save(k, v) { try { localStorage.setItem('canopy.' + k, JSON.stringify(v)); } catch {} }

/* ---------- helpers ---------- */
const SEP = '\\';
function baseName(p) {
  const t = p.replace(/[\\/]+$/, '');
  const i = Math.max(t.lastIndexOf('\\'), t.lastIndexOf('/'));
  return i < 0 || t.length <= 2 ? p : t.slice(i + 1);
}
function parentOf(p) {
  const t = p.replace(/[\\/]+$/, '');
  if (/^[A-Za-z]:$/.test(t) || t === '') return null;
  const i = Math.max(t.lastIndexOf('\\'), t.lastIndexOf('/'));
  if (i < 0) return null;
  const par = t.slice(0, i);
  return /^[A-Za-z]:$/.test(par) ? par + SEP : (par || '/');
}
function fmtSize(n) {
  if (n == null) return '';
  const u = ['B', 'KB', 'MB', 'GB', 'TB'];
  let i = 0;
  while (n >= 1024 && i < u.length - 1) { n /= 1024; i++; }
  return (i === 0 ? n : n.toFixed(n < 10 ? 1 : 0)) + ' ' + u[i];
}
const ICONS = [
  [/\.(png|jpe?g|gif|webp|heic|bmp|svg|ico|tiff?)$/i, '🖼️'],
  [/\.(mp4|mov|mkv|avi|wmv|webm|m4v)$/i, '🎞️'],
  [/\.(mp3|wav|flac|m4a|aac|ogg)$/i, '🎵'],
  [/\.(pdf)$/i, '📕'],
  [/\.(docx?|rtf|odt|pages)$/i, '📝'],
  [/\.(xlsx?|csv|ods|numbers)$/i, '📊'],
  [/\.(pptx?|key|odp)$/i, '📽️'],
  [/\.(zip|7z|rar|tar|gz|xz)$/i, '🗜️'],
  [/\.(exe|msi|bat|cmd|ps1)$/i, '⚙️'],
  [/\.(txt|md|log|json|ya?ml|xml|ini)$/i, '📄'],
  [/\.(js|ts|py|rs|cs|cpp|c|h|java|go|swift|html|css)$/i, '🧩'],
];
function iconFor(item) {
  if (item.dir) return '📁';
  for (const [re, ic] of ICONS) if (re.test(item.name)) return ic;
  return '📄';
}
function esc(s) { return s.replace(/[&<>"]/g, (c) => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;' }[c])); }
const samePath = (a, b) => a && b && a.toLowerCase().replace(/[\\/]+$/, '') === b.toLowerCase().replace(/[\\/]+$/, '');

/* ---------- camera ---------- */
function applyCam() { world.style.transform = `translate(${cam.x}px, ${cam.y}px) scale(${cam.z})`; }
function toWorld(cx, cy) {
  const r = viewport.getBoundingClientRect();
  return { x: (cx - r.left - cam.x) / cam.z, y: (cy - r.top - cam.y) / cam.z };
}

/* ---------- panes ---------- */
function childrenOf(p) { return panes.filter((c) => c.parent === p); }
function descendants(p) { const out = []; for (const c of childrenOf(p)) out.push(c, ...descendants(c)); return out; }

function removePane(p) {
  for (const d of [p, ...descendants(p)]) {
    d.el.remove();
    for (const [k, v] of selection) if (v.pane === d) selection.delete(k);
  }
  const gone = new Set([p, ...descendants(p)]);
  panes = panes.filter((x) => !gone.has(x));
}

async function openPane(path, parent = null, anchor = null, opts = {}) {
  const p = { id: nextId++, path, parent, anchor, items: [], x: 0, y: 0, pinned: false };
  if (opts.x != null) { p.x = opts.x; p.y = opts.y; p.pinned = true; }
  p.el = document.createElement('section');
  p.el.className = 'pane';
  p.el.dataset.id = p.id;
  p.el.innerHTML = `
    <header>
      <span class="title"></span><span class="count"></span>
      <button class="up" title="Open the folder above">↑</button>
      <button class="close" title="Close this branch">✕</button>
    </header>
    <div class="list"><div class="msg">Loading…</div></div>`;
  p.el.querySelector('.title').textContent = baseName(path) || path;
  p.el.querySelector('header').title = path;
  world.appendChild(p.el);
  panes.push(p);
  wirePane(p);
  await loadPane(p);
  if (opts.select) {
    const idx = p.items.findIndex((it) => samePath(it.path, opts.select));
    if (idx >= 0) {
      selectOnly(p, idx);
      const row = p.el.querySelectorAll('.row')[idx];
      if (row) row.scrollIntoView({ block: 'center' });
    }
  }
  layout();
  return p;
}

async function loadPane(p) {
  const list = p.el.querySelector('.list');
  try {
    const items = await invoke('list_dir', { path: p.path });
    p.items = showHidden ? items : items.filter((i) => !i.hidden);
    p.error = null;
  } catch (e) {
    p.items = [];
    p.error = String(e);
  }
  renderPane(p);
}

function renderPane(p) {
  const list = p.el.querySelector('.list');
  const scroll = list.scrollTop;
  p.el.querySelector('.count').textContent = p.error ? '' : `${p.items.length}`;
  if (p.error) { list.innerHTML = `<div class="msg">Can't open this folder.<br><small>${esc(p.error)}</small></div>`; return; }
  if (!p.items.length) { list.innerHTML = '<div class="msg">Empty folder</div>'; return; }
  const openKids = new Set(childrenOf(p).map((c) => c.anchor && c.anchor.toLowerCase()));
  list.innerHTML = p.items.map((it, i) => {
    const cls = ['row'];
    if (selection.has(it.path)) cls.push('sel');
    if (openKids.has(it.path.toLowerCase())) cls.push('open');
    if (it.hidden) cls.push('hidden');
    return `<div class="${cls.join(' ')}" data-i="${i}" title="${esc(it.name)}">
      <span>${iconFor(it)}</span><span class="name">${esc(it.name)}</span>
      <span class="size">${fmtSize(it.size)}</span><span class="chev">${it.dir ? '›' : ''}</span></div>`;
  }).join('');
  list.scrollTop = scroll;
}

function refreshRows(p) { renderPane(p); drawLinks(); }

async function refreshPaths(paths) {
  const want = paths.filter(Boolean).map((x) => x.toLowerCase().replace(/[\\/]+$/, ''));
  for (const p of panes) {
    if (want.includes(p.path.toLowerCase().replace(/[\\/]+$/, ''))) await loadPane(p);
  }
  layout();
}

/* ---------- selection ---------- */
function selectOnly(p, i) {
  const prev = new Set([...selection.values()].map((v) => v.pane));
  selection.clear();
  const it = p.items[i];
  if (it) selection.set(it.path, { pane: p, item: it });
  lastClicked = { pane: p, index: i };
  for (const q of new Set([...prev, p])) if (panes.includes(q)) renderPane(q);
}
function toggleSelect(p, i) {
  const it = p.items[i];
  if (selection.has(it.path)) selection.delete(it.path); else selection.set(it.path, { pane: p, item: it });
  lastClicked = { pane: p, index: i };
  renderPane(p);
}
function rangeSelect(p, i) {
  if (!lastClicked || lastClicked.pane !== p) return selectOnly(p, i);
  const [a, b] = [Math.min(lastClicked.index, i), Math.max(lastClicked.index, i)];
  for (let k = a; k <= b; k++) selection.set(p.items[k].path, { pane: p, item: p.items[k] });
  renderPane(p);
}
function clearSelection() {
  const touched = new Set([...selection.values()].map((v) => v.pane));
  selection.clear();
  for (const q of touched) if (panes.includes(q)) renderPane(q);
}

/* ---------- opening a folder beside its parent ---------- */
async function openChild(p, item, extraBranch) {
  const existing = childrenOf(p).find((c) => samePath(c.anchor, item.path));
  if (existing) { flash(existing.el); return; }
  if (!extraBranch) for (const c of childrenOf(p)) removePane(c);
  await openPane(item.path, p, item.path);
  renderPane(p);
}
function flash(el) { el.animate([{ outline: '3px solid var(--accent)' }, { outline: '3px solid transparent' }], { duration: 600 }); }

/* ---------- layout: a tidy tree, growing right or down ---------- */
function anchorOffset(parent, child) {
  // distance from the parent's top (right mode) or left (down mode) to the row
  const idx = parent.items.findIndex((it) => samePath(it.path, child.anchor));
  const list = parent.el.querySelector('.list');
  const head = parent.el.querySelector('header').offsetHeight;
  const row = parent.el.querySelectorAll('.row')[idx];
  if (!row) return head;
  const y = head + row.offsetTop - list.scrollTop + row.offsetHeight / 2;
  return Math.max(head, Math.min(y, head + list.clientHeight));
}

function layout() {
  const roots = panes.filter((p) => !p.parent);
  let cursor = 0;
  for (const r of roots) {
    if (!r.pinned) {
      if (mode === 'right') { r.x = 0; r.y = cursor; } else { r.x = cursor; r.y = 0; }
    }
    const extent = place(r);
    cursor = (mode === 'right' ? extent.bottom : extent.right) + GAP_Y * 2;
  }
  for (const p of panes) {
    p.el.style.left = p.x + 'px'; p.el.style.top = p.y + 'px';
    p.el.querySelector('.up').hidden = !!p.parent; // only the first pane of a tree can grow upward
  }
  drawLinks();
}

function place(p) {
  const h = p.el.offsetHeight || 120;
  let right = p.x + W, bottom = p.y + h;
  if (mode === 'right') {
    let next = -Infinity;
    for (const c of childrenOf(p)) {
      if (!c.pinned) {
        c.x = p.x + W + GAP_X;
        c.y = Math.max(p.y + anchorOffset(p, c) - 22, next);
      }
      const e = place(c);
      next = e.bottom + GAP_Y;
      right = Math.max(right, e.right); bottom = Math.max(bottom, e.bottom);
    }
  } else {
    let next = -Infinity;
    for (const c of childrenOf(p)) {
      if (!c.pinned) {
        c.y = p.y + h + GAP_X * 0.7;
        c.x = Math.max(p.x + 30, next);
      }
      const e = place(c);
      next = e.right + GAP_Y;
      right = Math.max(right, e.right); bottom = Math.max(bottom, e.bottom);
    }
  }
  return { right, bottom };
}

function drawLinks() {
  const parts = [];
  for (const c of panes) {
    if (!c.parent) continue;
    const p = c.parent;
    let x1, y1, x2, y2, d;
    if (mode === 'right' || c.pinned) {
      x1 = p.x + W; y1 = p.y + anchorOffset(p, c);
      x2 = c.x; y2 = c.y + 18;
      if (x2 < x1) { x2 = c.x + W; }
      const mx = (x1 + x2) / 2;
      d = `M${x1},${y1} C${mx},${y1} ${mx},${y2} ${x2},${y2}`;
    } else {
      x2 = c.x + 30; y2 = c.y;
      x1 = Math.min(Math.max(x2, p.x + 24), p.x + W - 24); y1 = p.y + p.el.offsetHeight;
      const my = (y1 + y2) / 2;
      d = `M${x1},${y1} C${x1},${my} ${x2},${my} ${x2},${y2}`;
    }
    parts.push(`<path d="${d}"/><circle cx="${x1}" cy="${y1}" r="3.5"/><circle cx="${x2}" cy="${y2}" r="3.5"/>`);
  }
  svg.innerHTML = parts.join('');
}

/* ---------- pane interactions ---------- */
function rowIndex(e) { const r = e.target.closest('.row'); return r ? +r.dataset.i : -1; }

function wirePane(p) {
  const list = p.el.querySelector('.list');
  list.addEventListener('scroll', drawLinks);

  p.el.querySelector('.close').onclick = (e) => { e.stopPropagation(); removePane(p); if (p.parent) renderPane(p.parent); layout(); };
  p.el.querySelector('.up').onclick = (e) => { e.stopPropagation(); goUp(p); };

  list.addEventListener('click', (e) => {
    const i = rowIndex(e); if (i < 0) return;
    const it = p.items[i];
    if (e.ctrlKey || e.metaKey) return toggleSelect(p, i);
    if (e.shiftKey) return rangeSelect(p, i);
    selectOnly(p, i);
    if (it.dir) openChild(p, it, e.altKey);
  });
  list.addEventListener('dblclick', (e) => {
    const i = rowIndex(e); if (i < 0) return;
    const it = p.items[i];
    if (!it.dir) invoke('open_path', { path: it.path }).catch(toastError);
  });
  list.addEventListener('contextmenu', (e) => {
    e.preventDefault();
    const i = rowIndex(e);
    if (i >= 0) {
      if (!selection.has(p.items[i].path)) selectOnly(p, i);
      rowMenu(e, p, p.items[i]);
    } else paneMenu(e, p);
  });
  list.addEventListener('mousedown', (e) => {
    if (e.button !== 0) return;
    const i = rowIndex(e); if (i < 0) return;
    e.stopPropagation();
    startRowDrag(e, p, i);
  });
  p.el.addEventListener('mousedown', (e) => e.stopPropagation());
  p.el.addEventListener('wheel', (e) => { if (!e.ctrlKey) e.stopPropagation(); }, { passive: true });

  // drag the header to move a pane by hand
  const head = p.el.querySelector('header');
  head.addEventListener('mousedown', (e) => {
    if (e.target.closest('button') || e.button !== 0) return;
    e.stopPropagation();
    const start = { x: e.clientX, y: e.clientY, px: p.x, py: p.y };
    const move = (ev) => {
      p.pinned = true;
      p.x = start.px + (ev.clientX - start.x) / cam.z;
      p.y = start.py + (ev.clientY - start.y) / cam.z;
      layout();
    };
    const up = () => { removeEventListener('mousemove', move); removeEventListener('mouseup', up); };
    addEventListener('mousemove', move); addEventListener('mouseup', up);
  });
  head.addEventListener('dblclick', (e) => { if (!e.target.closest('button')) { p.pinned = false; layout(); } });
}

async function goUp(p) {
  const par = parentOf(p.path);
  if (!par) return toast('This is the top of the drive.');
  // a root: put its parent on the canvas before it and keep the view still
  const before = { x: p.x, y: p.y };
  const np = await openPane(par, null, null, { select: p.path });
  panes.splice(panes.indexOf(np), 1);
  panes.splice(panes.indexOf(p), 0, np);
  p.parent = np; p.anchor = p.path; p.pinned = false;
  layout();
  renderPane(np);
  cam.x -= (p.x - before.x) * cam.z; cam.y -= (p.y - before.y) * cam.z;
  applyCam();
}

/* ---------- drag and drop ---------- */
function dragPaths(p, i) {
  const it = p.items[i];
  if (selection.has(it.path) && selection.size > 1) return [...selection.keys()];
  return [it.path];
}

function startRowDrag(e, p, i) {
  const sx = e.clientX, sy = e.clientY;
  let started = false;
  const move = (ev) => {
    if (started || Math.hypot(ev.clientX - sx, ev.clientY - sy) < 6) return;
    started = true;
    cleanup();
    if (!selection.has(p.items[i].path)) selectOnly(p, i);
    osDrag(dragPaths(p, i));
  };
  const cleanup = () => { removeEventListener('mousemove', move); removeEventListener('mouseup', cleanup); };
  addEventListener('mousemove', move); addEventListener('mouseup', cleanup);
}

function dragImage(n, label) {
  const c = document.createElement('canvas');
  c.width = 220; c.height = 44;
  const g = c.getContext('2d');
  g.fillStyle = '#2f8f5b';
  g.beginPath(); g.roundRect(2, 2, 216, 40, 10); g.fill();
  g.fillStyle = '#fff'; g.font = '600 15px Segoe UI, sans-serif'; g.textBaseline = 'middle';
  const text = n > 1 ? `${n} items` : label;
  g.fillText(text.length > 24 ? text.slice(0, 23) + '…' : text, 14, 23);
  return c.toDataURL('image/png');
}

// Real Windows drag: works into Explorer, the desktop, email and any app,
// and back onto Canopy's own panes (handled by the drop listener below).
async function osDrag(paths) {
  if (!T) return toast(`(preview) would drag ${paths.length} item(s)`);
  try {
    const ch = new T.core.Channel();
    ch.onmessage = () => {};
    await invoke('plugin:drag|start_drag', {
      item: paths,
      image: dragImage(paths.length, baseName(paths[0])),
      options: { mode: 'copy' },
      onEvent: ch,
    });
  } catch (err) { toastError(err); }
}

function dropTargetAt(clientX, clientY) {
  const el = document.elementFromPoint(clientX, clientY);
  if (!el) return null;
  const paneEl = el.closest('.pane');
  if (!paneEl) return null;
  const p = panes.find((x) => x.id === +paneEl.dataset.id);
  if (!p || p.error) return null;
  const rowEl = el.closest('.row');
  if (rowEl) {
    const it = p.items[+rowEl.dataset.i];
    if (it && it.dir) return { pane: p, el: rowEl, dir: it.path };
  }
  return { pane: p, el: paneEl, dir: p.path };
}

let dropHighlight = null;
function highlight(t) {
  if (dropHighlight && (!t || dropHighlight !== t.el)) dropHighlight.classList.remove('droptarget');
  dropHighlight = t ? t.el : null;
  if (dropHighlight) dropHighlight.classList.add('droptarget');
}

if (T && T.webview) {
  T.webview.getCurrentWebview().onDragDropEvent(async (ev) => {
    const d = ev.payload;
    const pos = d.position ? { x: d.position.x / devicePixelRatio, y: d.position.y / devicePixelRatio } : null;
    if (d.type === 'over' || d.type === 'enter') { if (pos) highlight(dropTargetAt(pos.x, pos.y)); return; }
    if (d.type === 'leave') return highlight(null);
    if (d.type === 'drop') {
      highlight(null);
      const t = pos && dropTargetAt(pos.x, pos.y);
      if (!t || !d.paths || !d.paths.length) return;
      await doTransfer(d.paths, t.dir);
    }
  });
}

let lastMoves = null;
async function doTransfer(paths, dest, how = 'auto') {
  const done = await invoke('transfer', { paths, dest, mode: how });
  const ok = done.filter((x) => x.op !== 'error');
  const bad = done.filter((x) => x.op === 'error');
  const moved = ok.filter((x) => x.op === 'move');
  clearSelection();
  await refreshPaths([dest, ...paths.map(parentOf)]);
  if (!done.length) return;
  const verb = moved.length === ok.length ? 'Moved' : moved.length ? 'Moved/copied' : 'Copied';
  let msg = ok.length ? `${verb} ${ok.length} item${ok.length > 1 ? 's' : ''} to ${baseName(dest)}` : '';
  if (bad.length) msg += (msg ? ' · ' : '') + `${bad.length} failed: ${bad[0].error}`;
  lastMoves = moved.length ? moved : null;
  toast(msg, lastMoves ? { label: 'Undo', fn: undoLast } : null);
}
async function undoLast() {
  if (!lastMoves) return;
  const moves = lastMoves; lastMoves = null;
  const errs = await invoke('undo_moves', { moves });
  await refreshPaths(moves.flatMap((m) => [parentOf(m.from), parentOf(m.to)]));
  toast(errs.length ? errs[0] : 'Put back.');
}

/* ---------- menus ---------- */
const menu = $('#menu');
function showMenu(e, entries) {
  menu.innerHTML = '';
  for (const en of entries) {
    if (en === '-') { menu.appendChild(document.createElement('hr')); continue; }
    const d = document.createElement('div');
    d.textContent = en[0];
    d.onclick = () => { hideMenu(); en[1](); };
    menu.appendChild(d);
  }
  menu.hidden = false;
  const r = menu.getBoundingClientRect();
  menu.style.left = Math.min(e.clientX, innerWidth - r.width - 8) + 'px';
  menu.style.top = Math.min(e.clientY, innerHeight - r.height - 8) + 'px';
}
function hideMenu() { menu.hidden = true; }
addEventListener('mousedown', (e) => { if (!menu.contains(e.target)) hideMenu(); }, true);

function rowMenu(e, p, it) {
  const sel = [...selection.keys()];
  const items = [
    [it.dir ? 'Open beside' : 'Open', () => it.dir ? openChild(p, it, true) : invoke('open_path', { path: it.path }).catch(toastError)],
    ['Show in Explorer', () => invoke('reveal', { path: it.path }).catch(toastError)],
    ['Rename', () => startRename(p, p.items.indexOf(it))],
    ['Copy path', () => copyText(sel.length > 1 ? sel.join('\r\n') : it.path)],
  ];
  if (it.dir) items.push('-', ['Open as new tree', () => newRoot(it.path)]);
  showMenu(e, items);
}
function paneMenu(e, p) {
  showMenu(e, [
    ['New folder', async () => {
      try { const np = await invoke('new_folder', { dir: p.path }); await loadPane(p); layout(); startRename(p, p.items.findIndex((i) => samePath(i.path, np))); }
      catch (err) { toastError(err); }
    }],
    ['Open in Explorer', () => invoke('open_path', { path: p.path }).catch(toastError)],
    ['Refresh', async () => { await loadPane(p); layout(); }],
    ['Copy path', () => copyText(p.path)],
  ]);
}
async function copyText(t) {
  try { await navigator.clipboard.writeText(t); toast('Path copied.'); } catch { toast(t); }
}

function startRename(p, i) {
  if (i < 0) return;
  const it = p.items[i];
  const row = p.el.querySelectorAll('.row')[i];
  const nameEl = row.querySelector('.name');
  const input = document.createElement('input');
  input.value = it.name;
  nameEl.replaceWith(input);
  input.focus();
  const dot = it.dir ? -1 : it.name.lastIndexOf('.');
  input.setSelectionRange(0, dot > 0 ? dot : it.name.length);
  let finished = false;
  const finish = async (commit) => {
    if (finished) return; finished = true;
    if (commit && input.value !== it.name) {
      try { await invoke('rename', { path: it.path, newName: input.value }); } catch (err) { toastError(err); }
    }
    await loadPane(p); layout();
  };
  input.onkeydown = (ev) => { ev.stopPropagation(); if (ev.key === 'Enter') finish(true); if (ev.key === 'Escape') finish(false); };
  input.onblur = () => finish(true);
  input.onmousedown = (ev) => ev.stopPropagation();
}

/* ---------- toast ---------- */
let toastTimer;
function toast(msg, action) {
  const t = $('#toast');
  t.innerHTML = '';
  const s = document.createElement('span'); s.textContent = msg; t.appendChild(s);
  if (action) { const b = document.createElement('button'); b.textContent = action.label; b.onclick = () => { t.hidden = true; action.fn(); }; t.appendChild(b); }
  t.hidden = false;
  clearTimeout(toastTimer);
  toastTimer = setTimeout(() => (t.hidden = true), action ? 8000 : 3500);
}
function toastError(e) { toast(String(e)); }

/* ---------- canvas pan and zoom ---------- */
viewport.addEventListener('mousedown', (e) => {
  if (e.button !== 0 && e.button !== 1) return;
  const start = { x: e.clientX, y: e.clientY, cx: cam.x, cy: cam.y };
  let moved = false;
  viewport.classList.add('panning');
  const move = (ev) => { moved = true; cam.x = start.cx + ev.clientX - start.x; cam.y = start.cy + ev.clientY - start.y; applyCam(); };
  const up = () => {
    viewport.classList.remove('panning');
    if (!moved) clearSelection();
    removeEventListener('mousemove', move); removeEventListener('mouseup', up);
  };
  addEventListener('mousemove', move); addEventListener('mouseup', up);
});
viewport.addEventListener('wheel', (e) => {
  e.preventDefault();
  if (e.ctrlKey) {
    const before = toWorld(e.clientX, e.clientY);
    cam.z = Math.min(2, Math.max(0.25, cam.z * Math.exp(-e.deltaY * 0.0015)));
    const r = viewport.getBoundingClientRect();
    cam.x = e.clientX - r.left - before.x * cam.z;
    cam.y = e.clientY - r.top - before.y * cam.z;
  } else {
    cam.x -= e.shiftKey ? e.deltaY : e.deltaX;
    cam.y -= e.shiftKey ? 0 : e.deltaY;
  }
  applyCam();
}, { passive: false });

function fit() {
  if (!panes.length) return;
  let minX = Infinity, minY = Infinity, maxX = -Infinity, maxY = -Infinity;
  for (const p of panes) {
    minX = Math.min(minX, p.x); minY = Math.min(minY, p.y);
    maxX = Math.max(maxX, p.x + W); maxY = Math.max(maxY, p.y + p.el.offsetHeight);
  }
  const r = viewport.getBoundingClientRect(), pad = 40;
  cam.z = Math.min(1.2, Math.max(0.25, Math.min((r.width - pad * 2) / (maxX - minX), (r.height - pad * 2) / (maxY - minY))));
  cam.x = pad - minX * cam.z + ((r.width - pad * 2) - (maxX - minX) * cam.z) / 2;
  cam.y = pad - minY * cam.z;
  applyCam();
}

/* ---------- toolbar ---------- */
async function newRoot(path, add = false, select = null) {
  if (!add) for (const r of panes.filter((p) => !p.parent)) removePane(r);
  const p = await openPane(path, null, null, { select });
  if (!add) { cam.x = 40; cam.y = 30; cam.z = 1; applyCam(); }
  return p;
}

async function setupPlaces() {
  const places = $('#places');
  const home = await invoke('home');
  const mk = (label, path, title) => {
    const b = document.createElement('button');
    b.textContent = label; b.title = (title || path) + ' — Shift-click to add beside the current tree';
    b.onclick = (e) => newRoot(path, e.shiftKey);
    places.appendChild(b);
  };
  mk('🏠 Home', home);
  mk('🖥️ Desktop', home + SEP + 'Desktop');
  mk('⬇️ Downloads', home + SEP + 'Downloads');
  for (const d of await invoke('drives')) mk('💽 ' + d.replace(/\\$/, ''), d, 'Drive ' + d);
  return home;
}

$('#layout').onclick = () => {
  mode = mode === 'right' ? 'down' : 'right';
  save('layout', mode);
  for (const p of panes) p.pinned = false;
  $('#layout').textContent = mode === 'right' ? '→ Right' : '↓ Down';
  layout();
};
$('#hidden').onclick = async () => {
  showHidden = !showHidden; save('hidden', showHidden);
  $('#hidden').textContent = 'Hidden: ' + (showHidden ? 'on' : 'off');
  for (const p of panes) await loadPane(p);
  layout();
};
$('#fit').onclick = fit;
$('#layout').textContent = mode === 'right' ? '→ Right' : '↓ Down';
$('#hidden').textContent = 'Hidden: ' + (showHidden ? 'on' : 'off');

addEventListener('keydown', (e) => {
  if (e.target.tagName === 'INPUT') return;
  if (e.key === 'Escape') { clearSelection(); hideMenu(); }
  if (e.key === 'f' || e.key === 'F') fit();
  if (e.key === 'F5') { e.preventDefault(); Promise.all(panes.map(loadPane)).then(layout); }
  if (e.key === 'F2' && selection.size === 1) { const v = [...selection.values()][0]; startRename(v.pane, v.pane.items.indexOf(v.item)); }
  if ((e.ctrlKey || e.metaKey) && e.key.toLowerCase() === 'f') { e.preventDefault(); $('#q').focus(); }
});

/* ---------- search ---------- */
const q = $('#q'), results = $('#results');
let searchTimer, hits = [], active = -1;
q.addEventListener('input', () => { clearTimeout(searchTimer); searchTimer = setTimeout(runSearch, 120); });
q.addEventListener('keydown', (e) => {
  if (e.key === 'ArrowDown') { active = Math.min(hits.length - 1, active + 1); paintResults(); e.preventDefault(); }
  if (e.key === 'ArrowUp') { active = Math.max(0, active - 1); paintResults(); e.preventDefault(); }
  if (e.key === 'Enter' && hits.length) revealHit(hits[Math.max(0, active)]);
  if (e.key === 'Escape') { results.hidden = true; q.blur(); }
});
q.addEventListener('focus', () => { if (hits.length) results.hidden = false; });
addEventListener('mousedown', (e) => { if (!e.target.closest('.search')) results.hidden = true; }, true);

async function runSearch() {
  const text = q.value.trim();
  if (!text) { results.hidden = true; hits = []; return; }
  hits = await invoke('search', { query: text, limit: 200 });
  active = hits.length ? 0 : -1;
  paintResults();
}
function paintResults() {
  results.hidden = false;
  if (!hits.length) {
    const s = lastStatus;
    results.innerHTML = `<div class="result"><span></span><span class="empty">${s && s.building ? 'Nothing yet — still indexing your drives…' : 'No matches.'}</span></div>`;
    return;
  }
  results.innerHTML = hits.map((h, i) => `
    <div class="result ${i === active ? 'active' : ''}" data-i="${i}">
      <span>${iconFor(h)}</span><span>${esc(h.name)}</span><span class="size">${fmtSize(h.size)}</span>
      <span class="where">${esc(parentOf(h.path) || '')}</span>
    </div>`).join('');
  results.querySelectorAll('.result').forEach((el) => {
    el.onclick = () => revealHit(hits[+el.dataset.i]);
    el.oncontextmenu = (e) => { e.preventDefault(); invoke('reveal', { path: hits[+el.dataset.i].path }); };
  });
  const a = results.querySelector('.active'); if (a) a.scrollIntoView({ block: 'nearest' });
}
async function revealHit(h) {
  results.hidden = true;
  const par = parentOf(h.path) || h.path;
  const root = await newRoot(par, false, h.path);
  if (h.dir) {
    const it = root.items.find((x) => samePath(x.path, h.path));
    if (it) await openChild(root, it, false);
  }
}

/* ---------- index status ---------- */
let lastStatus = null;
async function pollStatus() {
  try {
    const s = await invoke('index_status');
    lastStatus = s;
    const n = s.indexed.toLocaleString();
    $('#status').textContent = s.building ? `Indexing… ${s.scanned.toLocaleString()} items` : `${n} items indexed`;
    if (s.building) {
      setTimeout(pollStatus, 1000);
    } else if (!pollStatus.finished) {
      pollStatus.finished = true;
      for (const p of panes) await loadPane(p); // folder sizes are known now
      layout();
    }
  } catch { setTimeout(pollStatus, 3000); }
}

/* ---------- start ---------- */
(async function start() {
  applyCam();
  const home = await setupPlaces();
  await newRoot(home);
  pollStatus();
  setTimeout(() => $('#hint').classList.add('gone'), 12000);
})();

/* ---------- preview without the app (open ui/index.html in a browser) ---------- */
function mockInvoke(cmd, a) {
  const tree = {
    'C:\\Users\\you': ['Desktop/', 'Documents/', 'Downloads/', 'Pictures/', 'notes.txt'],
    'C:\\Users\\you\\Documents': ['Taxes/', 'Recipes/', 'resume.docx', 'budget.xlsx'],
    'C:\\Users\\you\\Documents\\Recipes': ['soup.pdf', 'bread.md'],
    'C:\\Users\\you\\Pictures': ['Japan 2026/', 'IMG_0001.jpg', 'IMG_0002.jpg'],
  };
  const r = {
    home: () => 'C:\\Users\\you',
    drives: () => ['C:\\', 'D:\\'],
    index_status: () => ({ scanned: 0, indexed: 1234, building: false, started: true }),
    search: () => [],
    list_dir: () => (tree[a.path] || []).map((n) => {
      const dir = n.endsWith('/'); const name = n.replace('/', '');
      return { name, path: a.path + '\\' + name, dir, size: dir ? 12345678 : 4321, modified: 0, hidden: false };
    }),
  }[cmd];
  return Promise.resolve(r ? r() : null);
}
