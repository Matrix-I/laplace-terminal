# Manual test results

## 2026-10-27 09:07 (4524de2) — automated, run by Claude outside the app

| Group | Status | Item | Detail |
|---|---|---|---|
| Build | auto-pass | cargo test | 183 passed, 0 failed |
| Build | auto-pass | cargo clippy --all-targets | no warnings (only block v0.1.6 future-incompat note) |
| Build | auto-pass | cargo check --target x86_64-unknown-linux-gnu | ok |
| Build | auto-pass | cargo build --release | ok |
| Bundled themes | auto-pass | +list-themes count | 861 |
| Script | auto-pass | reply/key glob checks, Kitty PNG / Sixel / animation / placeholder generators | offline self-test |

## 2026-10-27 09:10 (4524de2)

| Group | Status | Item | Detail |
|---|---|---|---|

## 2026-10-27 09:11 (4524de2)

| Group | Status | Item | Detail |
|---|---|---|---|
| OSC 4/10/11/12 | pass | background dark purple, text light blue |  |
| OSC 4/10/11/12 | auto-pass | OSC 11 query | ^[]11;rgb:1e1e/1e1e/2e2e^G |
| OSC 4/10/11/12 | pass | cursor pink |  |
| OSC 4/10/11/12 | pass | text is green |  |
| OSC 4/10/11/12 | auto-pass | OSC 4 query | ^[]4;1;rgb:0000/ffff/0000^G |
| OSC 4/10/11/12 | pass | background navy (X11 name) |  |
| OSC 4/10/11/12 | pass | nvim sees background=light |  |
| OSC 4/10/11/12 | pass | all colors back to config |  |
| OSC 4/10/11/12 | pass | other split keeps its colors |  |
| modifyOtherKeys + Cmd/Super for Kitty | AUTO-FAIL | Ctrl+Enter | got: ^M want: ^\[\[27;5;13~ |
| modifyOtherKeys + Cmd/Super for Kitty | auto-pass | Ctrl+1 | ^[[27;5;49~ |
| modifyOtherKeys + Cmd/Super for Kitty | auto-pass | Ctrl+A | ^[[27;5;97~ |
| modifyOtherKeys + Cmd/Super for Kitty | auto-pass | Ctrl+A (mode 1) | ^A |
| modifyOtherKeys + Cmd/Super for Kitty | auto-pass | Ctrl+1 (mode 1) | ^[[27;5;49~ |
| modifyOtherKeys + Cmd/Super for Kitty | AUTO-FAIL | Cmd+J (kitty flags 1) | got: ^[[106;5u want: ^\[\[106;9u |
| modifyOtherKeys + Cmd/Super for Kitty | auto-pass | Cmd+J (no mode, wait 4 s) | <nothing> |
| Notifications, progress, pointer, bell | pass | notification 'Laplace / Build finished' |  |
| Notifications, progress, pointer, bell | pass | notification titled 'Tests' |  |
| Notifications, progress, pointer, bell | pass | title starts with '45% ' |  |
| Notifications, progress, pointer, bell | pass | title starts with '⋯ ' |  |
| Notifications, progress, pointer, bell | pass | title starts with '✖ 80% ' |  |
| Notifications, progress, pointer, bell | pass | prefix gone |  |
| Notifications, progress, pointer, bell | pass | hand cursor over the terminal |  |
| Notifications, progress, pointer, bell | pass | I-beam |  |
| Notifications, progress, pointer, bell | pass | arrow |  |
| Notifications, progress, pointer, bell | pass | that tab gets 🔔, cleared when you visit it |  |
| Notifications, progress, pointer, bell | pass | Laplace bounces in the Dock |  |
| XTGETTCAP, DECRQSS, pixel mouse, color scheme | auto-pass | XTGETTCAP TN | ^[P1+r544E=787465726D2D323536636F6C6F72^[\ |
| XTGETTCAP, DECRQSS, pixel mouse, color scheme | auto-pass | DECRQSS SGR | ^[P1$r0;1;38;2;233;86;120m^[\ |
| XTGETTCAP, DECRQSS, pixel mouse, color scheme | auto-pass | DA1 advertises sixel | ^[[?62;4;22c |
| XTGETTCAP, DECRQSS, pixel mouse, color scheme | auto-pass | XTWINOPS 18t | ^[[8;41;143t |
| XTGETTCAP, DECRQSS, pixel mouse, color scheme | auto-pass | XTWINOPS 14t | ^[[4;1476;2717t |
| XTGETTCAP, DECRQSS, pixel mouse, color scheme | auto-pass | DSR 996 color scheme | ^[[?997;1n |
| XTGETTCAP, DECRQSS, pixel mouse, color scheme | auto-pass | (move the mouse over the window) | ^[[<35;1552;478M |
| XTGETTCAP, DECRQSS, pixel mouse, color scheme | pass | pixel coordinates |  |
| XTGETTCAP, DECRQSS, pixel mouse, color scheme | AUTO-FAIL | (switch macOS Appearance Light↔Dark) | got: ^M want: ^\[\[?997;[12]n |
| XTGETTCAP, DECRQSS, pixel mouse, color scheme | pass | nvim health |  |
| Config: padding, theme, keybind, colors, title, copy-on-select | pass | wider margins, grid centered |  |
| Config: padding, theme, keybind, colors, title, copy-on-select | pass | Dracula; Light appearance → Horizon Bright live |  |
| Config: padding, theme, keybind, colors, title, copy-on-select | pass | Cmd+T splits, Cmd+D nothing, Ctrl+Shift+H types hello+Enter |  |
| Config: padding, theme, keybind, colors, title, copy-on-select | pass | defaults back |  |
| Config: padding, theme, keybind, colors, title, copy-on-select | pass | orange cursor, green selection with yellow text |  |
| Config: padding, theme, keybind, colors, title, copy-on-select | pass | dragging doesn't touch clipboard, Cmd+C still copies |  |
| Config: padding, theme, keybind, colors, title, copy-on-select | pass | title stays Work |  |
| Shell integration, prompt jump, output selection | auto-pass | LAPLACE_SHELL_INTEGRATION_DIR set | /Users/linh.nguyen/.cache/laplace/shell-integration/0.3.0 |
| Shell integration, prompt jump, output selection | pass | prompt jump |  |
| Shell integration, prompt jump, output selection | pass | Cmd+Shift+O selects ls output |  |
| Shell integration, prompt jump, output selection | pass | Cmd+T inherits cwd |  |
| Shell integration, prompt jump, output selection | pass | bar cursor at prompt, block in vim, title shows dir then command |  |
| Shell integration, prompt jump, output selection | pass | integration off |  |
| Shell integration, prompt jump, output selection | pass | bash: profile loads, prompt jump works |  |
| Drag & drop, scrollbar, secure input, bell-features | auto-pass | (drop the file) | '/Users/linh.nguyen/Downloads/sample_A3 2.pdf'  |
| Drag & drop, scrollbar, secure input, bell-features | pass | thin thumb at the right edge, gone at the bottom |  |
| Drag & drop, scrollbar, secure input, bell-features | pass | 🔒 in title, survives app switch, toggles off |  |
| Drag & drop, scrollbar, secure input, bell-features | pass | alert sound |  |
| Drag & drop, scrollbar, secure input, bell-features | pass | no Dock bounce |  |
| Multiple windows | pass | Cmd+N opens in /tmp |  |
| Multiple windows | pass | windows independent, both update |  |
| Multiple windows | pass | Cmd+W / exit / red button closes only that window |  |
| Multiple windows | pass | closing last window keeps Dock icon, click opens a window |  |
| Multiple windows | pass | split, tabs, find, reload, theme switch work in every window |  |
| Multiple windows | pass | keybind new_window |  |
| scrollback-limit, unfocused splits, minimum-contrast | pass | inactive split faded, single pane never faded |  |
| scrollback-limit, unfocused splits, minimum-contrast | pass | stronger fade toward dark red |  |
| scrollback-limit, unfocused splits, minimum-contrast | pass | all text pure black/white |  |
| scrollback-limit, unfocused splits, minimum-contrast | pass | readable |  |
| scrollback-limit, unfocused splits, minimum-contrast | pass | history starts around 900 |  |
| Cell size, font-thicken, underline | pass | more spacing, centered, box drawing connects (tree/htop) |  |
| Cell size, font-thicken, underline | pass | wider columns |  |
| Cell size, font-thicken, underline | pass | bolder text |  |
| Cell size, font-thicken, underline | pass | flatter wave, double lines apart |  |
| Scrollbar drag, progress bar, bell border/audio | pass | drag follows, top shows 1, bottom is live |  |
| Scrollbar drag, progress bar, bell border/audio | pass | blue bar over left 40% of the top edge |  |
| Scrollbar drag, progress bar, bell border/audio | pass | red 70% |  |
| Scrollbar drag, progress bar, bell border/audio | pass | yellow 50% |  |
| Scrollbar drag, progress bar, bell border/audio | pass | full dim blue |  |
| Scrollbar drag, progress bar, bell border/audio | pass | bar gone |  |
| Scrollbar drag, progress bar, bell border/audio | pass | amber outline, cleared on key/refocus |  |
| Scrollbar drag, progress bar, bell border/audio | pass | quiet Glass sound |  |
| Shell integration additions | pass | marks survive resize |  |
| Shell integration additions | pass | bash 3.2 output selection |  |
| Shell integration additions | skip | nushell | not installed |
| Shell integration additions | skip | elvish | not installed |
| Bundled themes | auto-pass | +list-themes count | 861 |
| Bundled themes | auto-pass | gruvbox themes listed | 7 |
| Bundled themes | pass | Gruvbox incl. cursor & selection |  |
| Bundled themes | pass | black background, rest Dracula |  |
| Bundled themes | pass | old alias works |  |
| confirm-close-surface | pass | alert; Cancel keeps, Close closes |  |
| confirm-close-surface | pass | idle prompt Cmd+W closes without asking |  |
| confirm-close-surface | pass | Cmd+Q asks 'Quit Laplace?', red button asks 'Close this window?' |  |
| confirm-close-surface | pass | false never asks, always always asks |  |
| macOS menu bar | pass | Laplace/Shell/Edit/View/Window menus, every item works |  |
| macOS menu bar | pass | Secure Keyboard Entry menu item toggles ✓ and 🔒 |  |
| macOS menu bar | pass | Cmd+H, Ctrl+Cmd+F, Minimize, Window menu lists windows |  |
| macOS menu bar | pass | close all windows, Shell ▸ New Window opens one |  |
| Kitty Caps Lock | AUTO-FAIL | Ctrl+A with Caps Lock ON | got: <nothing> want: ^\[\[97;69u |
| Kitty Caps Lock | AUTO-FAIL | Ctrl+A with Caps Lock OFF | got: p want: ^\[\[97;5u |
| Terminal inspector | pass | panel shows sections, toggles off |  |
| Terminal inspector | pass | Cell updates live |  |
| Terminal inspector | pass | Last key shows bytes |  |
| Terminal inspector | pass | vim → alternate / app-cursor on, sleep → running |  |
| Native macOS tabs | pass | Cmd+T native tab in cwd |  |
| Native macOS tabs | pass | Cmd+Shift+[ ] and Cmd+1/2 switch, Cmd+W closes one |  |
| Native macOS tabs | pass | without option: own tab bar |  |
| Quick Terminal (macOS) | pass | drops in from top with focus |  |
| Quick Terminal (macOS) | pass | hides, focus returns, same shell when reopened |  |
| Quick Terminal (macOS) | pass | autohide on click elsewhere |  |
| Quick Terminal (macOS) | pass | exit closes, hotkey opens fresh |  |
| Custom shaders | pass | inverted, upright, mouse lines up |  |
| Custom shaders | pass | scanlines animate only while focused |  |
| Custom shaders | pass | static |  |
| Custom shaders | pass | broken shader skipped |  |
| OSC 4 recolors existing text | pass | earlier red and ls colors turned green |  |
| OSC 4 recolors existing text | pass | red again |  |
| Ligatures and font-feature | pass | ligatures, each over its own cells |  |
| Ligatures and font-feature | pass | bold and italic ligatures |  |
| Ligatures and font-feature | pass | half-selecting a ligature, cursor over it: no garbage |  |
| Ligatures and font-feature | pass | ligatures off |  |
| Ligatures and font-feature | pass | default font looks as before |  |
| Inline images (Kitty, Sixel) | pass | gradient image at native size |  |
| Inline images (Kitty, Sixel) | pass | sized to 20x10 cells |  |
| Inline images (Kitty, Sixel) | pass | sixel stripes, prompt continues below |  |
| Inline images (Kitty, Sixel) | pass | moves with text, clipped at pane edges |  |
| Inline images (Kitty, Sixel) | pass | stays next to same text after resize |  |
| Inline images (Kitty, Sixel) | pass | still there after vim |  |
| Inline images (Kitty, Sixel) | pass | clear removes images |  |
| Color emoji and grapheme clusters | pass | all color, each cluster one glyph 2 cells, text after aligned |  |
| Color emoji and grapheme clusters | AUTO-FAIL | clipboard round trip | got: img png; echo want: 😀 👍🏽 👨‍👩‍👧 🇻🇳 ❤️ #️⃣ 🏳️‍🌈 |
| Color emoji and grapheme clusters | pass | legacy: three separate faces |  |
| Color emoji and grapheme clusters | pass | marks on their base letters |  |
| Color emoji and grapheme clusters | pass | bold / inverse emoji still in color |  |
| Kitty graphics: z-index, placeholders, animation, shm | pass | image behind text, red cells cover it |  |
| Kitty graphics: z-index, placeholders, animation, shm | pass | image behind colored cells too |  |
| Kitty graphics: z-index, placeholders, animation, shm | pass | placeholder image 10x4 cells |  |
| Kitty graphics: z-index, placeholders, animation, shm | pass | red→blue→green animation, ~0% CPU when scrolled away |  |
| Kitty graphics: z-index, placeholders, animation, shm | pass | image via shared memory |  |
| Right-to-left text | pass | Hebrew right to left, rest unchanged |  |
| Right-to-left text | pass | Arabic letters joined, RTL order |  |
| Right-to-left text | pass | click/drag in Hebrew selects cell under mouse, copy logical order |  |
| Right-to-left text | pass | cursor at the next letter's visual position |  |
| Kitty lock modifiers (Linux) | skip | all | not on Linux |
| Quick Terminal (Linux) | skip | all | not on Linux |
| Debt: Kitty keys in the real app | AUTO-FAIL | a | got: ^M want: a |
| Debt: Kitty keys in the real app | auto-pass | Ctrl+A | ^[[97;5u |
| Debt: Kitty keys in the real app | auto-pass | Alt+A (Option) | ^[[97;3u |
| Debt: Kitty keys in the real app | auto-pass | Esc | ^[[27u |
| Debt: Kitty keys in the real app | auto-pass | Tab | 	 |
| Debt: Kitty keys in the real app | auto-pass | Ctrl+I | ^[[105;5u |
| Debt: Kitty keys in the real app | pass | key repeat (hold a key in vim/nvim) behaves |  |
| Debt: OSC 8 hover | pass | Cmd + mouse move underlines it, preview pill shows the URL |  |
| Debt: OSC 8 hover | pass | Cmd+Click opens it |  |

## 2026-10-27 09:30 — triage by Claude

| Group | Status | Item | Detail |
|---|---|---|---|
| Multiple windows | FIXED (uncommitted) | Cmd+N "does nothing" | winit centers every new window, so it opened exactly on top of the current one; now cascaded 28 pt down-right from it |
| modifyOtherKeys | RETEST | Ctrl+Enter got ^M | encoder unit-tested (`\e[27;5;13~`); a stray Enter left from the verdict prompt was read first — script now drains input |
| modifyOtherKeys | RETEST | Cmd+J got `\e[106;5u` | 5 = Ctrl, so Ctrl+J was pressed; Cmd encodes as 9 |
| XTGETTCAP… | RETEST | Appearance switch got ^M | same stray Enter |
| Kitty Caps Lock | RETEST | ON: nothing in 15 s, OFF: got "p" | toggling Caps Lock inside the 15 s window; script now asks to toggle first |
| Emoji | RETEST | clipboard held "img png; echo" | the emoji output line was never copied (clipboard still had the group 23 selection) |
| Debt: Kitty keys | RETEST | a got ^M | same stray Enter |
| Build | auto-pass | test / clippy / Linux check / release | 183 passed, no new warnings |
| Multiple windows | pass | Cmd+N opens a separate window | after fix: new windows created hidden with tabbing disallowed, cascaded from the current one |

## 2026-10-27 09:39 (4524de2)

| Group | Status | Item | Detail |
|---|---|---|---|
| modifyOtherKeys + Cmd/Super for Kitty | auto-pass | Ctrl+Enter | ^[[27;5;13~ |
| modifyOtherKeys + Cmd/Super for Kitty | auto-pass | Ctrl+1 | ^[[27;5;49~ |
| modifyOtherKeys + Cmd/Super for Kitty | auto-pass | Ctrl+A | ^[[27;5;97~ |
| modifyOtherKeys + Cmd/Super for Kitty | auto-pass | Ctrl+A (mode 1) | ^A |
| modifyOtherKeys + Cmd/Super for Kitty | auto-pass | Ctrl+1 (mode 1) | ^[[27;5;49~ |
| modifyOtherKeys + Cmd/Super for Kitty | auto-pass | ⌘+J — Command, not Ctrl (kitty flags 1) | ^[[106;9u |
| modifyOtherKeys + Cmd/Super for Kitty | auto-pass | Cmd+J (no mode, wait 4 s) | <nothing> |
| XTGETTCAP, DECRQSS, pixel mouse, color scheme | auto-pass | XTGETTCAP TN | ^[P1+r544E=787465726D2D323536636F6C6F72^[\ |
| XTGETTCAP, DECRQSS, pixel mouse, color scheme | auto-pass | DECRQSS SGR | ^[P1$r0;1;38;2;233;86;120m^[\ |
| XTGETTCAP, DECRQSS, pixel mouse, color scheme | auto-pass | DA1 advertises sixel | ^[[?62;4;22c |
| XTGETTCAP, DECRQSS, pixel mouse, color scheme | auto-pass | XTWINOPS 18t | ^[[8;41;143t |
| XTGETTCAP, DECRQSS, pixel mouse, color scheme | auto-pass | XTWINOPS 14t | ^[[4;1476;2717t |
| XTGETTCAP, DECRQSS, pixel mouse, color scheme | auto-pass | DSR 996 color scheme | ^[[?997;1n |
| XTGETTCAP, DECRQSS, pixel mouse, color scheme | auto-pass | (move the mouse over the window) | ^[[<35;649;935M |
| XTGETTCAP, DECRQSS, pixel mouse, color scheme | pass | pixel coordinates |  |
| XTGETTCAP, DECRQSS, pixel mouse, color scheme | AUTO-FAIL | (switch macOS Appearance Light↔Dark) | got: ^M want: ^\[\[?997;[12]n |
| XTGETTCAP, DECRQSS, pixel mouse, color scheme | pass | nvim health |  |
| Multiple windows | pass | Cmd+N opens in /tmp |  |
| Multiple windows | pass | windows independent, both update |  |

## 2026-10-27 09:41 (4524de2)

| Group | Status | Item | Detail |
|---|---|---|---|
| modifyOtherKeys + Cmd/Super for Kitty | auto-pass | Ctrl+Enter | ^[[27;5;13~ |
| modifyOtherKeys + Cmd/Super for Kitty | auto-pass | Ctrl+1 | ^[[27;5;49~ |
| modifyOtherKeys + Cmd/Super for Kitty | auto-pass | Ctrl+A | ^[[27;5;97~ |
| modifyOtherKeys + Cmd/Super for Kitty | auto-pass | Ctrl+A (mode 1) | ^A |
| modifyOtherKeys + Cmd/Super for Kitty | auto-pass | Ctrl+1 (mode 1) | ^[[27;5;49~ |
| modifyOtherKeys + Cmd/Super for Kitty | auto-pass | ⌘+J — Command, not Ctrl (kitty flags 1) | ^[[106;9u |
| modifyOtherKeys + Cmd/Super for Kitty | auto-pass | Cmd+J (no mode, wait 4 s) | <nothing> |
| XTGETTCAP, DECRQSS, pixel mouse, color scheme | auto-pass | XTGETTCAP TN | ^[P1+r544E=787465726D2D323536636F6C6F72^[\ |
| XTGETTCAP, DECRQSS, pixel mouse, color scheme | auto-pass | DECRQSS SGR | ^[P1$r0;1;38;2;233;86;120m^[\ |
| XTGETTCAP, DECRQSS, pixel mouse, color scheme | auto-pass | DA1 advertises sixel | ^[[?62;4;22c |
| XTGETTCAP, DECRQSS, pixel mouse, color scheme | auto-pass | XTWINOPS 18t | ^[[8;41;143t |
| XTGETTCAP, DECRQSS, pixel mouse, color scheme | auto-pass | XTWINOPS 14t | ^[[4;1476;2717t |
| XTGETTCAP, DECRQSS, pixel mouse, color scheme | auto-pass | DSR 996 color scheme | ^[[?997;1n |
| XTGETTCAP, DECRQSS, pixel mouse, color scheme | auto-pass | (move the mouse over the window) | ^[[<35;929;976M |
| XTGETTCAP, DECRQSS, pixel mouse, color scheme | pass | pixel coordinates |  |
| XTGETTCAP, DECRQSS, pixel mouse, color scheme | AUTO-FAIL | (switch macOS Appearance Light↔Dark) | got: ^M want: ^\[\[?997;[12]n |
| XTGETTCAP, DECRQSS, pixel mouse, color scheme | pass | nvim health |  |
| Multiple windows | pass | Cmd+N opens in /tmp |  |
| Multiple windows | pass | windows independent, both update |  |
| Multiple windows | pass | Cmd+W / exit / red button closes only that window |  |
| Multiple windows | pass | closing last window keeps Dock icon, click opens a window |  |
| Multiple windows | pass | split, tabs, find, reload, theme switch work in every window |  |
| Multiple windows | pass | keybind new_window |  |
| Kitty Caps Lock | AUTO-FAIL | Ctrl+A (Caps Lock ON) | got: ^[[97;5u want: ^\[\[97;69u |
| Kitty Caps Lock | auto-pass | Ctrl+A (Caps Lock OFF) | ^[[97;5u |
| Color emoji and grapheme clusters | pass | all color, each cluster one glyph 2 cells, text after aligned |  |
| Color emoji and grapheme clusters | AUTO-FAIL | clipboard round trip | got: ▶ press (move the mouse over the window) want: 😀 👍🏽 👨‍👩‍👧 🇻🇳 ❤️ #️⃣ 🏳️‍🌈 |
| Color emoji and grapheme clusters | pass | legacy: three separate faces |  |
| Color emoji and grapheme clusters | pass | marks on their base letters |  |
| Color emoji and grapheme clusters | pass | bold / inverse emoji still in color |  |
| Debt: Kitty keys in the real app | auto-pass | a | a |
| Debt: Kitty keys in the real app | auto-pass | Ctrl+A | ^[[97;5u |
| Debt: Kitty keys in the real app | auto-pass | Alt+A (Option) | ^[[97;3u |
| Debt: Kitty keys in the real app | auto-pass | Esc | ^[[27u |
| Debt: Kitty keys in the real app | auto-pass | Tab | 	 |
| Debt: Kitty keys in the real app | auto-pass | Ctrl+I | ^[[105;5u |
| Debt: Kitty keys in the real app | pass | key repeat (hold a key in vim/nvim) behaves |  |

## 2026-10-27 09:55 — triage by Claude

| Group | Status | Item | Detail |
|---|---|---|---|
| modifyOtherKeys | pass | all keys | retest after draining stray input |
| Multiple windows | pass | all | Cmd+N separate window fixed |
| Debt: Kitty keys | pass | all | |
| XTGETTCAP… | FIXED (uncommitted) | Appearance switch sent nothing | windows set their own NSAppearance, so winit never emits ThemeChanged; now observes AppleInterfaceThemeChangedNotification |
| Kitty Caps Lock | FIXED? (uncommitted) | Caps ON still sent 97;5u | probes outside the app saw Caps Lock (also with Ctrl held); switched caps_lock() to CGEventSourceFlagsState(HID) — needs retest |
| Emoji | OPEN | clipboard unchanged after copying the emoji line | pbcopy round trip OK, selected_text unit test OK (emoji_clusters_copy_back_exactly); needs an in-app check |

## 2026-10-27 09:54 (4524de2)

| Group | Status | Item | Detail |
|---|---|---|---|
| XTGETTCAP, DECRQSS, pixel mouse, color scheme | auto-pass | XTGETTCAP TN | ^[P1+r544E=787465726D2D323536636F6C6F72^[\ |
| XTGETTCAP, DECRQSS, pixel mouse, color scheme | auto-pass | DECRQSS SGR | ^[P1$r0;1;38;2;233;86;120m^[\ |
| XTGETTCAP, DECRQSS, pixel mouse, color scheme | auto-pass | DA1 advertises sixel | ^[[?62;4;22c |
| XTGETTCAP, DECRQSS, pixel mouse, color scheme | auto-pass | XTWINOPS 18t | ^[[8;41;143t |
| XTGETTCAP, DECRQSS, pixel mouse, color scheme | auto-pass | XTWINOPS 14t | ^[[4;1476;2717t |
| XTGETTCAP, DECRQSS, pixel mouse, color scheme | auto-pass | DSR 996 color scheme | ^[[?997;1n |
| XTGETTCAP, DECRQSS, pixel mouse, color scheme | auto-pass | (move the mouse over the window) | ^[[<35;661;879M |
| XTGETTCAP, DECRQSS, pixel mouse, color scheme | pass | pixel coordinates |  |
| XTGETTCAP, DECRQSS, pixel mouse, color scheme | AUTO-FAIL | (switch macOS Appearance Light↔Dark) | got: ^M want: ^\[\[?997;[12]n |
| XTGETTCAP, DECRQSS, pixel mouse, color scheme | pass | nvim health |  |
| Kitty Caps Lock | auto-pass | Ctrl+A (Caps Lock ON) | ^[[97;69u |
| Kitty Caps Lock | auto-pass | Ctrl+A (Caps Lock OFF) | ^[[97;5u |
| Emoji | pass | clipboard round trip (manual) | selected the echo output, Cmd+C, typed pbpaste \| xxd: bytes identical to the source string |

## 2026-10-27 09:56 (4524de2)

| Group | Status | Item | Detail |
|---|---|---|---|
| XTGETTCAP, DECRQSS, pixel mouse, color scheme | auto-pass | XTGETTCAP TN | ^[P1+r544E=787465726D2D323536636F6C6F72^[\ |
| XTGETTCAP, DECRQSS, pixel mouse, color scheme | auto-pass | DECRQSS SGR | ^[P1$r0;1;38;2;233;86;120m^[\ |
| XTGETTCAP, DECRQSS, pixel mouse, color scheme | auto-pass | DA1 advertises sixel | ^[[?62;4;22c |
| XTGETTCAP, DECRQSS, pixel mouse, color scheme | auto-pass | XTWINOPS 18t | ^[[8;41;143t |
| XTGETTCAP, DECRQSS, pixel mouse, color scheme | auto-pass | XTWINOPS 14t | ^[[4;1476;2717t |
| XTGETTCAP, DECRQSS, pixel mouse, color scheme | auto-pass | DSR 996 color scheme | ^[[?997;1n |
| XTGETTCAP, DECRQSS, pixel mouse, color scheme | auto-pass | (move the mouse over the window) | ^[[<35;666;1031M |
| XTGETTCAP, DECRQSS, pixel mouse, color scheme | pass | pixel coordinates |  |
| XTGETTCAP, DECRQSS, pixel mouse, color scheme | AUTO-FAIL | (switch macOS Appearance Light↔Dark) | got: ^M want: ^\[\[?997;[12]n |
| XTGETTCAP, DECRQSS, pixel mouse, color scheme | pass | nvim health |  |
| Kitty Caps Lock | auto-pass | Ctrl+A (Caps Lock ON) | ^[[97;69u |
| Kitty Caps Lock | auto-pass | Ctrl+A (Caps Lock OFF) | ^[[97;5u |

## 2026-10-27 10:05 — triage by Claude

| Group | Status | Item | Detail |
|---|---|---|---|
| Kitty Caps Lock | pass | ON / OFF | 09:54 and 09:56 runs |
| XTGETTCAP… | FIXED? (uncommitted) | Appearance switch still sent nothing | observer now DeliverImmediately (AppKit suspends distributed notifications while inactive), value read via CFPreferences after sync, re-checked on app activation — needs retest |

## 2026-10-27 10:06 (f9a0b42)

| Group | Status | Item | Detail |
|---|---|---|---|
| XTGETTCAP, DECRQSS, pixel mouse, color scheme | auto-pass | XTGETTCAP TN | ^[P1+r544E=787465726D2D323536636F6C6F72^[\ |
| XTGETTCAP, DECRQSS, pixel mouse, color scheme | auto-pass | DECRQSS SGR | ^[P1$r0;1;38;2;233;86;120m^[\ |
| XTGETTCAP, DECRQSS, pixel mouse, color scheme | auto-pass | DA1 advertises sixel | ^[[?62;4;22c |
| XTGETTCAP, DECRQSS, pixel mouse, color scheme | auto-pass | XTWINOPS 18t | ^[[8;41;143t |
| XTGETTCAP, DECRQSS, pixel mouse, color scheme | auto-pass | XTWINOPS 14t | ^[[4;1476;2717t |
| XTGETTCAP, DECRQSS, pixel mouse, color scheme | auto-pass | DSR 996 color scheme | ^[[?997;1n |
| XTGETTCAP, DECRQSS, pixel mouse, color scheme | AUTO-FAIL | (move the mouse over the window) | got: ^M want: ^\[\[<35;*M* |
| XTGETTCAP, DECRQSS, pixel mouse, color scheme | pass | pixel coordinates |  |
| XTGETTCAP, DECRQSS, pixel mouse, color scheme | AUTO-FAIL | (switch macOS Appearance Light↔Dark) | got: ^M want: ^\[\[?997;[12]n |
| XTGETTCAP, DECRQSS, pixel mouse, color scheme | pass | nvim health |  |

## 2026-10-27 10:08 (f9a0b42)

| Group | Status | Item | Detail |
|---|---|---|---|
| XTGETTCAP, DECRQSS, pixel mouse, color scheme | auto-pass | XTGETTCAP TN | ^[P1+r544E=787465726D2D323536636F6C6F72^[\ |
| XTGETTCAP, DECRQSS, pixel mouse, color scheme | auto-pass | DECRQSS SGR | ^[P1$r0;1;38;2;233;86;120m^[\ |
| XTGETTCAP, DECRQSS, pixel mouse, color scheme | auto-pass | DA1 advertises sixel | ^[[?62;4;22c |
| XTGETTCAP, DECRQSS, pixel mouse, color scheme | auto-pass | XTWINOPS 18t | ^[[8;41;143t |
| XTGETTCAP, DECRQSS, pixel mouse, color scheme | auto-pass | XTWINOPS 14t | ^[[4;1476;2717t |
| XTGETTCAP, DECRQSS, pixel mouse, color scheme | auto-pass | DSR 996 color scheme | ^[[?997;1n |
| XTGETTCAP, DECRQSS, pixel mouse, color scheme | auto-pass | (move the mouse over the window) | ^[[<35;1196;976M^[[<35;1160;1008M |
| XTGETTCAP, DECRQSS, pixel mouse, color scheme | pass | pixel coordinates |  |
| XTGETTCAP, DECRQSS, pixel mouse, color scheme | AUTO-FAIL | (switch macOS Appearance Light↔Dark) | got: ^M want: ^\[\[?997;[12]n |
| XTGETTCAP, DECRQSS, pixel mouse, color scheme | pass | nvim health |  |

## 2026-10-27 10:09 (f9a0b42)

| Group | Status | Item | Detail |
|---|---|---|---|
| XTGETTCAP, DECRQSS, pixel mouse, color scheme | auto-pass | XTGETTCAP TN | ^[P1+r544E=787465726D2D323536636F6C6F72^[\ |
| XTGETTCAP, DECRQSS, pixel mouse, color scheme | auto-pass | DECRQSS SGR | ^[P1$r0;1;38;2;233;86;120m^[\ |
| XTGETTCAP, DECRQSS, pixel mouse, color scheme | auto-pass | DA1 advertises sixel | ^[[?62;4;22c |
| XTGETTCAP, DECRQSS, pixel mouse, color scheme | auto-pass | XTWINOPS 18t | ^[[8;41;143t |
| XTGETTCAP, DECRQSS, pixel mouse, color scheme | auto-pass | XTWINOPS 14t | ^[[4;1476;2717t |
| XTGETTCAP, DECRQSS, pixel mouse, color scheme | auto-pass | DSR 996 color scheme | ^[[?997;2n |
| XTGETTCAP, DECRQSS, pixel mouse, color scheme | AUTO-FAIL | (move the mouse over the window) | got: ^M want: ^\[\[<35;*M* |
| XTGETTCAP, DECRQSS, pixel mouse, color scheme | pass | pixel coordinates |  |
| XTGETTCAP, DECRQSS, pixel mouse, color scheme | auto-pass | (switch macOS Appearance Light↔Dark) | ^[[?997;1n |
| XTGETTCAP, DECRQSS, pixel mouse, color scheme | pass | nvim health |  |
| XTGETTCAP… | pass | Appearance switch (manual run) | ^[[?997;1n received; came from the activation re-check (debug log: AppleInterfaceThemeChangedNotification never reached the app). Notification replaced by KVO on NSApp.effectiveAppearance — retest pending |

## 2026-10-27 10:12 (f9a0b42)

| Group | Status | Item | Detail |
|---|---|---|---|
| XTGETTCAP, DECRQSS, pixel mouse, color scheme | auto-pass | XTGETTCAP TN | ^[P1+r544E=787465726D2D323536636F6C6F72^[\ |
| XTGETTCAP, DECRQSS, pixel mouse, color scheme | auto-pass | DECRQSS SGR | ^[P1$r0;1;38;2;233;86;120m^[\ |
| XTGETTCAP, DECRQSS, pixel mouse, color scheme | auto-pass | DA1 advertises sixel | ^[[?62;4;22c |
| XTGETTCAP, DECRQSS, pixel mouse, color scheme | auto-pass | XTWINOPS 18t | ^[[8;41;143t |
| XTGETTCAP, DECRQSS, pixel mouse, color scheme | auto-pass | XTWINOPS 14t | ^[[4;1476;2717t |
| XTGETTCAP, DECRQSS, pixel mouse, color scheme | auto-pass | DSR 996 color scheme | ^[[?997;1n |
| XTGETTCAP, DECRQSS, pixel mouse, color scheme | auto-pass | (move the mouse over the window) | ^[[<35;1024;765M |
| XTGETTCAP, DECRQSS, pixel mouse, color scheme | pass | pixel coordinates |  |
| XTGETTCAP, DECRQSS, pixel mouse, color scheme | auto-pass | (switch macOS Appearance Light↔Dark) | ^[[?997;2n |
| XTGETTCAP, DECRQSS, pixel mouse, color scheme | pass | nvim health |  |
