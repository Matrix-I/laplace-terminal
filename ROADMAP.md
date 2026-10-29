# Litty Roadmap — Ghostty Feature Gaps

Features Ghostty has that Litty does not yet, grouped by area. Roughly ordered
by priority inside each group. Tick an item when it lands on `main`.

## Done

- [x] OSC 8 hyperlinks (`4c822b4`)
- [x] Styled underlines SGR `4:1`..`4:5`, `21`, underline color SGR 58/59, strikethrough SGR 9/29 (`4c822b4`)
- [x] Kitty keyboard protocol, all five progressive enhancements (`6e948c1`)
- [x] Select all: `Cmd+A` (macOS), `Ctrl+Shift+A` / `Super+A` (Linux)
- [x] `Shift+Click` / `Shift+Drag` extends the current selection to the mouse

## Manual test

Each item lists what to run inside a Litty window and the expected result.
`bash scripts/manual-test.sh [group…]` walks through them and records results in
`scripts/manual-test-results.md`.

- [x] **OSC 4 / 10 / 11 / 12**
  - `printf '\e]11;#1e1e2e\a\e]10;#cdd6f4\a'` → background and padding turn dark purple, text light blue
  - `printf '\e]12;#f38ba8\a'` → cursor turns pink
  - `printf '\e]4;1;#00ff00\a\e[31mred-now-green\e[0m\n'` → text after `\e[31m` is green
  - `bash -c 'printf "\e]11;?\a"; IFS= read -rs -d $'"'"'\a'"'"' -t 1 r; echo "${r#*]}"'` → `11;rgb:1e1e/1e1e/2e2e`
  - `Cmd+D`, change the background in one split only → the other split keeps its colors, also after switching focus
  - `printf '\e]11;#ffffff\a'`, then `nvim` and `:set bg?` → `background=light`
  - `printf '\e]110\a\e]111\a\e]112\a\e]104\a'` → all colors back to the config
  - `printf '\e]11;midnight blue\a'` → background turns navy (X11 color names)

- [x] **modifyOtherKeys + Cmd/Super for Kitty apps**
  - `printf '\e[>4;2m'; cat -v` then `Ctrl+Enter`, `Ctrl+1`, `Ctrl+A` → `^[[27;5;13~`, `^[[27;5;49~`, `^[[27;5;97~`; `Ctrl+C` to exit `cat`, then `printf '\e[>4m'`
  - Same with `printf '\e[>4;1m'` → `Ctrl+A` prints `^A` (legacy kept), `Ctrl+1` prints `^[[27;5;49~`
  - In the Kitty key dumper (cycle to flags 1): `Cmd+J` → `\x1b[106;9u`; with flags 0 nothing is sent
  - Without any of these modes `Cmd+J` still sends nothing to the shell

- [x] **Notifications, progress, pointer shape, bell**
  - `sleep 3; printf '\e]9;Build finished\a'`, switch to another app within 3 s → a macOS notification "Litty / Build finished"
  - `sleep 3; printf '\e]777;notify;Tests;All passed\a'`, switch away → notification titled "Tests"
  - `printf '\e]9;4;1;45\a'` → window / tab title starts with `45% `; `printf '\e]9;4;3\a'` → `⋯ `; `printf '\e]9;4;2;80\a'` → `✖ 80% `; `printf '\e]9;4;0\a'` → prefix gone
  - `printf '\e]22;pointer\a'` then move the mouse over the terminal → hand cursor; `printf '\e]22;text\a'` → I-beam; `printf '\e]22;\a'` → arrow
  - Open a 2nd tab, run `sleep 2; printf '\a'` in it and switch to tab 1 → that tab's title gets `🔔 `, cleared when you switch back
  - `sleep 3; printf '\a'` and switch to another app → Litty bounces in the Dock

- [x] **XTGETTCAP, DECRQSS, pixel mouse, color scheme**
  - In `nvim`: `:checkhealth` shows no terminal warnings about true color / undercurl, and `:lua print(vim.o.termguicolors)` → `true`
  - `bash -c 'printf "\eP+q544E\e\\"; IFS= read -rs -d "\\" -t 1 r; echo "${r#?}" | cat -v'` → `P1+r544E=787465726D2D323536636F6C6F72^[` (TN = xterm-256color)
  - `bash -c 'printf "\e[1;31m\eP\$qm\e\\\e[0m"; IFS= read -rs -d "\\" -t 1 r; echo "${r#?}" | cat -v'` → contains `1$r0;1;38;2;…m`
  - `printf '\e[?1016h\e[?1003h'; cat -v`, move the mouse → `^[[<35;X;YM` with X/Y in pixels (hundreds, not cell numbers); `Ctrl+C`, then `printf '\e[?1016l\e[?1003l'`
  - `printf '\e[?2031h'; cat -v`, switch macOS Appearance between Light and Dark → `^[[?997;2n` / `^[[?997;1n`; `Ctrl+C`, then `printf '\e[?2031l'`

- [x] **Config: padding, light/dark theme, keybind, colors, title, copy-on-select** (edit `~/.litty/config`, then `Cmd+Shift+,` to reload)
  - `window-padding-x = 24` + `window-padding-y = 12` → wider margins; `window-padding-balance = true` → the grid is centered (equal leftover on both sides)
  - `theme = light:Horizon Bright,dark:Dracula` → Dracula colors; switch macOS Appearance to Light → Horizon Bright immediately, back to Dark → Dracula
  - `keybind = cmd+t=new_split:right` → `Cmd+T` splits instead of opening a tab; `keybind = cmd+d=unbind` → `Cmd+D` no longer splits; `keybind = ctrl+shift+h=text:hello\n` → types `hello` + Enter; `keybind = clear` → defaults back
  - `cursor-color = #ff8800` → orange cursor (OSC 12 still overrides); `selection-background = #335533` + `selection-foreground = #ffff00` → green selection with yellow text
  - `copy-on-select = false` → dragging a selection no longer changes the clipboard; `Cmd+C` still copies
  - `title = Work` → window title stays "Work" whatever runs

- [x] **Shell integration, prompt jump, output selection** (open a new Litty window so the shell starts with the integration)
  - `echo $LITTY_SHELL_INTEGRATION_DIR` → `~/.cache/litty/shell-integration/<version>`; your usual prompt, aliases and `.zshrc` all still load
  - `seq 50; ls; seq 30`, then `Cmd+Shift+↑` repeatedly → the view jumps prompt by prompt; `Cmd+Shift+↓` back down, past the last one returns to live
  - `ls -la` then `Cmd+Shift+O` → exactly the `ls -la` output is highlighted and in the clipboard (not the prompt, not the command line)
  - `cd /tmp` then `Cmd+T` → the new tab opens in `/tmp`
  - With `shell-integration-features = cursor,title`: bar cursor while typing at the prompt, block cursor inside `vim`; the title shows the directory, then the running command
  - `shell-integration = none` in the config + new window → `echo $LITTY_SHELL_INTEGRATION_DIR` is empty and nothing above works
  - bash: run `SHELL=/bin/bash ./target/release/litty` from the repo → your `.bash_profile` loads without errors, prompt jump works (output selection needs bash 4.4+, e.g. `SHELL=/opt/homebrew/bin/bash`)

