# Laplace Terminal ⚡

[![Build Status](https://img.shields.io/badge/build-passing-brightgreen.svg)]()
[![Platform](https://img.shields.io/badge/platform-macOS%20%7C%20Linux-blue.svg)]()
[![GPU Backend](https://img.shields.io/badge/GPU-Metal%20%7C%20Vulkan-orange.svg)]()
[![Idle CPU](https://img.shields.io/badge/idle%20CPU-0.0%25-green.svg)]()
[![Idle RAM](https://img.shields.io/badge/idle%20RAM-~68MB-success.svg)]()
[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](LICENSE)

**Laplace** is an ultra-fast, lightweight, GPU-accelerated terminal emulator written in Rust. It delivers sub-millisecond input response, silky-smooth 120Hz ProMotion rendering, and an exceptionally small memory footprint without sacrificing modern developer ergonomics.

---

## ⚡ Highlights

- **Native GPU Rendering:** Powered by `wgpu` targeting native **Metal** on macOS and **Vulkan** on Linux. Renders the entire terminal grid in a **single instanced draw call** using a custom WGSL shader.
- **Zero-Allocation Ring Buffer:** Character cells are packed into just **12 bytes** (`codepoint`, packed `fg`, packed `bg`). Scrolling and heavy logging stream directly through an in-place circular ring buffer without runtime heap thrashing.
- **True 0.0% Idle CPU:** Pure event-driven execution using `ControlFlow::Wait`. Laplace sleeps completely when inactive, never running unnecessary polling loops.
- **Ghostty-Compatible Configuration:** Reads `~/.laplace/config` and automatically falls back to `~/.config/ghostty/config`. Supports live config reloading (`Cmd+,` / `Ctrl+,` and `Cmd+Shift+,` / `Ctrl+Shift+,`) and auto-reloads on window focus.
- **Native macOS Lifecycle:** Closing the last tab or window keeps Laplace running in the background (Dock) at 0% CPU. Clicking the Dock icon or running `open -a Laplace` immediately restores a fresh tab. Press `Cmd+Q` (or `Ctrl+Shift+Q` on Linux) to quit.
- **Tabs with CWD Inheritance:** Seamless multi-tab experience (`Cmd+T`/`Ctrl+Shift+T`, `Cmd+W`/`Ctrl+Shift+W`, tab cycling, 1..9 direct jumps). New tabs automatically open in the current working directory of the active tab.
- **Ghostty-Style Splits / Panes:** Split any tab horizontally or vertically (`Cmd+D` / `Cmd+Shift+D` on macOS, `Ctrl+Shift+D` / `Ctrl+Shift+E` on Linux). Seamlessly navigate between panes (`Cmd+Option+Arrows` / `Cmd+[` / `Cmd+]`), resize splits (`Cmd+Ctrl+Arrows`), toggle zoom (`Cmd+Shift+Enter`), or equalize ratios (`Cmd+Option+=`). Renders sleek hairline divider borders with zero overhead in the same single GPU instanced draw call.
- **Smart Hyperlink & Path Detection:** `Cmd+Click` (macOS) / `Ctrl+Click` (Linux) on URLs or local filesystem paths (`~/...`, `./...`, or absolute paths) to open them directly in your default browser or editor. Explicit **OSC 8 hyperlinks** (`ls --hyperlink`, `gcc`, `delta`, `rg --hyperlink-format`) take priority: hovering shows the real target, `file://` links open the local file, and only `http(s)`, `ftp(s)` and `mailto` URIs are opened.
- **Built-in CLI History Search:** Press `Ctrl+R` (or `Cmd+R` on macOS, `Super+R` on Linux) to open a zero-latency fuzzy history search popup with character-level match highlighting. Press `Enter` to run, `Tab` to insert into the prompt for editing, and `Esc` to close. Preserves `Ctrl+R` redo inside vim/nano!
- **Find in Terminal (Regex):** Press `Cmd+F` (macOS) or `Ctrl+Shift+F` / `Super+F` (Linux) to open a search box in the top-right corner. Queries are regular expressions by default (`Ctrl+R` toggles literal mode) with smart case. All matches in the scrollback are highlighted; `Enter` / `↑` / `Cmd+G` jump to the previous (older) match, `Shift+Enter` / `↓` / `Cmd+Shift+G` to the next, and `Esc` closes.
- **Click-to-Position Cursor:** Click anywhere on the active command line to immediately move the shell cursor to that character, with automatic alignment for wide Unicode glyphs, CJK characters, and multi-byte prompt symbols (`❯`, `🦀`, emojis).
- **Ergonomic Line & Word Editing:** Out-of-the-box support for word jumping (`Option+Left/Right` on macOS without interfering with Mission Control Spaces, `Ctrl+Left/Right` on Linux), prompt-aware deletion before cursor (`Ctrl+U`), delete to line end (`Ctrl+K`), and instant scrollback clear (`Cmd+K` / `Ctrl+Shift+K`).
- **Kitty Keyboard Protocol:** Full support for all five progressive enhancements (`CSI > flags u`): disambiguated `Esc`/`Ctrl`/`Alt` keys, key repeat & release events, alternate (shifted and US-layout) keys, every key as an escape code, and associated text — with separate flag stacks for the main and alternate screens. Neovim, Helix, fish 4 and Kakoune can tell `Ctrl+I` from `Tab`, `Ctrl+Shift+<key>` from `Ctrl+<key>`, and see key releases.
- **Full-Screen App & Unicode Support:** Mouse reporting (X10/SGR, click, drag and motion tracking) for vim, tmux and htop; focus events; alternate scroll in `less`/`man`; IME input (Vietnamese Telex/VNI, CJK); combining-mark composition; double-width CJK/emoji glyphs; bold/italic text; strikethrough (SGR 9) and styled underlines (SGR `4:1`..`4:5` single/double/curly/dotted/dashed, `21` double) with their own color (SGR 58/59), as used by Neovim diagnostics; reflow of wrapped lines on resize; synchronized output; OSC 7 (cwd), OSC 8 (hyperlinks), OSC 52 (set clipboard) and OSC 133 (prompt marks).
- **Aesthetics & Window Effects:** Native macOS vibrancy blur, transparent backgrounds, 24-bit TrueColor support, and built-in palettes.

---

## 📊 Performance & Memory Footprint

Tested on macOS Sonoma (Apple Silicon M-series, empty idle shell):

| Terminal Emulator | Idle RAM (RSS) | Idle CPU | Startup Time | GPU Backend |
| :--- | :---: | :---: | :---: | :---: |
| **Laplace** | **~68 – 86 MB** | **0.0%** | **< 30 ms** | Metal (Direct) |
| Ghostty | ~105 – 120 MB | 0.0% | ~50 ms | Metal |
| Alacritty | ~110 – 130 MB | 0.0% | ~60 ms | OpenGL / Metal |
| WezTerm | ~170 – 210 MB | 0.0% – 0.5% | ~120 ms | OpenGL / Vulkan |

---

## 🏗 Architecture

Laplace decouples I/O throughput from rendering to guarantee smooth UI interaction even during heavy terminal output:

```text
               ┌──────────────────────────────┐
               │    Shell Process (zsh/bash)  │
               └──────────────┬───────────────┘
                              │ OS PTY Pipe
                              ▼
┌─────────────────────────────────────────────────────────────┐
│ Thread 1: PTY Reader Thread                                  │
│  • Reads raw stream via 64 KB static buffer                 │
│  • Parses ANSI / VT sequences via `vte` state machine       │
│  • Writes directly into locked contiguous RingBuffer Grid   │
│  • Dispatches throttled WakeUp event to EventLoop           │
└─────────────────────────────┬───────────────────────────────┘
                              │ Atomic Dirty Flag + WakeUp
                              ▼
┌─────────────────────────────────────────────────────────────┐
│ Thread 2: Main Event & Render Thread (winit + wgpu)          │
│  • Event-driven: Sleeps at 0.0% CPU when idle               │
│  • Synchronized to display V-Sync (120Hz on ProMotion)      │
│  • Maps visible cells into single GPU Instance Buffer       │
│  • 1 Instanced Draw Call via custom WGSL shader             │
└─────────────────────────────────────────────────────────────┘
```

---

## ⌨️ Default Shortcuts

Laplace provides complete shortcut parity across macOS and Linux, adhering strictly to terminal conventions on each platform:

| Action | macOS (`Cmd`) | Linux (`Ctrl` / `Super`) | Notes |
| :--- | :--- | :--- | :--- |
| **New tab** *(inherits active CWD)* | <kbd>Cmd</kbd> + <kbd>T</kbd> / <kbd>Cmd</kbd> + <kbd>N</kbd> | <kbd>Ctrl</kbd> + <kbd>Shift</kbd> + <kbd>T</kbd> / <kbd>N</kbd> *(or <kbd>Super</kbd> + <kbd>T/N</kbd>)* | Preserves bare `Ctrl+T` (transpose/fzf) and `Ctrl+N` (history) |
| **Close active pane / tab** | <kbd>Cmd</kbd> + <kbd>W</kbd> | <kbd>Ctrl</kbd> + <kbd>Shift</kbd> + <kbd>W</kbd> *(or <kbd>Super</kbd> + <kbd>W</kbd>)* | Closes active split pane; closes tab if last pane remaining |
| **Split pane right (Vertical)** | <kbd>Cmd</kbd> + <kbd>D</kbd> | <kbd>Ctrl</kbd> + <kbd>Shift</kbd> + <kbd>D</kbd> *(or <kbd>Super</kbd> + <kbd>D</kbd>)* | Splits active pane vertically with hairline divider |
| **Split pane down (Horizontal)** | <kbd>Cmd</kbd> + <kbd>Shift</kbd> + <kbd>D</kbd> | <kbd>Ctrl</kbd> + <kbd>Shift</kbd> + <kbd>E</kbd> / <kbd>Super</kbd> + <kbd>Shift</kbd> + <kbd>D</kbd> | Splits active pane horizontally |
| **Toggle split zoom** | <kbd>Cmd</kbd> + <kbd>Shift</kbd> + <kbd>Enter</kbd> | <kbd>Ctrl</kbd> + <kbd>Shift</kbd> + <kbd>Enter</kbd> *(or <kbd>Super</kbd> + <kbd>Shift</kbd> + <kbd>Enter</kbd>)* | Expands active split to fill window; press again to restore |
| **Equalize split sizes** | <kbd>Cmd</kbd> + <kbd>Option</kbd> + <kbd>=</kbd> | <kbd>Ctrl</kbd> + <kbd>Alt</kbd> + <kbd>=</kbd> *(or <kbd>Super</kbd> + <kbd>Alt</kbd> + <kbd>=</kbd>)* | Resets all split dividers in current tab to equal ratios |
| **Navigate splits / tabs** | <kbd>Cmd</kbd> + <kbd>←</kbd> / <kbd>→</kbd> / <kbd>↑</kbd> / <kbd>↓</kbd> *(or + <kbd>Option</kbd>)* | <kbd>Ctrl</kbd> + <kbd>Alt</kbd> + Arrows *(or <kbd>Super</kbd> + Arrows)* | Spatial focus navigation (falls back to cycling tabs at edge) |
| **Cycle splits (Prev / Next)** | <kbd>Cmd</kbd> + <kbd>[</kbd> / <kbd>]</kbd> | <kbd>Ctrl</kbd> + <kbd>Alt</kbd> + <kbd>[</kbd> / <kbd>]</kbd> *(or <kbd>Super</kbd> + <kbd>[</kbd> / <kbd>]</kbd>)* | Cycles focus across split panes in active tab |
| **Resize active split** | <kbd>Cmd</kbd> + <kbd>Ctrl</kbd> + Arrows | <kbd>Super</kbd> + <kbd>Ctrl</kbd> + Arrows / <kbd>Ctrl</kbd> + <kbd>Alt</kbd> + <kbd>Shift</kbd> + Arrows | Dynamically expands or shrinks active split pane |
| **Quit Laplace** | <kbd>Cmd</kbd> + <kbd>Q</kbd> | <kbd>Ctrl</kbd> + <kbd>Shift</kbd> + <kbd>Q</kbd> *(or <kbd>Super</kbd> + <kbd>Q</kbd>)* | Preserves bare `Ctrl+Q` (XON flow control) |
| **Minimize window** | <kbd>Cmd</kbd> + <kbd>M</kbd> | <kbd>Ctrl</kbd> + <kbd>Shift</kbd> + <kbd>M</kbd> *(or <kbd>Super</kbd> + <kbd>M</kbd>)* | Preserves bare `Ctrl+M` (Enter / Carriage return) |
| **Cycle tabs (Prev / Next)** | <kbd>Cmd</kbd> + <kbd>Shift</kbd> + <kbd>[</kbd> / <kbd>]</kbd> | <kbd>Ctrl</kbd> + <kbd>Shift</kbd> + <kbd>[</kbd> / <kbd>]</kbd> | Circular tab switching |
| **Cycle tabs (Next / Prev)** | <kbd>Ctrl</kbd> + <kbd>Tab</kbd> / <kbd>Ctrl</kbd> + <kbd>Shift</kbd> + <kbd>Tab</kbd> | <kbd>Ctrl</kbd> + <kbd>Tab</kbd> / <kbd>Ctrl</kbd> + <kbd>Shift</kbd> + <kbd>Tab</kbd> | Cross-platform standard |
| **Navigate tabs (Left / Right)** | <kbd>Cmd</kbd> + <kbd>Option</kbd> + <kbd>←</kbd> / <kbd>→</kbd> | <kbd>Ctrl</kbd> + <kbd>Alt</kbd> + <kbd>←</kbd> / <kbd>→</kbd> *(or <kbd>Super</kbd> + <kbd>Alt</kbd>)* | Direct spatial switching |
| **Jump to tab 1..9** | <kbd>Cmd</kbd> + <kbd>1</kbd> .. <kbd>9</kbd> | <kbd>Ctrl</kbd> + <kbd>1</kbd> .. <kbd>9</kbd>, <kbd>Alt</kbd> + <kbd>1</kbd> .. <kbd>9</kbd> | Matches `^1`..`^9` badges; `9` jumps to last tab |
| **Copy selection** | <kbd>Cmd</kbd> + <kbd>C</kbd> | <kbd>Ctrl</kbd> + <kbd>Shift</kbd> + <kbd>C</kbd> *(or <kbd>Super</kbd> + <kbd>C</kbd>)* | Preserves `Ctrl+C` (SIGINT) |
| **Paste clipboard** | <kbd>Cmd</kbd> + <kbd>V</kbd> | <kbd>Ctrl</kbd> + <kbd>Shift</kbd> + <kbd>V</kbd> *(or <kbd>Super</kbd> + <kbd>V</kbd>)* | Bracketed paste aware; preserves `Ctrl+V` (literal escape) |
| **Zoom in (+1.0 pt)** | <kbd>Cmd</kbd> + <kbd>+</kbd> / <kbd>=</kbd> | <kbd>Ctrl</kbd> + <kbd>+</kbd> / <kbd>=</kbd> *(with/without Shift, or Super)* | Font size clamps between 6.0 – 72.0 pt |
| **Zoom out (-1.0 pt)** | <kbd>Cmd</kbd> + <kbd>-</kbd> | <kbd>Ctrl</kbd> + <kbd>-</kbd> / <kbd>_</kbd> *(with/without Shift, or Super)* | Font size clamps between 6.0 – 72.0 pt |
| **Reset zoom** | <kbd>Cmd</kbd> + <kbd>0</kbd> | <kbd>Ctrl</kbd> + <kbd>0</kbd> *(with/without Shift, or Super)* | Resets to configured default font size |
| **Open config editor** | <kbd>Cmd</kbd> + <kbd>,</kbd> | <kbd>Ctrl</kbd> + <kbd>,</kbd> *(or <kbd>Super</kbd> + <kbd>,</kbd>)* | Opens `~/.laplace/config` in `$EDITOR` |
| **Reload config file** | <kbd>Cmd</kbd> + <kbd>Shift</kbd> + <kbd>,</kbd> | <kbd>Ctrl</kbd> + <kbd>Shift</kbd> + <kbd>,</kbd> | Live reload without losing shell session |
| **Open URL or path** | <kbd>Cmd</kbd> + <kbd>Click</kbd> | <kbd>Ctrl</kbd> + <kbd>Click</kbd> *(or <kbd>Super</kbd> + <kbd>Click</kbd>)* | Opens in default browser / editor via `open` or `xdg-open` |
| **Hover URL preview** | <kbd>Cmd</kbd> + <kbd>Mouse Move</kbd> | <kbd>Ctrl</kbd> + <kbd>Mouse Move</kbd> *(or <kbd>Super</kbd>)* | Underlines URL and displays preview pill |
| **Select all** | <kbd>Cmd</kbd> + <kbd>A</kbd> | <kbd>Ctrl</kbd> + <kbd>Shift</kbd> + <kbd>A</kbd> *(or <kbd>Super</kbd> + <kbd>A</kbd>)* | Selects all scrollback and screen text; copy with <kbd>Cmd</kbd>/<kbd>Ctrl</kbd>+<kbd>Shift</kbd>+<kbd>C</kbd>. Preserves bare `Ctrl+A` (beginning of line) |
| **Extend selection** | <kbd>Shift</kbd> + <kbd>Click</kbd> / <kbd>Drag</kbd> | <kbd>Shift</kbd> + <kbd>Click</kbd> / <kbd>Drag</kbd> | Moves the nearest end of the current selection to the mouse and copies it |
| **Select text in mouse apps** | <kbd>Shift</kbd> + <kbd>Drag</kbd> | <kbd>Shift</kbd> + <kbd>Drag</kbd> | Bypasses mouse reporting in vim/tmux/htop to select and copy locally |
| **Click to position cursor** | <kbd>Mouse Left Click</kbd> | <kbd>Mouse Left Click</kbd> | Repositions cursor to clicked character on active command line (Unicode & wide-char aligned) |
| **Jump word backward / forward** | <kbd>Option</kbd> + <kbd>←</kbd> / <kbd>→</kbd> | <kbd>Ctrl</kbd> + <kbd>←</kbd> / <kbd>→</kbd> *(or <kbd>Alt</kbd> + Arrows)* | Jumps cursor backward/forward by word (`\x1bb` / `\x1bf`). On macOS, strictly `Option+Arrows` to avoid Mission Control Spaces collision |
| **Delete before cursor** | <kbd>Ctrl</kbd> + <kbd>U</kbd> | <kbd>Ctrl</kbd> + <kbd>U</kbd> | Prompt-aware deletion from prompt to cursor (preserves prompt & text after cursor; preserves Vim scroll in alt screen) |
| **Delete after cursor** | <kbd>Ctrl</kbd> + <kbd>K</kbd> | <kbd>Ctrl</kbd> + <kbd>K</kbd> | Deletes all text from cursor to end of command line (`kill-line`) |
| **Clear screen & scrollback** | <kbd>Cmd</kbd> + <kbd>K</kbd> | <kbd>Ctrl</kbd> + <kbd>Shift</kbd> + <kbd>K</kbd> | Clears scrollback history buffer and redraws prompt at top (matches Terminal.app / iTerm2 / Ghostty) |
| **CLI History Search** | <kbd>Ctrl</kbd> + <kbd>R</kbd> *(or <kbd>Cmd</kbd> + <kbd>R</kbd>)* | <kbd>Ctrl</kbd> + <kbd>R</kbd> *(or <kbd>Super</kbd> + <kbd>R</kbd> / <kbd>Ctrl</kbd> + <kbd>Shift</kbd> + <kbd>R</kbd>)* | Built-in fuzzy history search; `Enter`: execute, `Tab`: insert, `Esc`: close; preserves vim redo |
| **Find in Terminal** | <kbd>Cmd</kbd> + <kbd>F</kbd> | <kbd>Ctrl</kbd> + <kbd>Shift</kbd> + <kbd>F</kbd> *(or <kbd>Super</kbd> + <kbd>F</kbd>)* | Regex search box (top-right); `Enter`/`↑`: older match, `Shift+Enter`/`↓`: newer, `Ctrl+R`: toggle regex/literal, `Esc`: close |
| **Scroll history locally** | <kbd>Shift</kbd> + <kbd>PageUp</kbd> / <kbd>PageDown</kbd> | <kbd>Shift</kbd> + <kbd>PageUp</kbd> / <kbd>PageDown</kbd> | Scrolls active tab's scrollback buffer |

---

## ⚙️ Configuration

Laplace looks for its configuration file in the following order:
1. `~/.laplace/config`
2. `~/.config/ghostty/config` *(automatic Ghostty drop-in support)*

Press <kbd>Cmd</kbd> + <kbd>,</kbd> inside Laplace to create/open your configuration file.

### Example `~/.laplace/config`

```ini
# Font Configuration
font-family = "SF Mono"
font-family = "MesloLGS NF"
font-family = "Symbols Nerd Font Mono"
font-size = 14.0

# Window Appearance
background-opacity = 0.92
background-blur = 20
window-colorspace = "display-p3"
# window-width = 120
# window-height = 36

# Colors & Palette
background = #121316
foreground = #e6e6e6
# theme = "catppuccin-mocha"

# Cursor
cursor-style = "block"          # "block", "bar", "underline", "block_hollow"
cursor-style-blink = true

# Mouse & Scrolling
mouse-hide-while-typing = true
mouse-scroll-precision = 1.0
mouse-scroll-discrete = 3.0

# Shell Integration
shell-integration = "detect"     # "detect", "zsh", "bash", "none"
shell-integration-features = "cursor,title"

# Auto Update
auto-update = "check"           # "check" (default), "download", or "off"
auto-update-channel = "stable"  # "stable" (default) or "tip"
```

## 🔄 Auto-Update & CLI Commands

Laplace includes a built-in, zero-dependency self-updater that checks for releases on GitHub, validates SHA-256 checksums from `SHA256SUMS.txt`, and safely updates `Laplace.app` (with ad-hoc codesigning) or standalone binaries in-place.

- **macOS Menu Bar**: Choose **Laplace ▸ Check for Updates…** to check and install updates interactively with native Cocoa alerts.
- **CLI Commands**:
  - `laplace +check-update` (or `--check-update`): Check if a new version is available without installing.
  - `laplace +update` (or `--update`): Download and install the newest release directly in your terminal (use `--force` to reinstall current version).

---

## 📦 Installation

### macOS (Pre-built DMG / App)

1. Download **`Laplace.dmg`** (or `Laplace-macos.zip`) from the [Latest Release](https://github.com/Matrix-I/laplace-terminal/releases/latest).
2. Open `Laplace.dmg` and drag `Laplace.app` into your `/Applications` folder.
3. **macOS Gatekeeper Notice**: Because Laplace is not yet notarized with an Apple Developer Certificate, macOS will mark downloaded apps with a quarantine attribute. Run the following command in Terminal to allow it to open:

```bash
xattr -dr com.apple.quarantine /Applications/Laplace.app
```

Now you can launch Laplace from Spotlight, Launchpad, or the Applications folder!

### Linux (Pre-built Binary)

1. Download **`laplace-linux`** (or `laplace-linux-x86_64.tar.gz`) from the [Latest Release](https://github.com/Matrix-I/laplace-terminal/releases/latest).
2. Make it executable and place it in your `$PATH`:

```bash
chmod +x laplace-linux
sudo mv laplace-linux /usr/local/bin/laplace
```

---

## 🚀 Building & Packaging

### Prerequisites

- [Rust & Cargo](https://rustup.rs/) (edition 2021, Rust 1.80+)
- **macOS:** Xcode Command Line Tools (`xcode-select --install`)
- **Linux:** Vulkan drivers, `libxkbcommon`, `fontconfig`

### Build from Source

```bash
# Clone the repository
git clone https://github.com/Matrix-I/laplace-terminal.git
cd laplace-terminal

# Run tests
cargo test

# Build optimized release binary
cargo build --release
```

### macOS Application Bundle & DMG

Run the automated bundling script:

```bash
./bundle_macos.sh
```

This will:
1. Build Laplace with aggressive LTO and size stripping.
2. Generate `Laplace.app` signed with ad-hoc certificates.
3. Install `Laplace.app` to `~/Applications/Laplace.app`.
4. Create an installable drag-and-drop disk image: `dist/Laplace.dmg`.


---

## 📄 License

This project is licensed under the [MIT License](LICENSE).
