# Litty shell integration for zsh: OSC 133 prompt / command marks, OSC 7
# working directory and, per LITTY_SHELL_INTEGRATION_FEATURES, a bar cursor
# at the prompt (`cursor`) and the window title (`title`).
[[ -o interactive ]] || return 0
(( ${+_litty_loaded} )) && return 0
typeset -g _litty_loaded=1
typeset -g _litty_running=0

_litty_feats="${LITTY_SHELL_INTEGRATION_FEATURES-$LAPLACE_SHELL_INTEGRATION_FEATURES}"
_litty_has() { [[ ",${_litty_feats}," == *",$1,"* ]] }

_litty_precmd() {
  local ret=$?
  if (( _litty_running )); then
    builtin print -n "\e]133;D;${ret}\a"
    _litty_running=0
  fi
  builtin print -n "\e]7;file://${HOST}${PWD// /%20}\a"
  _litty_has title && builtin print -Pn "\e]2;%~\a"
  _litty_has cursor && builtin print -n "\e[5 q"
  # Prompt start / end marks, re-added whenever a theme rebuilds PS1.
  [[ $PS1 == *$'\e]133;A'* ]] || PS1=$'%{\e]133;A\a%}'"$PS1"
  [[ $PS1 == *$'\e]133;B'* ]] || PS1="$PS1"$'%{\e]133;B\a%}'
  # Run last, so hooks added later (prompt themes) cannot drop the marks.
  if [[ ${precmd_functions[-1]} != _litty_precmd ]]; then
    precmd_functions=(${precmd_functions:#_litty_precmd} _litty_precmd)
  fi
  return $ret
}

_litty_preexec() {
  _litty_has cursor && builtin print -n "\e[0 q"
  _litty_has title && builtin print -rn -- $'\e]2;'"${1//[[:cntrl:]]/}"$'\a'
  builtin print -n "\e]133;C\a"
  _litty_running=1
}

autoload -Uz add-zsh-hook
add-zsh-hook precmd _litty_precmd
add-zsh-hook preexec _litty_preexec
