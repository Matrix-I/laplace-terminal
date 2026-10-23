To hit the strict goal of **under 15MB RAM** and **0% idle CPU** on both macOS (Metal) and Linux (Vulkan), you must avoid heavy GUI toolkits, multi-layered text shapers, and dynamic heap allocations during runtime.

---

## 1. Core Architectural Principles

* **Zero-Allocation Runtime:** Allocate the entire terminal grid and scrollback buffer once at startup as a single contiguous flat array (`Vec<Cell>`). Never allocate or free memory when scrolling or printing text.
* **Compact Cell Memory Layout:** Pack each character cell into exactly **8 or 12 bytes** (instead of 32+ bytes in standard terminals) to fit the active grid inside the CPU's L1/L2 cache.
* **Event-Driven Sleep Loop:** Never run a continuous 60/120 FPS game loop. Use `ControlFlow::Wait` so the main thread completely sleeps until the OS wakes it via keyboard input or a PTY data signal.
* **Single-Draw-Call Instanced Rendering:** Pack all visible cells into a single GPU storage/instance buffer and draw the entire screen in **1 draw call** using a custom WGSL shader and a pre-baked monochrome/RGBA glyph texture atlas.

---

## 2. System Architecture & Memory Design

```text
[Shell Process (zsh/bash)]
       │
       ▼ (OS Pipe / File Descriptor)
┌──────────────────────────────────────────────────────────┐
│ Thread 1: PTY I/O Reader (Blocking read on 64KB stack)   │
│  • Reads raw bytes into a fixed [u8; 65536] buffer       │
│  • Parses ANSI via `vte` directly into locked RingBuffer │
│  • Sends WakeUp signal via `EventLoopProxy`              │
└──────────────────────┬───────────────────────────────────┘
                       │ (Atomic Dirty Flag + WakeUp)
                       ▼
┌──────────────────────────────────────────────────────────┐
│ Thread 2: Main Window & GPU Thread (`winit` + `wgpu`)    │
│  • Sleeps at 0% CPU until woken by OS or Thread 1        │
│  • Maps dirty RingBuffer rows into GPU Instance Buffer   │
│  • Executes 1 Instanced Draw Call (Metal / Vulkan)       │
└──────────────────────────────────────────────────────────┘

```

### Ultra-Compact Cell Struct (12 Bytes per Cell)

Standard terminals waste RAM by storing strings, vectors, or full RGBA floats per cell. Pack it tightly:

```rust
#[repr(C)]
#[derive(Copy, Clone)]
pub struct Cell {
    pub codepoint: u32, // 4 bytes: Unicode char
    pub fg_color: u32,  // 4 bytes: Packed RGBA (e.g., 0xRRGGBBAA)
    pub bg_color: u32,  // 4 bytes: Packed RGBA + style flags in alpha bits
}

```

* **RAM Math:** A terminal with **120 columns × 5,000 scrollback rows** = 600,000 cells.
* $600,000 \times 12\text{ bytes} \approx \mathbf{7.2\text{ MB}}$ of RAM for the entire terminal state and history.

---

## 3. Minimal Crate Selection (`Cargo.toml`)

Skip high-level text engines like `glyphon` or `cosmic-text` (they allocate heavily for complex shaping). Use a raw rasterizer (`fontdue`) to build your own fixed-size texture atlas:

* **`winit`:** Pure OS window creation and input handling.
* **`wgpu`:** Direct Metal (macOS) and Vulkan (Linux) access.
* **`fontdue`:** Ultra-fast, zero-dependency font rasterizer (uses a fraction of the RAM of FreeType/HarfBuzz).
* **`vte`:** Zero-allocation state machine parser from Alacritty.
* **`rustix` / ` nix` (or `portable-pty`):** Low-level POSIX `openpty` and `fork` calls.
* **`parking_lot`:** Fast, tiny Mutex for syncing the Ring Buffer between the PTY thread and Render thread.

---

## 4. Step-by-Step Implementation Plan

