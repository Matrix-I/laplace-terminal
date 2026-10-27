#!/bin/bash
# Interactive manual test runner for the "Pending manual test" items in ROADMAP.md.
# Run it inside a Laplace window:
#   bash scripts/manual-test.sh          # every group
#   bash scripts/manual-test.sh 2 7 24   # only these groups
#   bash scripts/manual-test.sh -l       # list the groups
# Terminal replies and key presses are checked automatically where possible;
# everything else asks for a verdict. Results are appended to
# scripts/manual-test-results.md.

REPO=$(cd "$(dirname "$0")/.." && pwd)
LOG=${LAPLACE_TEST_LOG:-$REPO/scripts/manual-test-results.md}
BIN=$REPO/target/release/laplace
export LC_ALL=en_US.UTF-8
OS=$(uname -s)

B=$'\e[1m' D=$'\e[2m' R=$'\e[0m' G=$'\e[32m' RED=$'\e[31m' Y=$'\e[33m' C=$'\e[36m'
GROUP=""

# ---------- helpers ----------

say() { printf '%s\n' "  $*"; }
note() { printf '%s\n' "  ${D}$*${R}"; }
# Show a command, then run it.
run() { printf '  %s$ %s%s\n' "$C" "$1" "$R"; eval "$1"; }
# Show text to copy (config lines, commands for another tab).
paste_box() {
    printf '  %s┌─ copy ──────────────────────────────────────────%s\n' "$Y" "$R"
    while IFS= read -r l; do printf '  %s│%s %s\n' "$Y" "$R" "$l"; done
    printf '  %s└─────────────────────────────────────────────────%s\n' "$Y" "$R"
}
pause() { printf '  %s[Enter] %s%s' "$D" "${1:-continue}" "$R"; IFS= read -r _; }

log() { # status, item, detail
    printf '| %s | %s | %s | %s |\n' "$GROUP" "$1" "$2" "${3//|/\\|}" >>"$LOG"
}
auto() { # item, actual, expected-glob
    if [[ $2 == $3 ]]; then
        printf '  %s✔ %s%s %s\n' "$G" "$1" "$R" "$D$2$R"; log auto-pass "$1" "$2"
    else
        printf '  %s✘ %s%s got: %s  want: %s\n' "$RED" "$1" "$R" "$2" "$3"; log AUTO-FAIL "$1" "got: $2 want: $3"
    fi
}
verdict() { # item
    local a n
    while :; do
        printf '  %s%s?%s [p]ass [f]ail [s]kip ' "$B" "$1" "$R"; IFS= read -r a
        case $a in
            p) log pass "$1" ""; return ;;
            f) printf '  what went wrong? '; IFS= read -r n; log FAIL "$1" "$n"; return ;;
            s) log skip "$1" ""; return ;;
        esac
    done
}

# Send a query and collect the reply until $2 appears (or 1 s of silence).
query() {
    local old chunk r=""
    old=$(stty -g)
    stty raw -echo min 0 time 10
    printf '%b' "$1" >/dev/tty
    while chunk=$(dd bs=4096 count=1 2>/dev/null) && [ -n "$chunk" ]; do
        r+=$chunk
        [[ $r == *"$2"* ]] && break
    done
    stty "$old"
    REPLY_V=$(printf '%s' "$r" | cat -v)
}

# Wait up to $2 seconds for one key press and compare it with $3 (cat -v form).
key() { # prompt, seconds, expected-glob
    local old
    printf '  %s▶ press %s%s ' "$B" "$1" "$R"
    old=$(stty -g)
    # Drop anything typed earlier (a second Enter after a verdict).
    stty raw -echo min 0 time 0
    while [ -n "$(dd bs=1024 count=1 2>/dev/null)" ]; do :; done
    stty raw -echo min 0 time $(($2 * 10))
    KEY_V=$(dd bs=1024 count=1 2>/dev/null | cat -v)
    stty "$old"
    printf '\n'
    auto "$1" "${KEY_V:-<nothing>}" "$3"
}

