# Laplace shell integration for nushell (0.101+), loaded from
# `$XDG_DATA_DIRS/nushell/vendor/autoload` after config.nu. Nushell emits the
# OSC 133 prompt / command marks and OSC 7 itself; turn them on, plus the
# window title and a bar cursor per LAPLACE_SHELL_INTEGRATION_FEATURES.

# Do not leak Laplace's data dir to programs started from this shell.
if ($env.LAPLACE_SHELL_INTEGRATION_XDG_DIR? | is-not-empty) {
    let ours = $env.LAPLACE_SHELL_INTEGRATION_XDG_DIR
    let rest = ($env.XDG_DATA_DIRS? | default "" | split row (char esep) | where $it != $ours and $it != "")
    if ($rest | is-empty) { hide-env XDG_DATA_DIRS } else { $env.XDG_DATA_DIRS = ($rest | str join (char esep)) }
    hide-env LAPLACE_SHELL_INTEGRATION_XDG_DIR
}

$env.config.shell_integration.osc133 = true
$env.config.shell_integration.osc7 = true

let laplace_features = ($env.LAPLACE_SHELL_INTEGRATION_FEATURES? | default "" | split row ",")
if "title" in $laplace_features {
    $env.config.shell_integration.osc2 = true
}
if "cursor" in $laplace_features {
    $env.config.cursor_shape.emacs = "line"
    $env.config.cursor_shape.vi_insert = "line"
}
