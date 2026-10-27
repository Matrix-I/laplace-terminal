# Laplace Roadmap — Ghostty Feature Gaps

Features Ghostty has that Laplace does not yet, grouped by area. Roughly ordered
by priority inside each group. Tick an item when it lands on `main`.

## Done

- [x] OSC 8 hyperlinks (`4c822b4`)
- [x] Styled underlines SGR `4:1`..`4:5`, `21`, underline color SGR 58/59, strikethrough SGR 9/29 (`4c822b4`)
- [x] Kitty keyboard protocol, all five progressive enhancements (`6e948c1`)
- [x] Select all: `Cmd+A` (macOS), `Ctrl+Shift+A` / `Super+A` (Linux)
- [x] `Shift+Click` / `Shift+Drag` extends the current selection to the mouse

## Manual test

Each item lists what to run inside a Laplace window and the expected result.
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
  - `sleep 3; printf '\e]9;Build finished\a'`, switch to another app within 3 s → a macOS notification "Laplace / Build finished"
  - `sleep 3; printf '\e]777;notify;Tests;All passed\a'`, switch away → notification titled "Tests"
  - `printf '\e]9;4;1;45\a'` → window / tab title starts with `45% `; `printf '\e]9;4;3\a'` → `⋯ `; `printf '\e]9;4;2;80\a'` → `✖ 80% `; `printf '\e]9;4;0\a'` → prefix gone
  - `printf '\e]22;pointer\a'` then move the mouse over the terminal → hand cursor; `printf '\e]22;text\a'` → I-beam; `printf '\e]22;\a'` → arrow
  - Open a 2nd tab, run `sleep 2; printf '\a'` in it and switch to tab 1 → that tab's title gets `🔔 `, cleared when you switch back
  - `sleep 3; printf '\a'` and switch to another app → Laplace bounces in the Dock

- [x] **XTGETTCAP, DECRQSS, pixel mouse, color scheme**
  - In `nvim`: `:checkhealth` shows no terminal warnings about true color / undercurl, and `:lua print(vim.o.termguicolors)` → `true`
  - `bash -c 'printf "\eP+q544E\e\\"; IFS= read -rs -d "\\" -t 1 r; echo "${r#?}" | cat -v'` → `P1+r544E=787465726D2D323536636F6C6F72^[` (TN = xterm-256color)
  - `bash -c 'printf "\e[1;31m\eP\$qm\e\\\e[0m"; IFS= read -rs -d "\\" -t 1 r; echo "${r#?}" | cat -v'` → contains `1$r0;1;38;2;…m`
  - `printf '\e[?1016h\e[?1003h'; cat -v`, move the mouse → `^[[<35;X;YM` with X/Y in pixels (hundreds, not cell numbers); `Ctrl+C`, then `printf '\e[?1016l\e[?1003l'`
  - `printf '\e[?2031h'; cat -v`, switch macOS Appearance between Light and Dark → `^[[?997;2n` / `^[[?997;1n`; `Ctrl+C`, then `printf '\e[?2031l'`

- [x] **Config: padding, light/dark theme, keybind, colors, title, copy-on-select** (edit `~/.laplace/config`, then `Cmd+Shift+,` to reload)
  - `window-padding-x = 24` + `window-padding-y = 12` → wider margins; `window-padding-balance = true` → the grid is centered (equal leftover on both sides)
  - `theme = light:Horizon Bright,dark:Dracula` → Dracula colors; switch macOS Appearance to Light → Horizon Bright immediately, back to Dark → Dracula
  - `keybind = cmd+t=new_split:right` → `Cmd+T` splits instead of opening a tab; `keybind = cmd+d=unbind` → `Cmd+D` no longer splits; `keybind = ctrl+shift+h=text:hello\n` → types `hello` + Enter; `keybind = clear` → defaults back
  - `cursor-color = #ff8800` → orange cursor (OSC 12 still overrides); `selection-background = #335533` + `selection-foreground = #ffff00` → green selection with yellow text
  - `copy-on-select = false` → dragging a selection no longer changes the clipboard; `Cmd+C` still copies
  - `title = Work` → window title stays "Work" whatever runs