# Kitty graphics / Sixel generators (stdout is the terminal).
img() {
    python3 -I - "$@" <<'PY'
import base64, struct, sys, zlib, os
out = sys.stdout.buffer

def png(w, h):
    rows = b"".join(b"\0" + bytes(c for x in range(w) for c in (x * 255 // w, y * 255 // h, 160, 255)) for y in range(h))
    chunk = lambda t, d: struct.pack(">I", len(d)) + t + d + struct.pack(">I", zlib.crc32(t + d) & 0xFFFFFFFF)
    return b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", struct.pack(">IIBBBBB", w, h, 8, 6, 0, 0, 0)) + chunk(b"IDAT", zlib.compress(rows)) + chunk(b"IEND", b"")

def send(params, data):
    enc = base64.b64encode(data)
    parts = [enc[i:i + 4096] for i in range(0, len(enc), 4096)] or [b""]
    for i, p in enumerate(parts):
        head = (params + "," if i == 0 else "") + "m=%d" % (i < len(parts) - 1)
        out.write(b"\x1b_G" + head.encode() + b";" + p + b"\x1b\\")

def solid(w, h, rgba):
    return bytes(rgba) * (w * h)

mode = sys.argv[1]
if mode == "png":
    send("a=T,f=100,q=2", png(160, 80))
elif mode == "cells":
    send("a=T,f=100,c=20,r=10,q=2", png(160, 80))
elif mode == "under":
    send("a=T,f=100,z=-1,c=30,r=4,q=2", png(160, 80))
elif mode == "deep":
    send("a=T,f=100,z=-1073741825,c=30,r=4,q=2", png(160, 80))
elif mode == "anim":
    send("a=T,i=77,f=32,s=32,v=32,q=2", solid(32, 32, (220, 40, 40, 255)))
    send("a=f,i=77,f=32,s=32,v=32,z=400,q=2", solid(32, 32, (40, 80, 220, 255)))
    send("a=f,i=77,f=32,s=32,v=32,z=400,q=2", solid(32, 32, (40, 200, 80, 255)))
    out.write(b"\x1b_Ga=a,i=77,r=1,z=400,q=2\x1b\\\x1b_Ga=a,i=77,s=3,v=1,q=2\x1b\\")
elif mode == "shm":
    from multiprocessing import shared_memory, resource_tracker
    data = png(160, 80)
    shm = shared_memory.SharedMemory(create=True, size=len(data), name="lpl%d" % os.getpid())
    shm.buf[:len(data)] = data
    resource_tracker.unregister(shm._name, "shared_memory")  # the terminal unlinks it
    send("a=T,f=100,t=s,S=%d,q=2" % len(data), shm._name.encode())
    shm.close()
elif mode == "placeholder":
    send("a=t,i=42,f=100,q=2", png(160, 80))
    out.write(b"\x1b_Ga=p,U=1,i=42,c=10,r=4,q=2\x1b\\")
    dia = [0x0305, 0x030D, 0x030E, 0x0310, 0x0312, 0x033D, 0x033E, 0x033F, 0x0346, 0x034A]
    for r in range(4):
        line = "".join("\U0010EEEE" + chr(dia[r]) + chr(dia[c]) for c in range(10))
        out.write(("\x1b[38;5;42m" + line + "\x1b[39m\n").encode())
elif mode == "sixel":
    s = '\x1bPq"1;1;96;48#0;2;100;20;20#1;2;20;90;30#2;2;20;40;100'
    s += "".join("#%d!96~-" % (b % 3) for b in range(8))
    out.write((s + "\x1b\\\n").encode())
out.flush()
PY
}

# ---------- groups ----------

g1() { GROUP="OSC 4/10/11/12"
    run "printf '\e]11;#1e1e2e\a\e]10;#cdd6f4\a'"; verdict "background dark purple, text light blue"
    query '\e]11;?\a' $'\a'; auto "OSC 11 query" "$REPLY_V" '*11;rgb:1e1e/1e1e/2e2e*'
    run "printf '\e]12;#f38ba8\a'"; verdict "cursor pink"
    run "printf '\e]4;1;#00ff00\a\e[31mred-now-green\e[0m\n'"; verdict "text is green"
    query '\e]4;1;?\a' $'\a'; auto "OSC 4 query" "$REPLY_V" '*4;1;rgb:0000/ffff/0000*'
    run "printf '\e]11;midnight blue\a'"; verdict "background navy (X11 name)"
    run "printf '\e]11;#ffffff\a'"; say "now run: nvim -c 'set bg?'  → background=light, then :q"; pause; verdict "nvim sees background=light"
    run "printf '\e]110\a\e]111\a\e]112\a\e]104\a'"; verdict "all colors back to config"
    say "Cmd+D, in ONE split run:  printf '\e]11;#402020\a'  then switch focus back and forth"; pause; verdict "other split keeps its colors"
}

g2() { GROUP="modifyOtherKeys + Cmd/Super for Kitty"
    printf '\e[>4;2m'
    key "Ctrl+Enter" 10 '^\[\[27;5;13~'; key "Ctrl+1" 10 '^\[\[27;5;49~'; key "Ctrl+A" 10 '^\[\[27;5;97~'
    printf '\e[>4;1m'
    key "Ctrl+A (mode 1)" 10 '^A'; key "Ctrl+1 (mode 1)" 10 '^\[\[27;5;49~'
    printf '\e[>4m'
    printf '\e[>1u'; key "⌘+J — Command, not Ctrl (kitty flags 1)" 10 '^\[\[106;9u'; printf '\e[<u'
    key "Cmd+J (no mode, wait 4 s)" 4 '<nothing>'
}

g3() { GROUP="Notifications, progress, pointer, bell"
    say "switch to another app within 3 s…"; sleep 3; printf '\e]9;Build finished\a'; verdict "notification 'Laplace / Build finished'"
    say "switch to another app within 3 s…"; sleep 3; printf '\e]777;notify;Tests;All passed\a'; verdict "notification titled 'Tests'"
    run "printf '\e]9;4;1;45\a'"; verdict "title starts with '45% '"
    run "printf '\e]9;4;3\a'"; verdict "title starts with '⋯ '"
    run "printf '\e]9;4;2;80\a'"; verdict "title starts with '✖ 80% '"
    run "printf '\e]9;4;0\a'"; verdict "prefix gone"
    run "printf '\e]22;pointer\a'"; verdict "hand cursor over the terminal"
    run "printf '\e]22;text\a'"; verdict "I-beam"
    run "printf '\e]22;\a'"; verdict "arrow"
    say "open a 2nd tab, run this there, then switch to this tab:"; echo "sleep 2; printf '\a'" | paste_box; pause; verdict "that tab gets 🔔, cleared when you visit it"
    say "switch to another app within 3 s…"; sleep 3; printf '\a'; verdict "Laplace bounces in the Dock"
}

g4() { GROUP="XTGETTCAP, DECRQSS, pixel mouse, color scheme"
    query '\eP+q544E\e\\' '\'; auto "XTGETTCAP TN" "$REPLY_V" '*P1+r544E=787465726D2D323536636F6C6F72*'
    query '\e[1;31m\eP$qm\e\\\e[0m' '\'; auto "DECRQSS SGR" "$REPLY_V" '*1$r0;1;*m*'
    query '\e[c' 'c'; auto "DA1 advertises sixel" "$REPLY_V" '*\[?*;4*c'
    query '\e[18t' 't'; auto "XTWINOPS 18t" "$REPLY_V" "*\[8;$(tput lines);$(tput cols)t"
    query '\e[14t' 't'; auto "XTWINOPS 14t" "$REPLY_V" '*\[4;*;*t'
    query '\e[?996n' 'n'; auto "DSR 996 color scheme" "$REPLY_V" '*\[?997;[12]n'
    printf '\e[?1016h\e[?1003h'; key "(move the mouse over the window)" 10 '^\[\[<35;*M*'
    printf '\e[?1016l\e[?1003l'; note "cols=$(tput cols): the X above should be in pixels (hundreds)"; verdict "pixel coordinates"
    printf '\e[?2031h'; key "(switch macOS Appearance Light↔Dark)" 25 '^\[\[?997;[12]n'; printf '\e[?2031l'
    say "nvim: :checkhealth (no true color / undercurl warnings), :lua print(vim.o.termguicolors) → true"; pause; verdict "nvim health"
}

g5() { GROUP="Config: padding, theme, keybind, colors, title, copy-on-select"
    say "Add each block to ~/.laplace/config, Cmd+Shift+, to reload, check, then remove it."
    printf '%s\n' 'window-padding-x = 24' 'window-padding-y = 12' 'window-padding-balance = true' | paste_box; pause; verdict "wider margins, grid centered"
    echo 'theme = light:Horizon Bright,dark:Dracula' | paste_box; note "(your config sets background/foreground after theme: comment them out for this one)"; pause; verdict "Dracula; Light appearance → Horizon Bright live"
    printf '%s\n' 'keybind = cmd+t=new_split:right' 'keybind = cmd+d=unbind' 'keybind = ctrl+shift+h=text:hello\n' | paste_box; pause; verdict "Cmd+T splits, Cmd+D nothing, Ctrl+Shift+H types hello+Enter"
    echo 'keybind = clear' | paste_box; pause; verdict "defaults back"
    printf '%s\n' 'cursor-color = #ff8800' 'selection-background = #335533' 'selection-foreground = #ffff00' | paste_box; pause; verdict "orange cursor, green selection with yellow text"
    echo 'copy-on-select = false' | paste_box; pause; verdict "dragging doesn't touch clipboard, Cmd+C still copies"
    echo 'title = Work' | paste_box; pause; verdict "title stays Work"
}

g6() { GROUP="Shell integration, prompt jump, output selection"
    auto "LAPLACE_SHELL_INTEGRATION_DIR set" "${LAPLACE_SHELL_INTEGRATION_DIR:-<empty>}" '*/laplace/shell-integration/*'
    say "Quit this script (Ctrl+C) or do these in another tab:"
    printf '%s\n' 'seq 50; ls; seq 30' '# Cmd+Shift+↑ repeatedly, Cmd+Shift+↓ back to live' 'ls -la' '# Cmd+Shift+O → only the ls output is selected and copied' 'cd /tmp' '# Cmd+T → new tab in /tmp' | paste_box; pause
    verdict "prompt jump"; verdict "Cmd+Shift+O selects ls output"; verdict "Cmd+T inherits cwd"
    echo 'shell-integration-features = cursor,title' | paste_box; pause; verdict "bar cursor at prompt, block in vim, title shows dir then command"
    echo 'shell-integration = none' | paste_box; note "(new window, then echo \$LAPLACE_SHELL_INTEGRATION_DIR is empty)"; pause; verdict "integration off"
    echo "SHELL=/bin/bash $BIN" | paste_box; pause; verdict "bash: profile loads, prompt jump works"
}

g7() { GROUP="Drag & drop, scrollbar, secure input, bell-features"
    say "Drag a Finder file whose name has spaces into this window (wait 15 s)"
    key "(drop the file)" 15 "*'/*' *"
    run "seq 500"; say "scroll up with the trackpad"; pause; verdict "thin thumb at the right edge, gone at the bottom"
    echo 'keybind = cmd+alt+s=toggle_secure_input' | paste_box; pause; verdict "🔒 in title, survives app switch, toggles off"
    echo 'bell-features = system' | paste_box; pause; run "printf '\a'"; verdict "alert sound"
    echo 'bell-features = no-attention' | paste_box; pause; say "switch away within 3 s…"; sleep 3; printf '\a'; verdict "no Dock bounce"
}

g8() { GROUP="Multiple windows"
    printf '%s\n' 'cd /tmp   # then Cmd+N → shell in /tmp; Cmd+T still a tab in this window' 'while true; do date; sleep 1; done   # in one window, type in the other' | paste_box; pause
    verdict "Cmd+N opens in /tmp"; verdict "windows independent, both update"
    verdict "Cmd+W / exit / red button closes only that window"
    verdict "closing last window keeps Dock icon, click opens a window"
    verdict "split, tabs, find, reload, theme switch work in every window"
    echo 'keybind = cmd+shift+n=new_window' | paste_box; pause; verdict "keybind new_window"
}

g9() { GROUP="scrollback-limit, unfocused splits, minimum-contrast"
    say "Cmd+D, click between the splits"; pause; verdict "inactive split faded, single pane never faded"
    printf '%s\n' 'unfocused-split-opacity = 0.3' 'unfocused-split-fill = #400000' | paste_box; pause; verdict "stronger fade toward dark red"
    echo 'minimum-contrast = 21' | paste_box; pause; verdict "all text pure black/white"
    echo 'minimum-contrast = 3' | paste_box; pause; run "printf '\e[34;44mblue on blue\e[0m\n'"; verdict "readable"
    echo 'scrollback-limit = 96000' | paste_box; say "new tab, seq 1000, scroll to top"; pause; verdict "history starts around 900"
}

g10() { GROUP="Cell size, font-thicken, underline"
    echo 'adjust-cell-height = 20%' | paste_box; pause; verdict "more spacing, centered, box drawing connects (tree/htop)"
    echo 'adjust-cell-width = 1' | paste_box; pause; verdict "wider columns"
    echo 'font-thicken = true' | paste_box; pause; verdict "bolder text"
    run "printf '\e[4:3mcurly\e[0m \e[4:2mdouble\e[0m\n'"; verdict "flatter wave, double lines apart"
}

g11() { GROUP="Scrollbar drag, progress bar, bell border/audio"
    run "seq 2000"; say "scroll up a bit, drag the thumb to top and bottom"; pause; verdict "drag follows, top shows 1, bottom is live"
    run "printf '\e]9;4;1;40\a'"; verdict "blue bar over left 40% of the top edge"
    run "printf '\e]9;4;2;70\a'"; verdict "red 70%"
    run "printf '\e]9;4;4;50\a'"; verdict "yellow 50%"
    run "printf '\e]9;4;3\a'"; verdict "full dim blue"
    run "printf '\e]9;4;0\a'"; verdict "bar gone"
    echo 'bell-features = border' | paste_box; pause; run "sleep 2; printf '\a'"; verdict "amber outline, cleared on key/refocus"
    printf '%s\n' 'bell-features = audio' 'bell-audio-path = /System/Library/Sounds/Glass.aiff' 'bell-audio-volume = 0.1' | paste_box; pause; run "printf '\a'"; verdict "quiet Glass sound"
}

g12() { GROUP="Shell integration additions"
    printf '%s\n' 'seq 50; ls   # resize narrower and wider, then Cmd+Shift+↑ and Cmd+Shift+O' | paste_box; pause; verdict "marks survive resize"
    echo "SHELL=/bin/bash $BIN   # ls -la, Cmd+Shift+O; Enter on empty prompt, Cmd+Shift+O again" | paste_box; pause; verdict "bash 3.2 output selection"
    command -v nu >/dev/null && { echo "SHELL=\$(which nu) $BIN" | paste_box; pause; verdict "nushell"; } || { note "nu not installed"; log skip "nushell" "not installed"; }
    command -v elvish >/dev/null && { echo "SHELL=\$(which elvish) $BIN" | paste_box; pause; verdict "elvish"; } || { note "elvish not installed"; log skip "elvish" "not installed"; }
}

g13() { GROUP="Bundled themes"
    auto "+list-themes count" "$("$BIN" +list-themes | wc -l | tr -d ' ')" '861'
    auto "gruvbox themes listed" "$("$BIN" +list-themes | grep -ic gruvbox)" '[1-9]*'
    echo 'theme = Gruvbox Dark' | paste_box; note "(comment out your background/foreground lines)"; pause; verdict "Gruvbox incl. cursor & selection"
    printf '%s\n' 'background = #000000' 'theme = Dracula' | paste_box; pause; verdict "black background, rest Dracula"
    echo 'theme = catppuccin-mocha' | paste_box; pause; verdict "old alias works"
}

g14() { GROUP="confirm-close-surface"
    say "in another tab: sleep 100, then Cmd+W"; pause; verdict "alert; Cancel keeps, Close closes"
    verdict "idle prompt Cmd+W closes without asking"
    verdict "Cmd+Q asks 'Quit Laplace?', red button asks 'Close this window?'"
    printf '%s\n' 'confirm-close-surface = false' '# then = always' | paste_box; pause; verdict "false never asks, always always asks"
}

g15() { GROUP="macOS menu bar"
    verdict "Laplace/Shell/Edit/View/Window menus, every item works"
    verdict "Secure Keyboard Entry menu item toggles ✓ and 🔒"
    verdict "Cmd+H, Ctrl+Cmd+F, Minimize, Window menu lists windows"
    verdict "close all windows, Shell ▸ New Window opens one"
}

g16() { GROUP="Kitty Caps Lock"
    say "turn Caps Lock ON now"; pause
    printf '\e[>1u'; key "Ctrl+A (Caps Lock ON)" 15 '^\[\[97;69u'; printf '\e[<u'
    say "turn Caps Lock OFF now"; pause
    printf '\e[>1u'; key "Ctrl+A (Caps Lock OFF)" 15 '^\[\[97;5u'; printf '\e[<u'
}

g17() { GROUP="Terminal inspector"
    say "Cmd+Option+I, move over colored output, type a, Ctrl+C, ↑ in another tab"; run "ls -G /"; pause
    verdict "panel shows sections, toggles off"; verdict "Cell updates live"; verdict "Last key shows bytes"; verdict "vim → alternate / app-cursor on, sleep → running"
}

g18() { GROUP="Native macOS tabs"
    echo 'macos-native-tabs = true' | paste_box; pause
    verdict "Cmd+T native tab in cwd"; verdict "Cmd+Shift+[ ] and Cmd+1/2 switch, Cmd+W closes one"; verdict "without option: own tab bar"
}

g19() { GROUP="Quick Terminal (macOS)"
    echo 'keybind = global:ctrl+grave_accent=toggle_quick_terminal' | paste_box; pause
    verdict "drops in from top with focus"; verdict "hides, focus returns, same shell when reopened"; verdict "autohide on click elsewhere"; verdict "exit closes, hotkey opens fresh"
}

g20() { GROUP="Custom shaders"
    echo "custom-shader = $REPO/examples/shaders/invert.glsl" | paste_box; pause; verdict "inverted, upright, mouse lines up"
    echo "custom-shader = $REPO/examples/shaders/scanlines.glsl" | paste_box; pause; verdict "scanlines animate only while focused"
    echo 'custom-shader-animation = false' | paste_box; pause; verdict "static"
    note "a shader with a typo: Laplace still starts, error on stderr"; verdict "broken shader skipped"
}

g21() { GROUP="OSC 4 recolors existing text"
    run "printf '\e[31mred\e[0m\n'; ls -G /"; pause "then recolor"
    run "printf '\e]4;1;#00ff00\a'"; verdict "earlier red and ls colors turned green"
    run "printf '\e]104\a'"; verdict "red again"
}

g22() { GROUP="Ligatures and font-feature"
    note "needs a ligature font: font-family = JetBrains Mono (or Fira Code)"
    run "echo '-> => != === <= >= :: |> www 0xFF'"; verdict "ligatures, each over its own cells"
    run "printf '\e[1m-> !=\e[0m \e[3m-> !=\e[0m\n'"; verdict "bold and italic ligatures"
    verdict "half-selecting a ligature, cursor over it: no garbage"
    echo 'font-feature = -calt' | paste_box; pause; verdict "ligatures off"
    verdict "default font looks as before"
}

g23() { GROUP="Inline images (Kitty, Sixel)"
    run "img png; echo"; verdict "gradient image at native size"
    run "img cells; echo"; verdict "sized to 20x10 cells"
    run "img sixel"; verdict "sixel stripes, prompt continues below"
    say "scroll up/down, Cmd+D split, resize, open vim and quit"; pause
    verdict "moves with text, clipped at pane edges"; verdict "stays next to same text after resize"; verdict "still there after vim"
    run "clear"; verdict "clear removes images"
}

g24() { GROUP="Color emoji and grapheme clusters"
    local s='😀 👍🏽 👨‍👩‍👧 🇻🇳 ❤️ #️⃣ 🏳️‍🌈'
    run "echo '$s'"; verdict "all color, each cluster one glyph 2 cells, text after aligned"
    say "drag-select the emoji OUTPUT line (the one below the echo), then Cmd+C"; pause
    auto "clipboard round trip" "$(pbpaste)" "$s"
    run "printf '\e[?2027l'; echo '👨‍👩‍👧'; printf '\e[?2027h'"; verdict "legacy: three separate faces"
    run "echo 'q̃ x̧̌ कि नमस्ते'"; verdict "marks on their base letters"
    run "printf '\e[1m😀\e[0m \e[7m👍🏽\e[0m\n'"; verdict "bold / inverse emoji still in color"
}

g25() { GROUP="Kitty graphics: z-index, placeholders, animation, shm"
    run "img under; printf '\e[3A'; echo 'TEXT OVER THE IMAGE'; printf '\e[41m   red bg   \e[0m\n\n\n'"; verdict "image behind text, red cells cover it"
    run "img deep; printf '\e[3A'; printf '\e[41m   red bg   \e[0m\n\n\n\n'"; verdict "image behind colored cells too"
    run "img placeholder"; verdict "placeholder image 10x4 cells"
    note "tmux: run 'tmux', then 'bash $REPO/scripts/manual-test.sh 25' inside it"
    run "img anim; echo"; verdict "red→blue→green animation, ~0% CPU when scrolled away"
    run "img shm; echo"; verdict "image via shared memory"
}

g26() { GROUP="Right-to-left text"
    run "echo 'abc שלום עולם 123 def'"; verdict "Hebrew right to left, rest unchanged"
    run "echo 'سلام عليكم'"; verdict "Arabic letters joined, RTL order"
    verdict "click/drag in Hebrew selects cell under mouse, copy logical order"
    say "type Hebrew at a prompt in another tab"; verdict "cursor at the next letter's visual position"
}

g27() { GROUP="Kitty lock modifiers (Linux)"
    [ "$OS" = Linux ] || { note "Linux only"; log skip "all" "not on Linux"; return; }
    printf '\e[>1u'; key "a with Caps Lock ON" 15 '*;65u*'; printf '\e[<u'
}

g28() { GROUP="Quick Terminal (Linux)"
    [ "$OS" = Linux ] || { note "Linux only"; log skip "all" "not on Linux"; return; }
    verdict "X11 global hotkey toggles"; verdict "laplace +toggle-quick-terminal toggles"
}

g29() { GROUP="Debt: Kitty keys in the real app"
    printf '\e[>1u'
    key "a" 10 'a'; key "Ctrl+A" 10 '^\[\[97;5u'; key "Alt+A (Option)" 10 '^\[\[97;3u'
    key "Esc" 10 '^\[\[27u'; key "Tab" 10 $'\t'; key "Ctrl+I" 10 '^\[\[105;5u'
    printf '\e[<u'; verdict "key repeat (hold a key in vim/nvim) behaves"
}

g30() { GROUP="Debt: OSC 8 hover"
    run "printf '\e]8;;https://example.com\e\\\\example link\e]8;;\e\\\\\n'"
    verdict "Cmd + mouse move underlines it, preview pill shows the URL"; verdict "Cmd+Click opens it"
}

NAMES=(x "OSC colors" "modifyOtherKeys" "Notifications/progress/pointer/bell" "XTGETTCAP/DECRQSS/mouse/scheme"
    "Config basics" "Shell integration" "Drag&drop/scrollbar/secure/bell" "Multiple windows" "Splits/contrast/scrollback"
    "Cell size/thicken/underline" "Scrollbar drag/progress bar/bell" "Shell integration additions" "Themes"
    "confirm-close-surface" "Menu bar" "Kitty Caps Lock" "Inspector" "Native tabs" "Quick Terminal macOS"
    "Custom shaders" "OSC 4 recolor" "Ligatures" "Inline images" "Emoji" "Kitty graphics extras" "RTL"
    "Kitty lock (Linux)" "Quick Terminal (Linux)" "Debt: Kitty keys" "Debt: OSC 8 hover")

if [ "$1" = -l ]; then
    for i in $(seq 1 30); do printf '%2d  %s\n' "$i" "${NAMES[$i]}"; done; exit
fi
[ -x "$BIN" ] || { echo "build first: cargo build --release"; exit 1; }
[ -s "$LOG" ] || printf '# Manual test results\n' >"$LOG"
printf '\n## %s (%s)\n\n| Group | Status | Item | Detail |\n|---|---|---|---|\n' "$(date '+%Y-%m-%d %H:%M')" "$(git -C "$REPO" rev-parse --short HEAD)" >>"$LOG"

SEL=("$@"); [ $# -eq 0 ] && SEL=($(seq 1 30))
for i in "${SEL[@]}"; do
    printf '\n%s━━ %d. %s ━━%s\n' "$B" "$i" "${NAMES[$i]}" "$R"
    "g$i"
done
printf '\n%sDone.%s Results: %s\n' "$B" "$R" "$LOG"