- [x] **Drag & drop, scrollbar, secure input, bell-features**
  - Drag a file whose name has spaces from Finder into the window → `'/path/My File.txt' ` is typed at the prompt
  - `seq 500`, scroll up with the trackpad → a thin thumb appears at the right edge, moves with the scroll position, disappears at the bottom
  - `keybind = cmd+alt+s=toggle_secure_input` in the config, reload, press it → title starts with `🔒`; switch to another app and back → still on; press again → `🔒` gone
  - `bell-features = system` → `printf '\a'` plays the macOS alert sound; `bell-features = no-attention` → no Dock bounce when unfocused

- [x] **Multiple windows**
  - `cd /tmp` then `Cmd+N` → a second window opens with its shell in `/tmp`; `Cmd+T` still opens a tab in the current window
  - Type in each window → output stays in its own window; both keep updating when you run `while true; do date; sleep 1; done` in one and type in the other
  - `Cmd+W` / `exit` / the red close button in the 2nd window → only that window closes, the 1st keeps working
  - Close the last window → app stays in the Dock; click the Dock icon → a fresh window opens
  - Split, tabs, find, `Cmd+,` reload, theme switch → work in every window
  - `keybind = cmd+shift+n=new_window` works too

- [x] **scrollback-limit, unfocused splits, minimum-contrast** (config + `Cmd+Shift+,`)
  - `Cmd+D` → the split you are not typing in is faded (default 0.7); click it → the fade moves to the other one; one pane alone is never faded
  - `unfocused-split-opacity = 0.3` → much stronger fade; `unfocused-split-fill = #400000` → the inactive split fades toward dark red
  - `minimum-contrast = 21` → all text is pure black or white; `printf '\e[34;44mblue on blue\e[0m\n'` with `minimum-contrast = 3` → readable
  - `scrollback-limit = 96000` + new tab (80 cols ≈ 100 lines) → `seq 1000`, scroll up → history starts around 900

- [x] **Cell size, font-thicken, underline polish**
  - `adjust-cell-height = 20%` → more line spacing, text stays vertically centered in each row, box-drawing (`tree`, `htop` borders) still connects; `adjust-cell-width = 1` → slightly wider columns
  - `font-thicken = true` → visibly bolder text; remove → back to normal
  - `printf '\e[4:3mcurly\e[0m \e[4:2mdouble\e[0m\n'` → the wave is flatter than before; the two lines of the double underline are clearly apart

- [x] **Scrollbar drag, progress bar, bell border / audio**
  - `seq 2000`, scroll up a bit so the thumb shows, then drag it at the right edge → the view follows the mouse; drag to the top → `1` visible; drag to the bottom → back to live output
  - `printf '\e]9;4;1;40\a'` → a blue bar over the left 40% of the pane's top edge; `;2;70` → red 70%; `;4;50` → yellow; `;3` → full dim blue; `printf '\e]9;4;0\a'` → gone. With splits it shows on the active pane
  - `bell-features = border` → `sleep 2; printf '\a'` → amber outline around the whole window (also over the padding); any key press or refocusing clears it
  - `bell-features = audio` + `bell-audio-path = /System/Library/Sounds/Glass.aiff` → `printf '\a'` plays Glass; `bell-audio-volume = 0.1` → quieter
  - Everything else still renders as before: text, cursor styles, underline styles, split dividers, selection, padding with `background-opacity = 0.8`

- [x] **Shell integration additions** (nushell / elvish not installed: untested)
  - `seq 50; ls`, resize the window narrower and wider, then `Cmd+Shift+↑` → still jumps to each prompt; `Cmd+Shift+O` → still selects the `ls` output
  - `SHELL=/bin/bash ./target/release/litty`: `ls -la` then `Cmd+Shift+O` → the output is selected (bash 3.2 now has the C mark); Enter on an empty prompt, then `Cmd+Shift+O` → still the `ls -la` output
  - nushell 0.101+ (if installed): `SHELL=$(which nu) ./target/release/litty` → prompt jump and output selection work, `$env.XDG_DATA_DIRS` does not contain `litty`
  - elvish (if installed): add `use litty` to `~/.config/elvish/rc.elv`, start with `SHELL=$(which elvish)` → prompt jump works

- [x] **Bundled themes**
  - `./target/release/litty +list-themes | wc -l` → 861; `| grep -i gruvbox` lists several
  - `theme = Gruvbox Dark` (or any listed name, any case) + reload → colors change, including cursor and selection colors from the theme
  - `background = #000000` placed *before* `theme = Dracula` → background stays black, the rest is Dracula
  - `theme = catppuccin-mocha` / `tokyonight` (old aliases) still work

- [x] **confirm-close-surface**
  - Run `sleep 100`, press `Cmd+W` → "Close this terminal?" alert; Cancel keeps it, Close closes it
  - At an idle prompt `Cmd+W` closes without asking
  - `sleep 100` in one window, `Cmd+Q` → "Quit Litty?"; the red close button on that window → "Close this window?"
  - `confirm-close-surface = false` → never asks; `= always` → asks even at an idle prompt

- [x] **macOS menu bar**
  - The menu bar shows Litty / Shell / Edit / View / Window; every item works on the focused window (New Window, New Tab, Split Right/Down, Close, Copy, Paste, Select All, Find, Clear Screen, Bigger/Smaller/Actual Size, Settings…)
  - Litty ▸ Secure Keyboard Entry → check mark + 🔒 in the title; again → off
  - `Cmd+H` hides, `Ctrl+Cmd+F` toggles full screen, Window ▸ Minimize works and the Window menu lists open windows
  - `sleep 100` then `Cmd+Q` → "Quit Litty?" (before, `Cmd+Q` quit immediately)
  - Close every window, then Shell ▸ New Window from the menu bar → a window opens

- [x] **Kitty Caps Lock**
  - In the Kitty key dumper (`kitten show-key -m kitty`, or the earlier Python dumper) with flags 1, Caps Lock on, `Ctrl+A` → `CSI 97;69u`; Caps Lock off → `CSI 97;5u`; plain `a` with Caps Lock still types `A`

- [x] **Terminal inspector**
  - `Cmd+Option+I` → a panel in the bottom-right corner with Grid / Cursor / Screen / Mouse / Keyboard / Colors / Process / Title / Cell / Last key; again → gone, the text under it is back
  - Move the mouse over colored `ls --color` output → "Cell" shows the codepoint, fg/bg and attributes live
  - Type `a`, `Ctrl+C`, `↑` → "Last key" shows `a → "a"`, `Ctrl+c → "\x03"`, `ArrowUp → "\e[A"`
  - Open `vim` → Screen `alternate`, app-cursor `on`; `sleep 5` → Process `running`

- [x] **Native macOS tabs**
  - `macos-native-tabs = true` + reload, `cd /tmp`, `Cmd+T` → a native tab appears in the title bar, its shell is in `/tmp`
  - `Cmd+Shift+]` / `Cmd+Shift+[` and `Cmd+1` / `Cmd+2` switch between native tabs; `Cmd+W` closes one, the others stay
  - Without the option: `Cmd+T` uses Litty's own tab bar as before