- [x] **Shell integration, prompt jump, output selection** (open a new Laplace window so the shell starts with the integration)
  - `echo $LAPLACE_SHELL_INTEGRATION_DIR` → `~/.cache/laplace/shell-integration/<version>`; your usual prompt, aliases and `.zshrc` all still load
  - `seq 50; ls; seq 30`, then `Cmd+Shift+↑` repeatedly → the view jumps prompt by prompt; `Cmd+Shift+↓` back down, past the last one returns to live
  - `ls -la` then `Cmd+Shift+O` → exactly the `ls -la` output is highlighted and in the clipboard (not the prompt, not the command line)
  - `cd /tmp` then `Cmd+T` → the new tab opens in `/tmp`
  - With `shell-integration-features = cursor,title`: bar cursor while typing at the prompt, block cursor inside `vim`; the title shows the directory, then the running command
  - `shell-integration = none` in the config + new window → `echo $LAPLACE_SHELL_INTEGRATION_DIR` is empty and nothing above works
  - bash: run `SHELL=/bin/bash ./target/release/laplace` from the repo → your `.bash_profile` loads without errors, prompt jump works (output selection needs bash 4.4+, e.g. `SHELL=/opt/homebrew/bin/bash`)

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
  - `SHELL=/bin/bash ./target/release/laplace`: `ls -la` then `Cmd+Shift+O` → the output is selected (bash 3.2 now has the C mark); Enter on an empty prompt, then `Cmd+Shift+O` → still the `ls -la` output
  - nushell 0.101+ (if installed): `SHELL=$(which nu) ./target/release/laplace` → prompt jump and output selection work, `$env.XDG_DATA_DIRS` does not contain `laplace`
  - elvish (if installed): add `use laplace` to `~/.config/elvish/rc.elv`, start with `SHELL=$(which elvish)` → prompt jump works

- [x] **Bundled themes**
  - `./target/release/laplace +list-themes | wc -l` → 861; `| grep -i gruvbox` lists several
  - `theme = Gruvbox Dark` (or any listed name, any case) + reload → colors change, including cursor and selection colors from the theme
  - `background = #000000` placed *before* `theme = Dracula` → background stays black, the rest is Dracula
  - `theme = catppuccin-mocha` / `tokyonight` (old aliases) still work

- [x] **confirm-close-surface**
  - Run `sleep 100`, press `Cmd+W` → "Close this terminal?" alert; Cancel keeps it, Close closes it
  - At an idle prompt `Cmd+W` closes without asking
  - `sleep 100` in one window, `Cmd+Q` → "Quit Laplace?"; the red close button on that window → "Close this window?"
  - `confirm-close-surface = false` → never asks; `= always` → asks even at an idle prompt

- [x] **macOS menu bar**
  - The menu bar shows Laplace / Shell / Edit / View / Window; every item works on the focused window (New Window, New Tab, Split Right/Down, Close, Copy, Paste, Select All, Find, Clear Screen, Bigger/Smaller/Actual Size, Settings…)
  - Laplace ▸ Secure Keyboard Entry → check mark + 🔒 in the title; again → off
  - `Cmd+H` hides, `Ctrl+Cmd+F` toggles full screen, Window ▸ Minimize works and the Window menu lists open windows
  - `sleep 100` then `Cmd+Q` → "Quit Laplace?" (before, `Cmd+Q` quit immediately)
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
  - Without the option: `Cmd+T` uses Laplace's own tab bar as before

