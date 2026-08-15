# ratatata

A tiny terminal text editor built with [ratatui](https://github.com/ratatui/ratatui).

Two panes, one goal: a small, dependency-light editor that stays out of your way.

```
┌──────────────────────────────┬────────────────────────────────┐
│ ~/proj/                      │ ~/proj/src/main.rs             │
│                              │   1 fn main() {                │
│ ▶ src/                       │   2     let msg = "hi";        │
│   Cargo.toml                 │   3     println!("{msg}");     │
│   README.md                  │   4 }                          │
│   main.rs                    │                                │
│                              │                                │
└──────────────────────────────┴────────────────────────────────┘
 EDITOR ~/proj/src/main.rs [Rust] ○ saved   1:1  Ctrl+O switch · ...
```

## Features

- **Sidebar + editor split** — browse directories on the left, edit files on the right
- **Syntax highlighting** — Sublime Text grammars via [syntect](https://github.com/trishume/syntect), detected by extension and first-line heuristics; re-highlights incrementally as you type, only re-parsing lines from the edit point onward
- **Clipboard integration** — copy/cut/paste through the system clipboard ([arboard](https://github.com/1Password/arboard)), with bracketed-paste support for terminals that send it
- **Full mouse support** — click to place the cursor, drag or Shift+click to select, scroll wheel to move through text and directories
- **macOS-friendly keys** — on terminals supporting the kitty keyboard protocol, Cmd+key works like Ctrl (unsupported terminals just ignore the request)
- **Safe file handling** — a dirty buffer blocks opening another file, and Ctrl+Q asks for confirmation before discarding unsaved changes
- **Unicode-aware editing** — the cursor tracks *characters*, not bytes, so wide characters and non-ASCII text render and edit correctly

## Usage

```sh
rat [path]
```

| Argument | Behavior |
| --- | --- |
| *(none)* | Browse the current directory |
| `file.txt` | Open the file |
| `dir/` | Browse the directory |
| `new.txt` (doesn't exist) | Start an untitled buffer bound to that path (created on first save) |

### Keys

| Keys | Action |
| --- | --- |
| `Ctrl+O` | Switch between sidebar and editor |
| `Ctrl+S` | Save (asks for a name if untitled) |
| `Ctrl+C` / `Ctrl+X` / `Ctrl+V` | Copy / cut / paste |
| `Ctrl+A` | Select all |
| `Ctrl+Q` | Quit (press twice when there are unsaved changes) |
| `Esc` / `Enter` | Cancel / confirm the save-as prompt |

**Sidebar:** `↑`/`↓` select, `Enter` open (directory = enter, file = open), `Backspace` go up, `Home`/`End`, `PgUp`/`PgDn` page.

**Editor:** type to insert, `←`/`→`/`↑`/`↓` move (hold `Shift` to extend the selection), `Home`/`End`, `PgUp`/`PgDn`, `Backspace`, `Delete`, `Tab` inserts four spaces.

**Mouse:** click the editor to move the cursor, drag or Shift+click to select, scroll to move; single-click the sidebar to select, double-click to open, scroll to browse.

## Installation

```sh
cargo install --path .
```

Or run in place:

```sh
cargo run -- [path]
```

Installs as the `rat` command (the binary name is set explicitly in `Cargo.toml`, separate from the `ratatata` package name). Requires Rust 2024 edition (Rust 1.85+). Works in any terminal that supports crossterm's event and drawing APIs — including tmux/`screen` when mouse reporting is enabled.

## Testing

```sh
cargo test
```

The test suite covers buffer editing, save/save-as flows, dirty-buffer guards, sidebar navigation, mouse interactions, and headless rendering (including syntax-highlight colors) via ratatui's `TestBackend`.

## How it works

```
src/
├── main.rs       entry point: arg resolution, terminal setup (mouse,
│                 bracketed paste, kitty keyboard protocol), event loop
├── app.rs        application state, key/mouse handling, rendering,
│                 save-as flow, status bar
├── buffer.rs     line-based text buffer with char-indexed cursor,
│                 selection, scrolling, load/save
├── sidebar.rs    scrollable directory listing (dirs first, ".." entry)
├── highlight.rs  incremental syntect highlighting with per-line cached
│                 parse states and scope stacks
└── clipboard.rs  system clipboard behind a small trait (tests use a fake)
```

Highlighting follows the pattern recommended in syntect's `HighlightState` docs: the parse state and scope stack after each line are cached, so an edit invalidates only the lines from the edit point onward, and they're re-parsed only when they become visible. The syntax definitions and theme (`base16-ocean.dark`) are loaded once into process-wide statics.
