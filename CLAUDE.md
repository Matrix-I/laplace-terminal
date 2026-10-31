# Litty Terminal — Rules

Cross-platform, zero-latency GPU terminal emulator (macOS Metal / Linux Vulkan, Wayland, X11).

## Core Invariants & Safety
- **Performance**: < 50MB idle RAM, 0% idle CPU (`ControlFlow::Wait`), zero heap allocation on hot paths (PTY parsing, scrolling, render loop).
- **Grid Cell**: `Cell` struct must stay exactly 12 bytes (`static_assert`). Pack new state into existing bits.
- **Concurrency & Locking**: Minimize grid `Mutex` hold time. Never perform blocking I/O (PTY writes, subprocesses, file ops) while holding it.
- **Process & Security**:
  - Only async-signal-safe calls after `fork()`; prepare all allocations beforehand.
  - Strip `ESC` (`\x1b[201~`) from pasted text to prevent bracketed paste escapes.
  - User/terminal URLs to `open` / `xdg-open` must pass as a single argument (never via shell).
  - Shell history writes: mutate raw bytes, write temp file, atomic `rename`.

## Keybinding & Platform Parity
- **Modifier Rules**: macOS uses `Cmd`. Linux terminal actions (Copy, Paste, Tab, Quit) **MUST** use `Ctrl+Shift` (or `Super`) to avoid clashing with shell/readline control characters (`Ctrl+C/V/W/N/T`). Navigation/UI can use `Ctrl` or `Alt`.
- **Primary Bindings Matrix (macOS vs Linux)**:
  - Copy / Paste: `Cmd+C` / `Cmd+V` ↔ `Ctrl+Shift+C` / `Ctrl+Shift+V`
  - Tabs (New / Close): `Cmd+T` / `Cmd+W` ↔ `Ctrl+Shift+T` / `Ctrl+Shift+W`
  - Window (New / Quit): `Cmd+N` / `Cmd+Q` ↔ `Ctrl+Shift+N` / `Ctrl+Shift+Q`
  - Splits (V / H): `Cmd+D` / `Cmd+Shift+D` ↔ `Ctrl+Shift+D` / `Ctrl+Shift+E` (or `Super+Shift+D`)
  - Clear Scrollback: `Cmd+K` ↔ `Ctrl+Shift+K`
  - Quick Zoom: `Cmd + / - / 0` ↔ `Ctrl + / - / 0`
  - Tabs 1..9: `Cmd+1..9` ↔ `Ctrl+1..9` / `Alt+1..9`
  - Jump Word: `Opt+Left/Right` ↔ `Ctrl+Left/Right` (avoid macOS Mission Control collision)
  - Search / History: `Cmd+F` ↔ `Ctrl+Shift+F`; `Ctrl+R` fuzzy search (preserve vim redo in alt screen)
  - UI hints: macOS renders `⌘`, Linux renders `^` or `Alt+`.

## Architecture & Modules
- `src/main.rs`: winit loop, state coordination, action dispatch, mouse & IME.
- `src/keymap.rs`: Pure key-to-byte / shortcut resolution for `Platform::MacOs` & `Platform::Linux` + Kitty protocol.
- `src/pty.rs`: `openpty` + `fork`/`execve`, reader thread pushing bytes to grid.
- `src/grid.rs`: 12-byte `Cell`, ring buffer, `vte::Perform`. Responses (DSR, DA) go to `Grid::response`.
- `src/renderer.rs` & `src/atlas.rs`: wgpu pipelines, single instanced draw call (`shader.wgsl`), glyph cache, font fallback.
- `src/split.rs`: Pane tree layout management.

## Workflow & PR Checklist
1. All keybinding additions must update `src/keymap.rs` for both platforms with dedicated unit tests.
2. Verification steps:
   - `cargo check`
   - `cargo check --target x86_64-unknown-linux-gnu`
   - `cargo test` (run outside sandbox; PTY tests require `openpty`)
   - `cargo clippy --all-targets` (zero warnings tolerated)
3. Conventional Commits format (`feat(scope):`, `fix(scope):`, `docs(scope):`).
