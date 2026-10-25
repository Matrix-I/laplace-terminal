# Laplace Terminal — Claude Code Rules

## Cross-Platform Keyboard Shortcuts & Modifier Mapping Rules

Laplace is designed to run natively and with zero latency on both **macOS** (Metal) and **Linux** (Vulkan/Wayland/X11). Every time a new shortcut, mouse gesture, or window interaction is added or modified, you **MUST** ensure full feature parity between macOS and Linux.

---

### 1. Modifier Mapping Conventions

On macOS, terminal applications use `Command` (`super_key`) as the primary modifier. On Linux, `Command` does not exist on standard keyboards; instead, `Control` (`ctrl`) and the `Super` (Windows) key are used.

#### Terminal Collision Rule (Crucial for Linux)
In a terminal emulator, bare `Ctrl + <letter>` keys are reserved for shell/readline control characters (e.g., `Ctrl+C` sends SIGINT, `Ctrl+V` sends verbatim byte, `Ctrl+W` deletes backward word, `Ctrl+T` transposes characters, `Ctrl+N` navigates command history).
* **Terminal actions on Linux** (Copy, Paste, New Tab, Close Tab, Quit) **MUST** use `Ctrl + Shift + <Key>` (and should accept `Super + <Key>` as an alternate modern desktop binding).
* **Navigation & UI actions on Linux** (Open Config, Direct Tab Switch 1..9, Zoom, Click Link) can use `Ctrl + <Key>` or `Ctrl + Alt + <Key>` because they do not collide with shell input.

---

### 2. Standard Shortcut Matrix