- [x] **Quick Terminal (macOS)**
  - `keybind = global:ctrl+grave_accent=toggle_quick_terminal` + reload; from another app press `Ctrl+\`` → a borderless Litty panel drops in at the top of the screen (full width, ~40% height) and has focus
  - Press it again → hidden, focus goes back to the previous app; again → same shell, history intact
  - Click another app while it is shown → it hides (`quick-terminal-autohide = false` keeps it)
  - `exit` in it → closed; the hotkey opens a fresh one; normal windows are unaffected

- [x] **Custom shaders**
  - `custom-shader = <repo>/examples/shaders/invert.glsl` + reload → colors inverted, text upright (not mirrored), selection / cursor / tab bar still line up with the mouse
  - Add `custom-shader = <repo>/examples/shaders/scanlines.glsl` → inverted *and* moving scanlines while focused; switch to another app → animation stops (CPU back to ~0%); `custom-shader-animation = false` → static
  - A shader with a typo → Litty still starts, the error is printed on stderr (run from a terminal) and that shader is skipped
  - Remove the lines + reload → normal rendering

- [x] **OSC 4 recolors existing text**
  - `printf '\e[31mred\e[0m\n'`, then `printf '\e]4;1;#00ff00\a'` → the earlier "red" turns green; `printf '\e]104\a'` → red again
  - `ls --color` output follows a palette change too

- [x] **Ligatures and font-feature**
  - Install a ligature font (e.g. JetBrains Mono, Fira Code; a copy is at `/private/tmp/claude-501/-Users-linh-nguyen-Desktop-personal-litty-terminal/d22b6e14-bfe2-4859-821c-3e2b0b0c6257/scratchpad/fonts/JetBrainsMono-Regular.ttf`) and set `font-family = JetBrains Mono` (or run `LITTY_FONT=<that .ttf> ./target/release/litty` with no `font-family` in the config)
  - `echo '-> => != === <= >= :: |> www 0xFF'` → arrows and comparison operators render as joined ligatures, each still occupying its own cells; plain words unchanged
  - Select half a ligature with the mouse, move the cursor across it → no garbage, selection colors per cell
  - Bold (`printf '\e[1m-> !=\e[0m\n'`) and italic ligatures render
  - `font-feature = -calt` (+ `-liga`) + reload → ligatures off; `font-feature = ss01` → stylistic set where the font has one
  - Default font (SF Mono / Menlo, no ligatures) looks exactly as before

- [x] **Inline images (Kitty graphics, Sixel)**
  - With kitty's `kitten icat` (or `timg -pk`, `chafa -f kitty`): `kitten icat --transfer-mode=stream some.png` → the image shows inline at native size; `--place 20x10@0x0` sizes it to 20x10 cells
  - Scroll the output up and down → the image moves with the text; it is cut off cleanly at the pane edges and does not leak into a neighbouring split
  - `clear` / `Cmd+K` → images gone; `printf '\ec'` too
  - Sixel: `img2sixel some.png` (libsixel) or `chafa -f sixel some.png` → image appears, the prompt continues below it
  - Resize the window narrower/wider → images stay next to the same text
  - `vim` (alternate screen) and back → images on the main screen are still there
  - With a custom shader on, images are upright and in the right place

- [x] **Color emoji and grapheme clusters**
  - `echo '😀 👍🏽 👨‍👩‍👧 🇻🇳 ❤️ #️⃣ 🏳️‍🌈'` → all in color; the family, the flag, the rainbow flag and the thumbs-up with skin tone are each one glyph two cells wide; `❤️` and `#️⃣` are two cells wide too, text after them lines up
  - Select the line and copy (`Cmd+C`), paste into another app → the exact same sequences come back
  - `printf '\e[?2027l'; echo '👨‍👩‍👧'` → legacy widths: three separate faces (as in other terminals); `printf '\e[?2027h'` restores clustering; `grapheme-width-method = legacy` + new tab starts in legacy mode
  - `echo 'q̃ x̧̌ कि नमस्ते'` → marks sit on their base letters (before this change they were dropped)
  - Bold / selected / cursor-on emoji: still in color, the selection background shows around them
  - Linux: with `fonts-noto-color-emoji` installed the same emoji are in color

- [x] **Kitty graphics: under text, placeholders, animation, shared memory**
  - `kitten icat --z-index -1 some.png` → the image is behind the prompt text, which stays readable; colored cell backgrounds (`printf '\e[41m   \e[0m'`) still cover it; `--z-index -1073741825` → behind the colored cells too
  - tmux: `tmux` then `kitten icat --unicode-placeholder some.png` (or yazi in tmux) → the image appears and scrolls with tmux's text; resizing the pane does not leave garbage
  - `kitten icat some.gif` → the GIF animates; scroll it out of view → CPU drops to ~0% (Activity Monitor); scroll back → it animates again
  - `kitten icat --transfer-mode=memory some.png` → shown (shared memory)

- [x] **Right-to-left text**
  - `echo 'abc שלום עולם 123 def'` → the Hebrew words read right to left ("עולם" left of "שלום"), `123` and the Latin words unchanged
  - `echo 'سلام عليكم'` → letters joined (connected script, not isolated letters), words in right-to-left order
  - Click / drag-select inside the Hebrew word → the cell under the mouse is selected; copy gives the text in logical order
  - The cursor after typing Hebrew at the prompt sits at the visual position of the next letter
  - `vim` with Arabic/Hebrew text → shown unreordered (the app is responsible), as in other terminals

## Verification debt

- [x] Manually test Kitty keys in the real app: letters, `Ctrl`/`Alt`+letter, `Esc`, `Tab` vs `Ctrl+I`, key repeat (verify `key_without_modifiers()` on macOS)
- [x] Manually test OSC 8 hover (`Cmd`/`Ctrl` + mouse move underlines the link, preview pill shows the URI)
- [ ] **Needs a Linux machine: Kitty lock modifiers (Linux)**
  - `kitten show-key -m kitty`, Caps Lock on, press `a` → modifiers include `caps_lock`; off → not; Num Lock on + keypad `7` → `num_lock`
- [ ] **Needs a Linux machine: Quick Terminal (Linux)**
  - X11: `keybind = global:ctrl+grave_accent=toggle_quick_terminal` + reload; from another window press it → a borderless Litty panel at the top, again → hidden
  - Wayland: create a desktop shortcut running `litty +toggle-quick-terminal` → toggles the same way; with Litty not running it prints "no running Litty"

## Tech debt (v0.4.1)

Found while benchmarking v0.4.0 against v0.3.0 (`perf_bench` parser harness on a
160×50 grid, best of 3 interleaved rounds; runtime probe through `$SHELL`).

