# Laplace Terminal — Development & Agent Guidelines

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

---

### 3. Implementation Checklist for New Shortcuts

When introducing any new key binding or mouse interaction in `src/main.rs`:
1. **Never hardcode macOS-only modifiers without an `#[cfg(not(target_os = "macos"))]` branch.**
2. **Check for PTY collisions**: Verify if the proposed `Ctrl + <Key>` combination emits an ASCII control code (`\x00`..`\x1f`) needed by shell or text editors (vim, nano). If it does, use `Ctrl + Shift` instead.
3. **Verify UI representation**: If the UI renders a badge or hint (e.g. `src/renderer.rs`), ensure macOS displays `⌘` and Linux displays `^` (for Ctrl) or `Alt+`, and ensure the actual key handler matches the visual symbol.
4. **Compile verification**:
   - Check macOS build: `cargo check`
   - Check Linux build: `cargo check --target x86_64-unknown-linux-gnu`