- [x] **Quick Terminal (macOS)**
  - `keybind = global:ctrl+grave_accent=toggle_quick_terminal` + reload; from another app press `Ctrl+\`` → a borderless Laplace panel drops in at the top of the screen (full width, ~40% height) and has focus
  - Press it again → hidden, focus goes back to the previous app; again → same shell, history intact
  - Click another app while it is shown → it hides (`quick-terminal-autohide = false` keeps it)
  - `exit` in it → closed; the hotkey opens a fresh one; normal windows are unaffected

- [x] **Custom shaders**
  - `custom-shader = <repo>/examples/shaders/invert.glsl` + reload → colors inverted, text upright (not mirrored), selection / cursor / tab bar still line up with the mouse
  - Add `custom-shader = <repo>/examples/shaders/scanlines.glsl` → inverted *and* moving scanlines while focused; switch to another app → animation stops (CPU back to ~0%); `custom-shader-animation = false` → static
  - A shader with a typo → Laplace still starts, the error is printed on stderr (run from a terminal) and that shader is skipped
  - Remove the lines + reload → normal rendering

- [x] **OSC 4 recolors existing text**
  - `printf '\e[31mred\e[0m\n'`, then `printf '\e]4;1;#00ff00\a'` → the earlier "red" turns green; `printf '\e]104\a'` → red again
  - `ls --color` output follows a palette change too

- [x] **Ligatures and font-feature**
  - Install a ligature font (e.g. JetBrains Mono, Fira Code; a copy is at `/private/tmp/claude-501/-Users-linh-nguyen-Desktop-personal-laplace-terminal/d22b6e14-bfe2-4859-821c-3e2b0b0c6257/scratchpad/fonts/JetBrainsMono-Regular.ttf`) and set `font-family = JetBrains Mono` (or run `LAPLACE_FONT=<that .ttf> ./target/release/laplace` with no `font-family` in the config)
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
  - X11: `keybind = global:ctrl+grave_accent=toggle_quick_terminal` + reload; from another window press it → a borderless Laplace panel at the top, again → hidden
  - Wayland: create a desktop shortcut running `laplace +toggle-quick-terminal` → toggles the same way; with Laplace not running it prints "no running Laplace"

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
- [x] Quick Terminal global hotkey on Linux: X11 key grab (`keybind = global:...`), and `laplace +toggle-quick-terminal` over a control socket for a Wayland desktop shortcut
- [x] macOS menu bar (Laplace / Shell / Edit / View / Window; `Cmd+Q` goes through `confirm-close-surface`)
- [x] Native macOS tabs (`macos-native-tabs = true`: `Cmd+T` opens a window tab; `Cmd+1..9` / `Cmd+Shift+[ ]` switch them)
- [x] Secure Keyboard Entry (macOS): `keybind = <trigger>=toggle_secure_input`, 🔒 in the title, released while unfocused
- [x] Secure Keyboard Entry from a menu bar item (Laplace ▸ Secure Keyboard Entry, with check mark)
- [x] Drag and drop files to paste their shell-quoted paths
- [x] Scrollbar (overlay thumb while scrolled back)
- [x] Draggable scrollbar
- [x] Bell: dock bounce when unfocused + 🔔 on the tab title until it is seen
- [x] `bell-features` = `system` (alert sound), `attention`, `title`
- [x] `bell-features` `audio` (`bell-audio-path`, `bell-audio-volume`) and `border`
- [x] Terminal inspector (debug overlay, `Cmd+Option+I` / `Ctrl+Shift+I`, keybind `inspector:toggle`)

## Shell integration

- [x] Automatic zsh, bash and fish integration (scripts injected, no dotfile edits; previously only env vars were exported)
- [x] nushell (vendor autoload) and elvish (`use laplace`) integration
- [x] bash 3.2 (macOS `/bin/bash`): OSC 133;C from a DEBUG trap (skipped if another tool owns the trap)
- [x] Jump to previous / next prompt using OSC 133 marks
- [x] Select or copy the output of the last command using OSC 133 marks
- [x] Keep prompt marks across a resize (remapped through the reflow)
