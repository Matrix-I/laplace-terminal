# Laplace Roadmap — Ghostty Feature Gaps

Features Ghostty has that Laplace does not yet, grouped by area. Roughly ordered
by priority inside each group. Tick an item when it lands on `main`.

## Done

- [x] OSC 8 hyperlinks (`4c822b4`)
- [x] Styled underlines SGR `4:1`..`4:5`, `21`, underline color SGR 58/59, strikethrough SGR 9/29 (`4c822b4`)
- [x] Kitty keyboard protocol, all five progressive enhancements (`6e948c1`)
- [x] Select all: `Cmd+A` (macOS), `Ctrl+Shift+A` / `Super+A` (Linux)
- [x] `Shift+Click` / `Shift+Drag` extends the current selection to the mouse

## Verification debt

- [ ] Manually test Kitty keys in the real app: letters, `Ctrl`/`Alt`+letter, `Esc`, `Tab` vs `Ctrl+I`, key repeat (verify `key_without_modifiers()` on macOS)
- [ ] Manually test OSC 8 hover (`Cmd`/`Ctrl` + mouse move underlines the link, preview pill shows the URI)

## Terminal protocols

- [ ] OSC 4 / 10 / 11 / 12 set and query (palette, fg, bg, cursor color) + OSC 104 / 110 / 111 / 112 reset
- [ ] `modifyOtherKeys` (`CSI > 4 ; 2 m`) for apps that do not speak the Kitty protocol
- [ ] Kitty protocol: report Caps Lock / Num Lock modifiers (winit lacks lock state)
- [ ] Kitty protocol: pass unbound `Cmd`/`Super` combos to apps that enabled it (nvim `<D-…>`)
- [ ] OSC 9 / OSC 777 desktop notifications
- [ ] OSC 9;4 progress reporting
- [ ] OSC 22 mouse pointer shape
- [ ] XTGETTCAP and DECRQSS replies
- [ ] DEC modes 1016 (SGR pixel mouse), 2027 (grapheme clustering), 2031 (color scheme change notifications)
- [ ] Kitty graphics protocol
- [ ] Sixel graphics

## Fonts & rendering

- [ ] Text shaping and ligatures (`rustybuzz`); needs an atlas redesign that keeps hot paths allocation-free
- [ ] `font-feature` (`calt`, `ss01`, …)
- [ ] `adjust-cell-height` / `adjust-cell-width`, `font-thicken`
- [ ] Polish: smaller curly underline amplitude, more spacing in double underline
- [ ] Custom shaders (`custom-shader`)

## Configuration

- [ ] Custom key bindings (`keybind = cmd+t=new_tab`, Ghostty syntax)
- [ ] More Ghostty config options (only ~20 keys are read today)
- [ ] Bundle the iTerm2-Color-Schemes theme set (~300 themes; Laplace ships ~8)
- [ ] `theme = light:X,dark:Y` following the system appearance
- [ ] `window-padding-x` / `window-padding-y` / `window-padding-balance`

## Windows & OS integration

- [ ] Multiple windows (New Window)
- [ ] Quick Terminal (global hotkey drop-down window)
- [ ] Native macOS tabs and menu bar
- [ ] Secure Keyboard Entry (macOS)
- [ ] Drag and drop files to paste their paths
- [ ] Scrollbar
- [ ] Bell: audible and visual (`bell-features`)
- [ ] Terminal inspector (debug overlay)

## Shell integration

- [ ] fish and elvish integration scripts (zsh and bash exist)
- [ ] Jump to previous / next prompt using OSC 133 marks
- [ ] Select or copy the output of the last command using OSC 133 marks
