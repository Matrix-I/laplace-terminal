# Litty shell integration for elvish. Elvish has no autoload, so add
#   use litty
# to ~/.config/elvish/rc.elv; Litty puts this module on the module path
# (XDG_DATA_DIRS). Emits OSC 133 prompt / command marks, OSC 7 and, per
# LITTY_SHELL_INTEGRATION_FEATURES, a bar cursor and the window title.
use str
use re
use platform

# Do not leak Litty's data dir to programs started from this shell.
var xdg_inject = ''
if (has-env LITTY_SHELL_INTEGRATION_XDG_DIR) {
  set xdg_inject = $E:LITTY_SHELL_INTEGRATION_XDG_DIR
  unset-env LITTY_SHELL_INTEGRATION_XDG_DIR
} elif (has-env LAPLACE_SHELL_INTEGRATION_XDG_DIR) {
  set xdg_inject = $E:LAPLACE_SHELL_INTEGRATION_XDG_DIR
  unset-env LAPLACE_SHELL_INTEGRATION_XDG_DIR
}
if (!=s $xdg_inject '') {
  var rest = [(str:split : (or (get-env XDG_DATA_DIRS) '') | each {|d| if (and (!=s $d $xdg_inject) (!=s $d '')) { put $d } })]
  if (== (count $rest) 0) { unset-env XDG_DATA_DIRS } else { set-env XDG_DATA_DIRS (str:join : $rest) }
}

var features = []
if (has-env LITTY_SHELL_INTEGRATION_FEATURES) {
  set features = [(str:split , $E:LITTY_SHELL_INTEGRATION_FEATURES)]
} elif (has-env LAPLACE_SHELL_INTEGRATION_FEATURES) {
  set features = [(str:split , $E:LAPLACE_SHELL_INTEGRATION_FEATURES)]
}
var running = $false

set edit:before-readline = [$@edit:before-readline {
  if $running {
    # Elvish does not expose the exit status of an interactive command.
    print "\e]133;D\a"
    set running = $false
  }
  var clean_pwd = (re:replace '[[:cntrl:]]+' '' $pwd)
  print "\e]7;file://"(re:replace '[[:cntrl:]]+' '' (platform:hostname))(str:replace ' ' '%20' $clean_pwd)"\a"
  if (has-value $features cursor) { print "\e[5 q" }
  if (has-value $features title) { print "\e]2;"(re:replace '[[:cntrl:]]+' '' (tilde-abbr $pwd))"\a" }
  print "\e]133;A\a"
}]

set edit:after-readline = [$@edit:after-readline {|cmd|
  if (has-value $features cursor) { print "\e[0 q" }
  if (has-value $features title) { print "\e]2;"(re:replace '[[:cntrl:]]+' '' $cmd)"\a" }
  print "\e]133;C\a"
  set running = $true
}]

