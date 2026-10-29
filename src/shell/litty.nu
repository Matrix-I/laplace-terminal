# Litty shell integration for nushell (0.101+), loaded from
# `$XDG_DATA_DIRS/nushell/vendor/autoload` after config.nu. Nushell emits the
# OSC 133 prompt / command marks and OSC 7 itself; turn them on, plus the
# window title and a bar cursor per LITTY_SHELL_INTEGRATION_FEATURES.

# Do not leak Litty's data dir to programs started from this shell.
let litty_xdg = ($env.LITTY_SHELL_INTEGRATION_XDG_DIR? | default $env.LAPLACE_SHELL_INTEGRATION_XDG_DIR?)
if ($litty_xdg | is-not-empty) {
    let ours = $litty_xdg
    let rest = ($env.XDG_DATA_DIRS? | default "" | split row (char esep) | where $it != $ours and $it != "")
    if ($rest | is-empty) { hide-env XDG_DATA_DIRS } else { $env.XDG_DATA_DIRS = ($rest | str join (char esep)) }
    if ($env.LITTY_SHELL_INTEGRATION_XDG_DIR? | is-not-empty) { hide-env LITTY_SHELL_INTEGRATION_XDG_DIR }
    if ($env.LAPLACE_SHELL_INTEGRATION_XDG_DIR? | is-not-empty) { hide-env LAPLACE_SHELL_INTEGRATION_XDG_DIR }
}

$env.config.shell_integration.osc133 = true
$env.config.shell_integration.osc7 = true

let litty_raw_features = ($env.LITTY_SHELL_INTEGRATION_FEATURES? | default ($env.LAPLACE_SHELL_INTEGRATION_FEATURES? | default ""))
let litty_features = ($litty_raw_features | split row ",")
if "title" in $litty_features {
    $env.config.shell_integration.osc2 = true
}
if "cursor" in $litty_features {
    $env.config.cursor_shape.emacs = "line"
    $env.config.cursor_shape.vi_insert = "line"
}

