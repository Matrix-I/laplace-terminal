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
- **Ghostty-Compatible Configuration:** Reads `~/.laplace/config` and automatically falls back to `~/.config/ghostty/config`. Supports live config reloading (`Cmd+,` / `Cmd+Shift+,`) and auto-reloads on window focus.
- **Native macOS Lifecycle:** Closing the last tab or window keeps Laplace running in the background (Dock) at 0% CPU. Clicking the Dock icon or running `open -a Laplace` immediately restores a fresh tab. Press `Cmd+Q` to quit.
- **Tabs with CWD Inheritance:** Seamless multi-tab experience (`Cmd+T`, `Cmd+N`, `Cmd+W`, `Cmd+Shift+[` / `]`). New tabs automatically open in the current working directory of the active tab.
- **Smart Hyperlink & Path Detection:** `Cmd+Click` on URLs or local filesystem paths (`~/...`, `./...`, or absolute paths) to open them directly in your default browser or editor.
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

### macOS

| Shortcut | Action |
| :--- | :--- |
| <kbd>Cmd</kbd> + <kbd>T</kbd> / <kbd>Cmd</kbd> + <kbd>N</kbd> | New tab (inherits active CWD) |
| <kbd>Cmd</kbd> + <kbd>W</kbd> | Close active tab (keeps app in Dock if last tab) |
| <kbd>Cmd</kbd> + <kbd>Q</kbd> | Quit Laplace |
| <kbd>Cmd</kbd> + <kbd>Shift</kbd> + <kbd>[</kbd> | Previous tab |
| <kbd>Cmd</kbd> + <kbd>Shift</kbd> + <kbd>]</kbd> | Next tab |
| <kbd>Cmd</kbd> + <kbd>1</kbd> .. <kbd>9</kbd> | Jump to tab 1 through 9 (9 = last tab) |
| <kbd>Cmd</kbd> + <kbd>C</kbd> | Copy selected text to clipboard |
| <kbd>Cmd</kbd> + <kbd>V</kbd> | Paste from clipboard (bracketed paste aware) |
| <kbd>Cmd</kbd> + <kbd>+</kbd> / <kbd>=</kbd> | Increase font size |
| <kbd>Cmd</kbd> + <kbd>-</kbd> | Decrease font size |
| <kbd>Cmd</kbd> + <kbd>0</kbd> | Reset font size |
| <kbd>Cmd</kbd> + <kbd>,</kbd> | Open configuration file in default editor |
| <kbd>Cmd</kbd> + <kbd>Shift</kbd> + <kbd>,</kbd> | Reload configuration file |
| <kbd>Cmd</kbd> + <kbd>Click</kbd> | Open clicked hyperlink or file path |
| <kbd>Shift</kbd> + <kbd>PageUp</kbd> / <kbd>PageDown</kbd> | Scroll scrollback buffer locally |

### Linux

| Shortcut | Action |
| :--- | :--- |
| <kbd>Ctrl</kbd> + <kbd>T</kbd> / <kbd>Ctrl</kbd> + <kbd>N</kbd> | New tab |
| <kbd>Ctrl</kbd> + <kbd>Shift</kbd> + <kbd>W</kbd> | Close tab |
| <kbd>Ctrl</kbd> + <kbd>Shift</kbd> + <kbd>Q</kbd> | Quit application |
| <kbd>Ctrl</kbd> + <kbd>Tab</kbd> / <kbd>Ctrl</kbd> + <kbd>Shift</kbd> + <kbd>Tab</kbd> | Switch next / previous tab |
| <kbd>Alt</kbd> + <kbd>1</kbd> .. <kbd>9</kbd> | Switch to tab 1..9 |
| <kbd>Ctrl</kbd> + <kbd>Shift</kbd> + <kbd>C</kbd> | Copy |
| <kbd>Ctrl</kbd> + <kbd>Shift</kbd> + <kbd>V</kbd> | Paste |
| <kbd>Ctrl</kbd> + <kbd>,</kbd> | Open config |

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
git clone https://github.com/your-username/laplace-terminal.git
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