| Feature | macOS (`target_os = "macos"`) | Linux (`not(target_os = "macos")`) | Rationale / Behavior |
| :--- | :--- | :--- | :--- |
| **New / Duplicate Tab** | `Cmd + T` or `Cmd + N` | `Ctrl + Shift + T` / `Ctrl + Shift + N` *(or `Super + T/N`)* | Bare `Ctrl+T` is needed for shell transpose/fzf; bare `Ctrl+N` is history next. |
| **Close Tab** | `Cmd + W` | `Ctrl + Shift + W` *(or `Super + W`)* | Bare `Ctrl+W` is delete word in bash/zsh. |
| **Copy Selection** | `Cmd + C` | `Ctrl + Shift + C` *(or `Super + C`)* | Bare `Ctrl+C` sends SIGINT (`\x03`). |
| **Paste Clipboard** | `Cmd + V` | `Ctrl + Shift + V` *(or `Super + V`)* | Bare `Ctrl+V` sends verbatim escape (`\x16`). |
| **Quit Application** | `Cmd + Q` | `Ctrl + Shift + Q` *(or `Super + Q`)* | Bare `Ctrl+Q` is XON resume flow control. |
| **Minimize Window** | `Cmd + M` | `Ctrl + Shift + M` *(or `Super + M`)* | Bare `Ctrl+M` is carriage return (`\r`) in terminal. |
| **Open Config** | `Cmd + ,` | `Ctrl + ,` *(or `Super + ,`)* | Opens `~/.laplace/config` in `$EDITOR`. |
| **Reload Config** | `Cmd + Shift + ,` (or `Cmd + <`) | `Ctrl + Shift + ,` (or `Ctrl + <`) | Reloads config on the fly. |
| **Select Tab 1..9** | `Cmd + 1..9` | `Ctrl + 1..9` *(matches `^1..^9` badge)*, `Alt + 1..9`, `Super + 1..9` | Tab 9 jumps to last tab if $\ge 9$ tabs. |
| **Cycle Tabs (Prev / Next)** | `Cmd + Shift + [` / `Cmd + Shift + ]` | `Ctrl + Shift + [` / `Ctrl + Shift + ]` | Works alongside `Ctrl + Tab` and `Ctrl + PageUp / PageDown`. |
| **Navigate Tabs (Left / Right)**| `Cmd + Option + Left / Right` | `Ctrl + Alt + Left / Right` *(or `Super + Alt`)* | Direct parallel mapping. |
| **Zoom In** | `Cmd + =` / `Cmd + +` | `Ctrl + =` / `Ctrl + +` *(with or without Shift, or Super)* | Increments font size by +1.0 pt. |
| **Zoom Out** | `Cmd + -` | `Ctrl + -` / `Ctrl + _` *(with or without Shift, or Super)* | Decrements font size by -1.0 pt. |
| **Reset Zoom** | `Cmd + 0` | `Ctrl + 0` *(with or without Shift, or Super)* | Resets font size to configured default. |
| **Open URL / Path Click** | `Cmd + Click` | `Ctrl + Click` *(or `Super + Click`)* | Opens target with `open` (macOS), `xdg-open` (Linux). |
| **Hover URL Underline** | `Cmd + Mouse Move` | `Ctrl + Mouse Move` *(or `Super + Mouse Move`)* | Shows pointer cursor and URL preview pill. |
| **Bypass Mouse Reporting** | `Shift + Click / Drag` | `Shift + Click / Drag` | Local selection while an app tracks the mouse; `Cmd`/`Super + Click` still opens links. |
| **CLI History Search** | `Ctrl + R` *(or `Cmd + R`)* | `Ctrl + R` *(or `Super + R` / `Ctrl + Shift + R`)* | Zero-latency fuzzy history search; preserves vim redo in alternate screen. |
| **Split Pane Right (Vertical)** | `Cmd + D` | `Ctrl + Shift + D` *(or `Super + D`)* | Splits active pane vertically with hairline divider. |
| **Split Pane Down (Horizontal)** | `Cmd + Shift + D` | `Ctrl + Shift + E` / `Super + Shift + D` / `Ctrl + Alt + D` | Splits active pane horizontally. |
| **Clear Screen & Scrollback** | `Cmd + K` | `Ctrl + Shift + K` | Clears scrollback history buffer and redraws prompt at top (matches iTerm2/Terminal.app/Ghostty). |
| **Delete Before Cursor** | `Ctrl + U` | `Ctrl + U` | Deletes text from prompt to cursor (preserves prompt & text after cursor; preserves vim half-page scroll in alt screen). |
| **Delete After Cursor** | `Ctrl + K` | `Ctrl + K` | Deletes text from cursor to end of command line (`kill-line`). |
| **Jump Word Backward / Forward** | `Option + Left / Right` | `Ctrl + Left / Right` *(or `Alt + Left / Right`)* | Jumps cursor backward/forward by word (`\x1bb` / `\x1bf`). On macOS, strictly `Option+Arrows` to avoid Mission Control Spaces collision. |
| **Toggle Split Zoom** | `Cmd + Shift + Enter` | `Ctrl + Shift + Enter` *(or `Super + Shift + Enter`)* | Zooms active split to full tab size and back. |
| **Equalize Splits** | `Cmd + Option + =` / `Cmd + Ctrl + =` | `Ctrl + Alt + =` *(or `Super + Alt + =`)* | Balances split ratios evenly across pane tree. |
| **Navigate Splits / Tabs** | `Cmd + Arrows` *(or `Cmd + Option + Arrows`)* | `Ctrl + Alt + Arrows` *(or `Super + Arrows`)* | Directional spatial navigation between splits (cycles tabs at edges). |
| **Cycle Splits (Prev / Next)** | `Cmd + [` / `Cmd + ]` | `Ctrl + Alt + [` / `Ctrl + Alt + ]` *(or `Super + [` / `]`)* | Cycles focus across split panes in active tab. |
| **Resize Active Split** | `Cmd + Ctrl + Arrows` | `Super + Ctrl + Arrows` / `Ctrl + Alt + Shift + Arrows` | Dynamically nudges layout tree split ratios. |

---

### 3. Implementation Checklist for New Shortcuts