1. **Phase 1: Zero-Allocation Ring Buffer & Compact Grid:** Target: Under 8MB heap allocation, O(1) line scrolling.
* Create `Grid` holding a flat `Vec<Cell>` of size `cols * max_rows` (e.g., $120 \times 5000$).
* Implement ring indexing: store a `head_row: usize` pointer. When a newline occurs at the bottom of the screen, simply increment `head_row = (head_row + 1) % max_rows` and clear the new row in-place.
* Maintain a cursor struct (`row`, `col`, current `fg`, current `bg`) and a boolean `dirty` flag.


2. **Phase 2: Low-Level PTY & VTE State Machine:** Target: High-throughput stream parsing without heap churn.
* Spawn the user's shell (`/bin/zsh` or `/bin/bash`) tied to a pseudo-terminal master/slave pair.
* Spawn **Thread 1 (PTY Reader)** with a static stack buffer `let mut buf = [0u8; 65536];`.
* Implement the `vte::Perform` trait directly on your `Grid` struct:
* `print(c)`: Write `c` to `grid[cursor]`, advance cursor.
* `execute(byte)`: Handle `\n` (linefeed), `\r` (carriage return), `\x08` (backspace).
* `csi_dispatch(...)`: Handle cursor jumps (`\x1b[H`), screen clears (`\x1b[2J`), and SGR colors (`\x1b[31m`).




3. **Phase 3: Custom GPU Instanced Renderer & Glyph Atlas:** Target: 120Hz ProMotion on Mac, minimal VRAM footprint.
* Initialize `winit` and `wgpu` with `PowerPreference::LowPower` (forces integrated GPU on dual-GPU laptops to save battery and RAM).
* Configure `SurfaceConfiguration` with `PresentMode::Fifo` (strict V-Sync, locks cleanly to 120Hz on macOS ProMotion).
* Create a **512×512 R8Unorm GPU Texture** (takes only **256 KB of VRAM**).
* Use `fontdue` to rasterize ASCII (32–126) at startup into the texture atlas; rasterize Unicode characters on-demand only when encountered.
* Write a ~40-line WGSL shader that takes an array of visible cells (`InstanceBuffer`) and draws every character on screen in a **single `render_pass.draw(0..4, 0..visible_cells)` call**.


4. **Phase 4: Event-Driven Synchronization & Keyboard Input:** Target: 0.0% CPU usage when terminal is idle.
* Set the `winit` event loop to `ControlFlow::Wait`.
* Pass a `winit::event_loop::EventLoopProxy<CustomEvent::WakeUp>` to the PTY Reader thread.
* When the PTY Reader finishes parsing a chunk of bytes, call `proxy.send_event(WakeUp)` to trigger `window.request_redraw()`.
* Coalesce frames: if a command dumps 100MB of logs (`cat huge.log`), the PTY thread parses continuously in memory while the UI only redraws once every V-Sync interval (8.3ms at 120Hz).
* Translate `winit::event::WindowEvent::KeyboardInput` into raw ANSI byte slices and write them directly to the PTY master file descriptor.


5. **Phase 5: Binary & Memory Stripping:** Target: Under 2MB binary size, minimal resident memory.
* Configure `Cargo.toml` release profile to strip symbols and optimize for size/speed:

```toml
[profile.release]
opt-level = 3
lto = "fat"
codegen-units = 1
panic = "abort"
strip = true

```

* Limit `wgpu` backend features in `Cargo.toml` to only `metal` on macOS and `vulkan` on Linux (disables DirectX, OpenGL, and WebGPU shader translators, cutting binary size and RAM in half).


---

## 5. Recommended Project Directory Structure

```text
my-term/
├── Cargo.toml
└── src/
    ├── main.rs       # Entry point, winit EventLoop (ControlFlow::Wait)
    ├── pty.rs        # OS PTY spawning, reader/writer threads
    ├── grid.rs       # Compact Cell (12B), RingBuffer, vte::Perform impl
    ├── atlas.rs      # fontdue rasterizer + 512x512 LRU texture packer
    ├── renderer.rs   # wgpu initialization, instance buffer, draw call
    └── shader.wgsl   # Instanced quad vertex + glyph sampling fragment shader

```