- [x] **Unicode parser throughput ~16% below v0.3.0** (≈100 vs ≈120 MB/s on the mixed Vietnamese / CJK / emoji / box-drawing corpus)
  - Resolved: fast-path common narrow scripts (Latin-1, Vietnamese Latin Extended `0x1EA0..=0x1EF9`, Box Drawing `0x2500..=0x259F`) directly in `print()` when `!self.joiner`.
  - In `put_char()`, avoid calling `is_wide_cp()` unless `r[last + 1].codepoint == 0`, skipping width lookups on normal single-width overwrites.
  - Benchmark gains: Latin-1 +25.5% (80.7 -> 101.3 MB/s), Box drawing +17.5% (88.2 -> 103.6 MB/s), Vietnamese +5.6% (69.2 -> 73.1 MB/s).
- [x] **Startup footprint ~6.5 MB above v0.3.0** (≈77 vs ≈70.5 MB physical footprint, idle shell)
  - Resolved: `atlas::load_font()` was parsing the primary font twice (first with `substitutions: false`, then discarding it and reparsing with `load_substitutions: true`), allocating and freeing ~10k small heap objects and fragmenting the macOS `Malloc Small` zone.
  - Fixed: Determine `shaper` upfront and parse the primary font directly once with `load_substitutions: shaper.is_some()`, eliminating the secondary parse and heap churn.
- [x] **Linux release binary not stripped** (9.1 MB vs 5.7 MB in v0.3.0)
  - Resolved / Clarified: Verified that the Linux release binary is already stripped (`file` reports `stripped`, `strip = true` in `[profile.release]`, `nm` reports `no symbols`).
  - The 5.7 -> 9.1 MB delta is actual executable code & data from dependencies introduced in v0.4.0: `rustybuzz` (OpenType layout & shaping tables), `wgpu`'s GLSL feature (the full `naga` front-end compiler for custom shaders), `regex`, `png`, `miniz_oxide`, `x11rb`, and `assets/themes.txt` (422 KB).

## Security & Code Quality Audit (v0.4.1)

Comprehensive code quality, memory safety, and security scan performed on 2026-10-28.
Baseline status: 186 unit/integration tests passing, clean `cargo clippy --all-targets -- -D warnings`.