When introducing any new key binding, add it to `src/keymap.rs` (`resolve_char` / `resolve_named`) and execute it in `run_action` / `run_tab_action` in `src/main.rs`; mouse interactions live in `src/main.rs` (encoding in `src/mouse.rs`):
1. **Define both platforms in the keymap** (`Platform::MacOs` / `Platform::Linux`) and add a case to the keymap tests — both tables are tested on any host.
2. **Check for PTY collisions**: Verify if the proposed `Ctrl + <Key>` combination emits an ASCII control code (`\x00`..`\x1f`) needed by shell or text editors (vim, nano). If it does, use `Ctrl + Shift` instead.
3. **Verify UI representation**: If the UI renders a badge or hint (e.g. `src/renderer.rs`), ensure macOS displays `⌘` and Linux displays `^` (for Ctrl) or `Alt+`, and ensure the actual key handler matches the visual symbol.
4. **Compile verification**:
   - Check macOS build: `cargo check`
   - Check Linux build: `cargo check --target x86_64-unknown-linux-gnu`

---

## Project Layout

| Module | Responsibility |
| :--- | :--- |
| `src/main.rs` | winit event loop, tabs/panes state, shortcut execution, mouse & IME handling, clipboard |
| `src/keymap.rs` | Pure shortcut resolution and key-to-byte encoding for macOS and Linux (unit-tested) |
| `src/mouse.rs` | Mouse reporting encoding (X10 / SGR) for applications that track the mouse |
| `src/pty.rs` | `openpty` + `fork`/`execve`, PTY reader thread (parses into the grid) |
| `src/grid.rs` | 12-byte `Cell`, scrollback ring buffer, `vte::Perform` implementation |
| `src/renderer.rs` | wgpu pipeline, instance buffer, tab bar, history popup |
| `src/atlas.rs` | Glyph atlas, procedural sprites, font fallback chain, CoreText fallback |
| `src/shader.wgsl` | Single instanced draw call for all cells |
| `src/split.rs` | Split-pane layout tree |
| `src/history.rs` | Shell history search (fzf-style matching) |
| `src/config.rs` | Ghostty-compatible config parsing and themes |
| `src/macos.rs` | Objective-C FFI (`msg_send!`) for window/vibrancy/dock integration; no-op stubs elsewhere |

## Core Invariants

- **Memory / CPU budget:** target < 15 MB RAM and 0% idle CPU. Never introduce a continuous render loop; keep `ControlFlow::Wait` and redraw only on PTY output, input, or cursor blink.
- **`Cell` stays exactly 12 bytes** (enforced by a compile-time assert). Pack new attributes into existing bits instead of growing the struct.
- **No allocation on hot paths:** PTY parsing, scrolling, and per-frame rendering must not allocate. The grid may only allocate on resize or ring-buffer growth.
- **After `fork()` only async-signal-safe calls** — prepare every `CString`/`Vec` before forking.
- **Locking:** hold the grid `Mutex` for as short as possible; never perform blocking I/O (PTY writes, subprocesses, file I/O) while holding it.
- **PTY replies** (DSR, DA, DECRQM, OSC queries) must be pushed to `Grid::response`, never written directly from `Perform` handlers.

## Security Rules

- Sanitize pasted text before sending it to the PTY: strip `ESC` (at least `\x1b[201~`) so clipboard content cannot break out of bracketed paste.
- Never rewrite the user's shell history files in place: operate on raw bytes (zsh metafied encoding), write to a temp file and `rename` atomically.
- Pass user/terminal-derived strings to `open` / `xdg-open` as a single argument — never through a shell.

## Workflow

- Before finishing any change, run:
  - `cargo check` (macOS)
  - `cargo check --target x86_64-unknown-linux-gnu` (Linux parity)
  - `cargo test`
  - `cargo clippy --all-targets` — do not add new warnings.
- Add unit tests next to the code (`#[cfg(test)] mod tests`) for parser, grid, and keybinding behavior changes; use the existing `feed()` helper in `grid.rs` tests.
- PTY tests call `openpty`, which the Claude Code sandbox blocks (EPERM); run `cargo test` outside the sandbox.
- Every new or changed shortcut must also update the Standard Shortcut Matrix above and the README.
- Commit messages follow Conventional Commits (`feat(scope): ...`, `fix(scope): ...`, `docs(scope): ...`).
