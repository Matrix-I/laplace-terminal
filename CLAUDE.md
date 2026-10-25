# Laplace Terminal — Claude Code Rules

Shared agent guidelines (shortcut matrix, cross-platform modifier rules) live in AGENTS.md and apply here in full:

@AGENTS.md

## Project Layout

| Module | Responsibility |
| :--- | :--- |
| `src/main.rs` | winit event loop, tabs/panes state, key & mouse handling, clipboard |
| `src/pty.rs` | `openpty` + `fork`/`execve`, PTY reader thread (parses into the grid) |
| `src/grid.rs` | 12-byte `Cell`, scrollback ring buffer, `vte::Perform` implementation |
| `src/renderer.rs` | wgpu pipeline, instance buffer, tab bar, history popup |
| `src/atlas.rs` | Glyph atlas, procedural sprites, font fallback chain, CoreText fallback |
| `src/shader.wgsl` | Single instanced draw call for all cells |
| `src/split.rs` | Split-pane layout tree |
| `src/history.rs` | Shell history search (fzf-style matching) |
| `src/config.rs` | Ghostty-compatible config parsing and themes |
| `src/macos.rs` | Objective-C FFI for window/vibrancy/dock integration |

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
- Every new or changed shortcut must also update the matrix in AGENTS.md and the README.
- Commit messages follow Conventional Commits (`feat(scope): ...`, `fix(scope): ...`, `docs(scope): ...`).