- [x] **High: Validate hexadecimal characters in XTGETTCAP DCS responses (`src/grid.rs`)**
  - Vulnerability: Unsanitized input echoed back in `reply_termcap` for unknown/invalid capabilities (`\x1bP0+r<hex_name>\x1b\`) could inject control characters or commands into the shell stdin queue.
  - Fix: Validate that `hex_name` contains strictly valid ASCII hexadecimal characters (`0-9`, `a-f`, `A-F`); drop or ignore requests containing invalid characters.
- [x] **High: Restrict Kitty graphics protocol local file access & temporary file deletion (`src/graphics.rs`)**
  - Vulnerability: `t=f` / `t=t` loads arbitrary filesystem paths, allowing remote SSH sessions or untrusted output to probe local file existence/errors. `t=t` unlinks any file containing `tty-graphics-protocol`.
  - Fix: Restrict `t=f` and `t=t` file reads to local secure temporary directories (e.g. `$TMPDIR`, `/tmp`) or private cache; ensure `t=t` unlinks only files strictly within Litty's designated temporary directory.
- [x] **Medium: Positional argument delimiter `--` and executable guard in `open_target` (`src/main.rs`)**
  - Vulnerability: Direct argument passing to `open` (macOS) and `xdg-open` (Linux) without `--` permits flag injection for targets starting with `-`. On macOS, clicking `.app` / `.command` files via `open` executes them directly.
  - Fix: Pass `--` delimiter before targets (`open -- <path>`). Guard or confirm opening executable files/scripts from terminal clicks or OSC 8 hyperlinks.
- [x] **Medium: Close all file descriptors $\ge 3$ post-fork before `execve` (`src/pty.rs`)**
  - Vulnerability: Child processes close only `master` and `slave`. Any descriptors opened without `O_CLOEXEC` by drivers, threads, or concurrent PTY allocations leak into child shells.
  - Fix: Call `closefrom(3)` (macOS/BSD) or `close_range(3, !0, 0)` (Linux) in the child process immediately after `fork()`.
- [x] **Low: Control socket placement in user-private directory (`src/hotkey.rs`)**
  - Vulnerability: When `XDG_RUNTIME_DIR` is missing, `/tmp/litty-{uid}.sock` can be pre-created by other users in `/tmp` (sticky bit), causing `bind` to fail (local DoS on global hotkey).
  - Fix: Store the control socket in a secure private directory (e.g., `~/.cache/litty/` or Darwin user temp directory).
- [x] **Quality / Memory: Font Mmap lifecycle on configuration reload (`src/atlas.rs`)**
  - Debt: `Box::leak` in `shaper_for` leaks `memmap2::Mmap` on every config reload (`Cmd+Shift+,`).
  - Fix: Manage mmap lifetimes using reference-counted or owning structures (`Arc<memmap2::Mmap>`) so reloads reclaim memory.
- [x] **Defense-in-depth: Multiline paste confirmation when bracketed paste is disabled (`src/main.rs`)**
  - Enhancement: Protect against accidental multiline paste execution in shells or CLI utilities running without bracketed paste mode.


## Memory Leak & Resource Management Audit

Comprehensive memory leak, cache accumulation, lifecycle, and zombie process audits conducted across macOS and Linux architectures.

### Phase 1: Core Memory Leaks & Unbounded Caches (Completed 2026-10-28)

- [x] **Critical: Fix Objective-C subview retain count leaks in progress dialog (`src/macos.rs`)**
  - Vulnerability / Leak: In `show_progress_dialog()`, `NSImageView`, two `NSTextField` labels, and `NSProgressIndicator` were created with `alloc` + `initWithFrame:` (+1 retain count) and added via `addSubview:` (content view retains +1). Missing `release` post-add permanently leaked subviews when the dialog was closed.
  - Fix: Call `msg_send!(subview, c"release")` immediately after `addSubview:` for each subview.
- [x] **Critical / Safety: Prevent premature unmap and dangling pointer in Linux color emoji shaper (`src/atlas.rs`)**
  - Vulnerability / Safety: In `rasterize_color()` on Linux (`not(target_os = "macos")`), `shaper_for(&face.path, face.index).map(|(f, _)| f)` discarded the `Arc<memmap2::Mmap>`, immediately unmapping the font file while `rustybuzz::Face` retained a dangling pointer to unmapped memory.
  - Fix: Store `(rustybuzz::Face<'static>, Arc<memmap2::Mmap>)` inside `OnceLock` so the memory-mapped bytes remain alive.
- [x] **Medium: Prune Kitty image placements on scroll and free unreferenced image data (`src/graphics.rs`, `src/grid.rs`)**
  - Vulnerability / Leak: `graphics.prune()` was only called on image placement or history clear; normal text scrolling (`scroll_up_in`) never invoked `prune()`, leaving placements of long-scrolled-off images in memory. Furthermore, `prune()` filtered `placements` but never cleaned up orphaned images in `self.images` (retaining up to `STORE_LIMIT = 64 MB` per terminal).
  - Fix: Invoke `prune(first_line)` during scrollback eviction in `scroll_up_in()`, and clean up images in `self.images` that have no remaining standard or virtual placements.
- [x] **Medium: Cap pending buffer size in chunked Kitty transmissions against OOM (`src/graphics.rs`)**
  - Vulnerability / DoS: In `Store::kitty()`, multi-chunk transmissions (`m=1`) appended payload chunks into `self.pending` without an upper bound check, allowing rogue or runaway processes to consume arbitrary memory until OOM.
  - Fix: Impose a maximum buffer limit on `self.pending` (matching `STORE_LIMIT`), aborting the transmission and releasing the buffer if exceeded.
- [x] **Low: Reset or bound grapheme cluster interning tables (`src/atlas.rs`, `src/grid.rs`)**
  - Vulnerability / Bloat: `Atlas::clusters` and `Atlas::cluster_ids` appended up to 2 million strings without eviction even when Atlas texture slots were evicted. `Grid::clusters` was also never cleared on `clear_screen_and_history()` (`Cmd+K`) or `full_reset()` (`ESC c`).
  - Fix: Reset or prune grapheme cluster caches on clear/reset, and bound the atlas cluster table to 4096 entries with an eviction/reset policy.
- [x] **Low: Reap browser helper process when opening release URL (`src/main.rs`)**
  - Vulnerability / Leak: `process_window_requests()` launched `open` (macOS) or `xdg-open` (Linux) via bare `.spawn()` without `wait()` or `spawn_detached()`, leaving a zombie process in the OS process table upon exit.
  - Fix: Replace bare `.spawn()` calls with `spawn_detached()`.

### Phase 2: Deep Scan — Process Zombies, GPU VRAM & Table Bloat (Completed 2026-10-29)

- [x] **Critical: Reap child process on PTY writer/reader setup failure (`src/pty.rs`)**
  - Vulnerability / Zombie leak: In `Pty::spawn_in`, if `dup_cloexec(master)` or `thread::Builder::spawn` failed, or in `spawn_reader` if `dup_cloexec` or `spawn` failed, the function returned early without calling `reap_child(pid)`. The child shell process was left running or became an un-reaped zombie process.
  - Fix: Catch errors and guarantee `reap_child(pid)` is called on all error paths before returning `Err(e)`.
- [x] **Medium: Release GPU image textures and postfx render targets on window shrink / tab close (`src/image_layer.rs`, `src/postfx.rs`, `src/renderer.rs`)**
  - Vulnerability / VRAM leak: `Renderer::shrink_drawables()` shrinks swapchain textures to 1x1 when running tabless in Dock, but `ImageLayer::textures` (cached Kitty/Sixel GPU textures) and `PostFx::targets` (offscreen ping-pong textures) remained allocated in GPU VRAM indefinitely.
  - Fix: Add `ImageLayer::clear()` and `PostFx::shrink()`, and invoke them inside `shrink_drawables()` to reclaim all GPU VRAM textures immediately.
- [x] **Medium: Clean up unused `CellExtra` / hyperlink slots on screen & history clear (`src/grid.rs`)**
  - Vulnerability / Leak: `Grid::clear_screen_and_history()` resets rows and history, but left up to 256 `CellExtra` (holding heap-allocated `OscLink` URI and ID strings) in `self.extras` until the table overflowed.
  - Fix: Add `trim_extras()` and invoke it on `clear_screen_and_history()` and `full_reset()` to drop dead hyperlink strings and truncate empty slots back to `[None]` immediately.
- [x] **Low: Fix Objective-C `NSVisualEffectView` retain count leak (`src/macos.rs`)**
  - Vulnerability / Leak: In `setup_background_blur()`, `NSVisualEffectView` was allocated with `alloc` + `initWithFrame:` (+1 retain count) and added via `addSubview:positioned:relativeTo:` (incrementing retain count to 2). Missing `release` left it permanently retained.
  - Fix: Send `msg_send!(effect_view, c"release")` immediately after `addSubview:positioned:relativeTo:`.
- [x] **Low: Prevent zombie process if reaper thread fails in `spawn_detached` (`src/main.rs`)**
  - Vulnerability / Zombie leak: If `thread::Builder::spawn` fails in `spawn_detached()`, the child process was dropped without `child.wait()`, leaving a zombie.
  - Fix: Record process PID and synchronously kill and wait on `child` via `libc::kill` and `libc::waitpid` before returning `Err(e)`.

### Phase 3: Exhaustive Audit — Edge Cases, State Invalidation & OS Integration (Completed 2026-10-29)

- [x] **Medium: Bound in-session command history to `MAX_HISTORY_ITEMS` (`src/history.rs`)**
  - Vulnerability / Memory leak: In `HistorySearch::add_session_command()`, `self.session_commands` was capped to 500 entries, but `self.items.insert(0, trimmed.to_string())` did not check `MAX_HISTORY_ITEMS` (10,000). In long-running sessions, `self.items` grew unboundedly without trimming older items.
  - Fix: Pop excess items from `self.items` whenever length exceeds `MAX_HISTORY_ITEMS`.
- [x] **Medium: Invalidate stale pane-scoped state on pane exit and tab close (`src/main.rs`)**
  - Vulnerability / State leak: When a split pane was closed or its child exited, the window `State` continued holding references to the dead pane ID in `search.pane_id`, `hovered_link`, `scrollbar_drag`, `active_click_pane_id`, and `last_mouse_report`.
  - Fix: Add `State::invalidate_pane_state(pane_id)` and invoke it whenever a pane exits or is closed, clearing search state, hovered hyperlinks, drag states, and mouse reporting coordinates.
- [x] **Medium: Prune on-screen image placements and orphaned auto-id images on screen clear & alt-screen exit (`src/graphics.rs`, `src/grid.rs`)**
  - Vulnerability / VRAM & RAM leak: On `Cmd+K` (`clear_screen_and_history`), placements on visible rows (`lines_pushed..lines_pushed + rows`) were never deleted, leaving phantom image placements and unreferenced image buffers in RAM. Furthermore, on exiting the alternate screen or clearing lines, auto-assigned images (`id >= (1 << 24)`) whose placements were deleted remained in `self.graphics.images` indefinitely until byte-budget eviction.
  - Fix: Add `Store::prune_dead_auto_ids()` and invoke it in `delete_lines()`, `set_alt(false, ...)`, and `clear_screen_and_history()`.
- [x] **Medium: Prevent system-wide macOS Secure Event Input leak on window drop / dock hide (`src/main.rs`)**
  - Vulnerability / OS resource leak: When a window with `secure_input: true` and `focused: true` was closed or hidden into the Dock without tabs, `macos::set_secure_input(false)` was never called because `State` lacked a `Drop` implementation. Because `EnableSecureEventInput()` is an OS-wide reference-counted Carbon API, this left the entire operating system permanently locked in Secure Keyboard Entry mode, breaking global hotkeys and accessibility utilities across all applications.
  - Fix: Implement `Drop for State` to guarantee `macos::set_secure_input(false)` on drop, and explicitly release secure input and close search popups in `hide_or_exit_last_window()`.
- [x] **Low: Ensure update staging directory & temporary binary cleanup on error (`src/update.rs`)**
  - Vulnerability / Disk leak: In `install_release()`, if extraction (`ditto`), architecture verification (`check_binary_arch`), or installation failed, `staging_dir` (on macOS) and `temp_dest` (standalone binary) were left behind in the updates cache without cleanup.
  - Fix: Guard both temporary locations with RAII drop guards so they are guaranteed to be cleaned up immediately whenever an error occurs.
- [x] **Low: Cache GPU BindGroups in PostFx to prevent per-frame allocations (`src/postfx.rs`)**
  - Vulnerability / GPU allocation churn: In `PostFx::run()`, `device.create_bind_group` was called every frame for each shader pipeline pass, allocating hundreds of transient GPU bind groups per second.
  - Fix: Pre-create `bind_groups` inside `cell_target()` when render targets resize, reuse them in `run()`, and release them in `shrink()`.
- [x] **Low: Support vibrancy & background blur across multiple windows (`src/macos.rs`)**
  - Vulnerability / State bug: `setup_visual_effect_view` used a global process-wide `AtomicBool` (`BLUR_SETUP`), which prevented vibrancy and blur from ever being attached to secondary windows (`Cmd+N`).
  - Fix: Remove `BLUR_SETUP` and check the window's `contentView.subviews` for existing `NSVisualEffectView` instances before attaching, ensuring every window receives blur without duplicates.
- [x] **Medium: Prevent zombie process and heavy thread stacks in audio alerts (`src/main.rs`)**
  - Vulnerability / Zombie leak: `play_sound` spawned `/usr/bin/afplay` (macOS) or `paplay` (Linux) with a raw `std::thread::spawn(move || child.wait())`, allocating an 8MB thread stack per alert. If thread spawning failed, `child` was dropped without waiting, leaving an un-reaped zombie process. Furthermore, sound paths were passed without `--` argument delimiters.
  - Fix: Route player execution through `spawn_detached()`, ensuring dedicated lightweight (32 KB) reaper threads with automatic synchronous SIGKILL and `waitpid` fallback, and pass `--` before paths.
- [x] **High: Prevent child process zombies on early Pty drop or reader thread panic (`src/pty.rs`)**
  - Vulnerability / Zombie leak: If a `Pty` was instantiated via `Pty::spawn_in` and dropped before `spawn_reader` ran (or if pane creation failed), `Pty` lacked a `Drop` implementation and never called `reap_child(pid)`. Furthermore, if the reader thread panicked during byte feed/parsing, the thread unwound without reaping the child. The child shell process was left permanently as an un-reaped zombie in the OS process table.
  - Fix: Add `reader_spawned: AtomicBool` to `Pty` with `Drop for Pty` calling `reap_child(self.pid)` when the reader thread was not spawned. In `spawn_reader`, introduce an RAII `ChildReaper` drop guard to guarantee `reap_child(pid)` runs even if the reader thread panics.
- [x] **Medium: Invalidate pane state on tab close and single-pane child exit (`src/main.rs`)**
  - Vulnerability / State leak: Closing a tab via `close_active_tab()` (`Cmd+W`) or child process exit in a single-pane tab removed the tab but did not invoke `State::invalidate_pane_state()`, leaving dead pane IDs in `search.pane_id`, `hovered_link`, `scrollbar_drag`, `active_click_pane_id`, and `last_mouse_report`.
  - Fix: Invalidate pane state for all panes in the removed tab in `close_active_tab()` and in `AppEvent::ChildExit`.
- [x] **Medium: Roll back split tree layout on `spawn_pane` failure (`src/main.rs`)**
  - Vulnerability / State leak: In `split_active_pane()`, `tab.layout.split_pane(target_id, new_id, direction)` inserted `new_id` into the tree before `spawn_pane()`. If PTY or process spawning failed, `new_id` was never pushed into `tab.panes`, permanently leaving a ghost split leaf with no backing pane and corrupting layout navigation.
  - Fix: Call `tab.layout.remove_pane(new_id)` on `spawn_pane` error to restore the split tree.
- [x] **Medium: Prune Kitty virtual placements and auto-id images on screen clear (`src/grid.rs`)**
  - Vulnerability / VRAM leak: `clear_screen_and_history()` cleared all grid cells (destroying all U+10EEEE placeholder cells), but left `self.graphics.virtuals` intact. Because `prune_dead_auto_ids()` kept any auto-id image referenced by `self.graphics.virtuals`, the image data in RAM/VRAM was never freed and `virtuals` accumulated over time.
  - Fix: Clear `self.graphics.virtuals` and invoke `self.graphics.prune_dead_auto_ids()` inside `clear_screen_and_history()`.
- [x] **Medium: Prune orphaned auto-id images during grid resize reflow (`src/grid.rs`)**
  - Vulnerability / VRAM leak: During `Grid::resize()`, placements in scrollback truncated by text reflow were dropped from `self.graphics.placements`, but `prune_dead_auto_ids()` was never called. Auto-id images whose placements were dropped lingered in memory and VRAM until store limit eviction.
  - Fix: Invoke `self.graphics.prune_dead_auto_ids()` at the end of the text reflow block in `Grid::resize()`.
- [x] **Low: Clear grid search matches on dock hide (`src/main.rs`)**
  - Vulnerability / Idle RAM retention: In `hide_or_exit_last_window()`, `s.search.close()` was called instead of `close_search(s)`. This closed the search overlay without calling `grid.clear_search()`, leaving up to 10,000 `Selection` instances in `search_matches` on the active pane while sitting idle in the Dock.
  - Fix: Call `close_search(s)` to clear all grid search matches when hiding the window into the Dock.
- [x] **Low: Eliminate heap allocations in mouse event encoding (`src/mouse.rs`, `src/main.rs`)**
  - Vulnerability / Event loop allocation churn: `mouse::encode()` allocated a heap `Vec<u8>` on every mouse event (motion, drag, press, release) via `format!(...).into_bytes()` or `vec![...]`. In terminal applications tracking mouse motion (e.g. Vim, Tmux, Micro, Kakoune), this triggered hundreds of transient heap allocations per second.
  - Fix: Encode directly into a 32-byte stack buffer returning `Option<([u8; 32], usize)>`, passing slice `&bytes[..len]` to `pane.pty.write()`.
- [x] **Low: Eliminate heap allocation churn in visual order and bidi column lookups (`src/bidi.rs`, `src/grid.rs`)**
  - Vulnerability / Render loop allocation churn: In `bidi::visual_order`, `let classes: Vec<Class> = ... .collect();` allocated a heap vector every frame for each row containing right-to-left text. In `Grid::logical_col` and `Grid::cursor_visual_col`, every mouse movement, link hover, or cursor update allocated 3 temporary `Vec` instances on RTL rows.
  - Fix: Store character classes directly in the `levels` scratch buffer to resolve neutrals before mapping to bidi levels (zero allocations in `visual_order`). Introduce thread-local reusable scratch buffers in `Grid::logical_col` and `Grid::cursor_visual_col`.

## Deep Audit & Resource Management (v0.5.0)

- [x] **High: Kitty virtual placements leak and VRAM retention on alternate screen exit (`src/grid.rs`, `src/graphics.rs`)**
  - Vulnerability / VRAM & RAM leak: When TUI applications in the alternate screen (e.g. Neovim, Yazi, Ranger) place images with Unicode placeholders (`U=1`), a `VirtualPlacement` is registered. Unlike standard `Placement`, `VirtualPlacement` lacks an `alt: bool` flag. When exiting the alternate screen, `self.graphics.virtuals` is not pruned. Because `prune_dead_auto_ids()` checks `!self.virtuals.iter().any(|v| v.image_id == id)`, auto-assigned image buffers in RAM and textures in GPU VRAM are retained indefinitely across sessions.
  - Fix: Add `pub alt: bool` to `VirtualPlacement`, record `alt: self.alt_active` in `place_virtual()`, and filter `self.graphics.virtuals.retain(|v| !v.alt)` before calling `prune_dead_auto_ids()` on alternate screen exit.
- [x] **Medium: Hyperlink & SGR 58 underline color slots (`CellExtra`) stale retention on `clear_history` (`src/grid.rs`)**
  - Vulnerability / Slot exhaustion: `clear_history()` resets `self.history = 0` but does not zero out evicted cells in the circular ring buffer `self.cells`, nor does it invoke `trim_extras()`. Furthermore, `collect_extras()` scans all physical rows in `self.cells.iter()`, marking dead cells outside `0..(self.history + self.rows)` as active and permanently exhausting the 255 hyperlink/color slots.
  - Fix: Limit `collect_extras()` row traversal to active visible and scrollback rows (`0..(self.history + self.rows)`), and call `trim_extras()` inside `clear_history()`.
- [x] **Low: Inverted nested split ratio adjustment in multi-pane layouts (`src/split.rs`)**
  - Bug / Usability: In `SplitNode::adjust_ratio()`, `first.contains(target_id)` evaluates to `true` at the root split node if `target_id` is located anywhere inside the first subtree. As a result, the outermost root split ratio is adjusted rather than recursing down to the innermost split containing the active pane.
  - Fix: Recurse into child subtrees first (`first.adjust_ratio(...) || second.adjust_ratio(...)`), and only adjust the local ratio if the target is an immediate direct leaf of the node.
- [x] **Low: Eliminate per-line `Vec` heap allocations during window resize reflow (`src/grid.rs`)**
  - Optimization / Heap churn: During terminal resize, reflowing scrollback unwrapped each line into a separate heap-allocated `Vec<Cell>`, triggering thousands of small heap allocations when resizing windows with deep scrollback history.
  - Fix: Flatten intermediate logical line extraction into a single contiguous `logical_cells` buffer with `line_spans: Vec<Range<usize>>`, eliminating per-line heap allocations.
- [x] **Low: Reuse instance buffer and shrink oversized GPU vertex buffer on window resize (`src/renderer.rs`)**
  - Optimization / GPU & RAM bloat: When cell count changed, `self.instances` was re-allocated from scratch with `vec![Instance::default(); n]`, discarding existing capacity. Additionally, shrinking a large window retained the peak GPU instance buffer indefinitely without shrinking.
  - Fix: Reuse `self.instances` with `clear()` and `resize()`, shrinking capacity when more than double the needed size, and reallocate the GPU instance buffer when it exceeds twice the required footprint.



## Terminal protocols

- [x] OSC 4 / 10 / 11 / 12 set and query (palette, fg, bg, cursor color) + OSC 104 / 110 / 111 / 112 reset
- [x] OSC 4 / 104 recolor text already on screen (matched by color, since cells store RGB: a true-color cell of exactly the old color changes too)
- [x] X11 color names in OSC color specs (`red`, `slate blue`, `gray50`, …)
- [x] `modifyOtherKeys` (`CSI > 4 ; 2 m`) for apps that do not speak the Kitty protocol
- [x] Kitty protocol: Caps Lock modifier on macOS (read from AppKit)
- [x] Kitty protocol: Caps Lock / Num Lock on Linux (inferred from the keys typed: winit exposes no lock state)
- [x] Kitty protocol: pass unbound `Cmd`/`Super` combos to apps that enabled it (nvim `<D-…>`)
- [x] OSC 9 / OSC 777 desktop notifications
- [x] OSC 9;4 progress reporting (shown in the tab / window title)
- [x] OSC 9;4 progress as a bar drawn over the pane (like Ghostty; active pane only, indeterminate is static)
- [x] OSC 22 mouse pointer shape
- [x] XTGETTCAP and DECRQSS replies (+ XTWINOPS 14t / 16t / 18t size reports)
- [x] DEC modes 1016 (SGR pixel mouse) and 2031 (color scheme change notifications, DSR 996)
- [x] Color emoji (second, RGBA atlas: Apple Color Emoji via CoreText on macOS, Noto Color Emoji CBDT bitmaps on Linux)
- [x] DEC mode 2027 grapheme clustering (`grapheme-width-method = unicode`, the default): ZWJ sequences, flags, skin tones, VS16 width upgrade, keycaps; clusters interned per terminal and referenced from the 12-byte cell
- [x] Combining marks without a precomposed form kept with their base (previously dropped)
- [x] Kitty graphics protocol (RGB / RGBA / PNG, zlib, direct / file / temp-file transmission, chunked, `a=T/t/p/q/d`, replies, `c`/`r` sizing, cursor movement)
- [x] Kitty graphics: images under the text (`z < 0`, and below cell backgrounds under `-1073741824`), unicode placeholders (`U=1`, U+10EEEE), animation frames (`a=f`, `a=a`; driven by the clock, no idle wakeups), shared-memory transmission (`t=s`), `S=` / `O=`
- [x] Kitty graphics: frame composition (`a=c`)
- [x] Sixel graphics (palette, RGB/HLS colors, repeats, raster attributes, transparent background; DA1 advertises `4`)

## Fonts & rendering

- [x] Programming ligatures (`rustybuzz` shapes punctuation runs; substituted glyphs are drawn across the cells they cover)
- [x] Emoji ZWJ sequences and combining clusters (Indic matras, stacked marks) shaped as one glyph (rustybuzz with the primary font, CoreText's cascade on macOS when it lacks glyphs)
- [x] Right-to-left text on the primary screen: implicit bidi reordering per line (Hebrew, Arabic, digits stay left-to-right), Arabic and Persian contextual letter forms; cursor and mouse follow the visual order; the alternate screen is left to the application
- [x] `font-feature` (`-calt`, `-liga`, `ss01`, …)
- [x] `adjust-cell-height` / `adjust-cell-width`, `font-thicken`
- [x] Polish: smaller curly underline amplitude, more spacing in double underline
- [x] Custom shaders (`custom-shader`, Shadertoy-style GLSL, chained; `custom-shader-animation = true|false|always`)

## Configuration

- [x] Custom key bindings (`keybind = cmd+t=new_tab`, Ghostty syntax: actions, `text:` / `csi:` / `esc:`, `unbind`, `ignore`, `clear`)
- [x] More Ghostty config options: `cursor-color`, `selection-background` / `selection-foreground`, `copy-on-select`, `title`
- [x] `scrollback-limit`, `unfocused-split-opacity`, `unfocused-split-fill`, `minimum-contrast`
- [x] `confirm-close-surface` (`true` / `false` / `always`; NSAlert on macOS, `zenity` on Linux when installed)
- [x] Bundle the iTerm2-Color-Schemes theme set (861 themes, `+list-themes`; themes apply before the rest of the config)
- [x] `theme = light:X,dark:Y` following the system appearance
- [x] `window-padding-x` / `window-padding-y` / `window-padding-balance`
- [x] `auto-update` (`check` / `download` / `off`), `auto-update-channel` (`stable` / `tip`), menu item & CLI updater

## Windows & OS integration

- [x] Multiple windows (New Window, `Cmd+N` / `Ctrl+Shift+N`)
- [x] Quick Terminal on macOS (`keybind = global:cmd+grave_accent=toggle_quick_terminal`, `quick-terminal-autohide`)
- [x] Quick Terminal global hotkey on Linux: X11 key grab (`keybind = global:...`), and `litty +toggle-quick-terminal` over a control socket for a Wayland desktop shortcut
- [x] macOS menu bar (Litty / Shell / Edit / View / Window; `Cmd+Q` goes through `confirm-close-surface`)
- [x] Native macOS tabs (`macos-native-tabs = true`: `Cmd+T` opens a window tab; `Cmd+1..9` / `Cmd+Shift+[ ]` switch them)
- [x] Secure Keyboard Entry (macOS): `keybind = <trigger>=toggle_secure_input`, 🔒 in the title, released while unfocused
- [x] Secure Keyboard Entry from a menu bar item (Litty ▸ Secure Keyboard Entry, with check mark)
- [x] Drag and drop files to paste their shell-quoted paths
- [x] Scrollbar (overlay thumb while scrolled back)
- [x] Draggable scrollbar
- [x] Bell: dock bounce when unfocused + 🔔 on the tab title until it is seen
- [x] `bell-features` = `system` (alert sound), `attention`, `title`
- [x] `bell-features` `audio` (`bell-audio-path`, `bell-audio-volume`) and `border`
- [x] Terminal inspector (debug overlay, `Cmd+Option+I` / `Ctrl+Shift+I`, keybind `inspector:toggle`)

## Shell integration

- [x] Automatic zsh, bash and fish integration (scripts injected, no dotfile edits; previously only env vars were exported)
- [x] nushell (vendor autoload) and elvish (`use litty`) integration
- [x] bash 3.2 (macOS `/bin/bash`): OSC 133;C from a DEBUG trap (skipped if another tool owns the trap)
- [x] Jump to previous / next prompt using OSC 133 marks
- [x] Select or copy the output of the last command using OSC 133 marks
- [x] Keep prompt marks across a resize (remapped through the reflow)

## Code Health & Hardening (Audit Backlog)

### Security Fixes
- [x] Fix token leak in process table: pass GitHub API token via header file/stdin instead of command line flags in `curl` (`src/update.rs`)
- [x] Restrict Kitty graphics file protocol (`medium=f`) to user-configured safe directories instead of loose blacklist (`src/graphics.rs`)
- [x] Restrict / validate POSIX shared memory unlinking (`shm_unlink`) to Litty-owned prefixes (`src/graphics.rs`)
- [x] Gate OSC 52 clipboard write behind user confirmation or configuration flag (`src/grid.rs`, `src/main.rs`)
- [x] Add cryptographic signature verification (Minisign / GPG) for auto-updates (`src/update.rs`)
- [x] Fix Unix domain socket creation permission race window (`src/hotkey.rs`)
- [x] Use absolute paths for system utility executions (`curl`, `codesign`, `touch`, `open`, `pbcopy`/`pbpaste`, `xclip`) instead of bare `$PATH` lookups

### Crash & Panic Fixes
- [x] Fix `f32::clamp(min, max)` panic (`assertion failed: min <= max`) when split pane rects shrink to <= 1 cell (`src/split.rs`)
- [x] Fix out-of-bounds panic on `self.instances` during IME preedit rendering (`src/renderer.rs`)
- [x] Fix integer underflow panic when `total_rows == 0` on tiny/zero-sized grid (`src/renderer.rs`)
- [x] Fix integer underflow and invalid scissor rect panic in flipped target rendering (`src/image_layer.rs`)
- [x] Fix index out-of-bounds panic when searching on an empty grid (`src/search.rs`)
- [x] Prevent WGSL shader division by zero when `u.cols == 0` (`src/shader.wgsl`)
- [x] Guard against panicking on empty `panes` in `Tab::active_pane` (`src/main.rs`)

### Performance & Concurrency Fixes
- [x] Fix quadratic $O(H^2 \cdot W)$ reallocation and memory copy loop in Sixel image decoder (`src/graphics.rs`)
- [x] Replace $O(N)$ linear slot scan on atlas cache miss with constant/logarithmic LRU tracking (`src/atlas.rs`)
- [x] Make clipboard copy and paste asynchronous to prevent UI thread freezing on blocking pipe/xclip (`src/main.rs`)
- [x] Cache font directory scan results to eliminate massive synchronous disk I/O on startup and config reload (`src/fontdb.rs`)
- [x] Add timeout (`set_read_timeout`) to hotkey Unix control socket listener to prevent permanent thread hang (`src/hotkey.rs`)
- [x] Reduce `grid.lock()` hold duration in PTY reader thread by buffering or chunking feed operations (`src/pty.rs`)

### Memory Leaks & Resource Management
- [ ] Bounded channel and backpressure for PTY writer thread to prevent unbounded memory growth from terminal queries (`src/pty.rs`)
- [ ] Cap maximum buffered bytes and add timeouts for unclosed DCS / APC image streams (`src/grid.rs`)
- [ ] Stream hashing in `sha256_digest` without duplicating entire multi-megabyte download buffer (`src/update.rs`)
- [ ] Clean up `ACTIVE_PROGRESS_WINDOW` reference if progress dialog is closed by user (`src/macos.rs`)
- [ ] Close file descriptors > 4096 on macOS after `fork()` to prevent descriptor leaks into child shells (`src/pty.rs`)
- [ ] Ensure NUL termination in `proc_vnodepathinfo` path buffer before `CStr::from_ptr` (`src/pty.rs`)

### Code Quality & Logic Integrity
- [ ] Fix history deletion wiping all historical occurrences of a command string instead of the single selected entry (`src/history.rs`)
- [ ] Use unique temporary filenames in `remove_command_from_file` to prevent collisions between concurrent windows (`src/history.rs`)
- [ ] Scope history file modifications to the current active shell rather than mutating other shells' history files (`src/history.rs`)
- [ ] Replace unsound static lifetime cast on `Arc<Mmap>` font buffers in shaper face (`src/atlas.rs`)
- [ ] Check and report errors from `codesign` and `touch` in macOS app bundle updater (`src/update.rs`)
- [ ] Avoid duplicate KVO observer registrations on macOS effective appearance changes (`src/macos.rs`)

