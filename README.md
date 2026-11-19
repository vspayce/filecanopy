# Canopy

A file browser for Windows that shows your folders side by side on a canvas,
each one linked to the folder it came from, so you can see the path you took.

- **Click a folder** to open it beside its parent. **Alt-click** opens another
  branch without closing the first. **↑** on the first pane grows the tree upward.
- **Drag files anywhere**: between panes, onto a folder row, and out to Explorer,
  the desktop, email or any other app. Files dragged in from Explorer land in the
  pane you drop them on. Like Explorer, dropping on the same drive moves and on
  another drive copies; a move can be undone from the notice that appears.
  Dragging *out* of Canopy always copies.
- **Ctrl-click** selects across folders; **Shift-click** selects a range.
- **Search every file** as you type. Canopy keeps its own index of your drives,
  on your PC only, which also gives every folder its size.
- **→ Right / ↓ Down** switches how branches grow. Drag a pane's header to move it,
  double-click the header to put it back. Drag the background to pan,
  Ctrl+wheel to zoom, **F** to fit, **F2** to rename, **F5** to refresh.

Canopy never deletes anything, has no account and sends nothing anywhere.

## Install

Each push builds a Windows installer: open the latest run under **Actions**,
download **Canopy Windows installer**, unzip and run it. It isn't code-signed
yet, so SmartScreen shows a warning: **More info → Run anyway**.

## Build it yourself

Rust, Node 20+, then `npm install`, `npx tauri icon app-icon.png`, `npx tauri build`.
The interface is plain HTML/JS in `ui/` and also opens in a browser with sample
folders, for working on the look.

## In the terminal

`canopy-cli` is the same canvas drawn in text, for Windows Terminal, PowerShell,
macOS Terminal and SSH. The top line numbers every folder on your path;
**Space + number** jumps back to it, **Ctrl+Home/End** goes to either end, and
**?** lists the rest. Each push also builds it: download **canopy-cli Windows**
(or macOS / Linux) from the latest run under **Actions**. Details and a shell
hand-off snippet are in [docs/terminal-version.md](docs/terminal-version.md).

## Next ideas

- Read the NTFS master file table and change journal directly (how "Everything"
  works) so the index builds in seconds and stays live.
- Thumbnails in panes, a gallery view, remembering the canvas between launches.
