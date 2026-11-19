# Canopy in the terminal (`canopy-cli`)

The same idea as the Windows app (folders laid out side by side, each linked
to the one it came from), drawn with text characters so it runs in Windows
Terminal, PowerShell, macOS Terminal, iTerm, Linux terminals and over SSH.

```
 [0] C:\ › [1] Users › [2] Jason › [3] Projects › [4] filecanopy
┌ Projects ─────────────────────┐   ┌ filecanopy ───────────────────┐
│▸ airlock                      │ ┌─│▸ cli                          │
│▶ filecanopy                   │─┘ │▸ docs                         │
│▸ schofield                    │   │▸ src-tauri                    │
│  notes.txt                1.2K│   │  README.md                1.9K│
└───────────────────────────────┘   └───────────────────────────────┘
 Copied path                                                  ? help
```

## The path strip

The top line is a visible version of the `pushd`/`popd` stack. Each folder you
went through gets a number (0–9, then a–z).

- **Space, then the number** jumps straight back to that folder. Two separate
  presses, so a stray digit never moves you somewhere by accident. While you're
  in "jump" mode the numbers light up; Esc cancels.
- **Click** a folder in the strip to jump there.
- **Ctrl+Home** goes to the start of the path, **Ctrl+End** to the deepest folder.
- **Backspace** (or ←) steps back one. At the first folder it adds the parent,
  so the map grows upward.
- Jumping back doesn't close the deeper folders: they dim, and **→/Enter** on the
  same folder walks back down. Opening a *different* folder replaces them.
- **Ctrl+number** or **Alt+number** also jump, in terminals that send those keys
  (kitty, WezTerm, Ghostty, and newer Windows Terminal). Older terminals turn
  Ctrl+number into control codes (Ctrl+3 is Esc, Ctrl+4 can force-quit), which
  is why Space+number is the default.

## Plain digits are counts

Like Vim, typing a number before a move repeats it: `3` then ↓ moves down three.
Digits on their own never jump.

## Other keys

| Key | Does |
| --- | --- |
| ↑ ↓ or j k | move (PgUp/PgDn, Home/End too) |
| → Enter l | open folder (beside this one) or open file |
| ← Backspace h | back one folder |
| / | filter this folder as you type; Esc clears |
| c | copy the selected path to the clipboard |
| o | show it in Explorer / Finder |
| y, x, p | copy, cut, paste into the focused folder |
| r or F2 | rename · n new folder |
| m + letter | mark a folder · ' + letter jumps to the mark |
| . | show hidden files · F5 refresh |
| ? | help · q quit |

## Mouse

Click to select, double-click to open, scroll a pane with the wheel. Drag a file
onto another pane or onto a folder row and Canopy asks **m**ove or **c**opy.
Dragging *out* to Explorer isn't possible from a terminal, so **c** (copy path)
and **o** (show in Explorer) cover that.

## Safety

Like the app, `canopy-cli` never deletes anything. A move across drives becomes
a copy and the original is kept. Name clashes get " (2)" added instead of
overwriting.

## Hand-off to your shell

`canopy-cli --print-dir` prints the folder you were in when you quit with **q**,
so your shell can follow you there:

PowerShell (`notepad $PROFILE`):

```powershell
function cn { $d = canopy-cli --print-dir @args; if ($d) { Set-Location $d } }
```

bash/zsh:

```sh
cn() { local d; d="$(canopy-cli --print-dir "$@")" && [ -n "$d" ] && cd "$d"; }
```

## Later

- A drives list above `C:\` on Windows.
- Folder sizes from the app's index.
- Remembering marks between runs.
- A helper window that lets you drag the selected file out to other apps.
