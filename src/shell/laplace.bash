# Laplace shell integration for bash. Laplace starts `bash --rcfile <this file>`
# (works on every bash, including macOS's 3.2), so load the login startup files
# as the login shell Laplace would otherwise start, then install the hooks.
if [ -n "$LAPLACE_BASH_INJECT" ]; then
  builtin unset LAPLACE_BASH_INJECT
  if [ -r /etc/profile ]; then builtin source /etc/profile; fi
  for _laplace_f in "$HOME/.bash_profile" "$HOME/.bash_login" "$HOME/.profile"; do
    if [ -r "$_laplace_f" ]; then
      builtin source "$_laplace_f"
      break
    fi
  done
  builtin unset _laplace_f
fi

# OSC 133 prompt / command marks, OSC 7 working directory and, per
# LAPLACE_SHELL_INTEGRATION_FEATURES, a bar cursor and the window title.
[[ $- == *i* ]] || return 0
[[ -n $_laplace_loaded ]] && return 0
_laplace_loaded=1

_laplace_has() { [[ ",${LAPLACE_SHELL_INTEGRATION_FEATURES}," == *",$1,"* ]]; }

_laplace_prompt_command() {
  local ret=$?
  _laplace_armed=
  printf '\e]133;D;%s\a' "$ret"
  printf '\e]7;file://%s%s\a' "$HOSTNAME" "${PWD// /%20}"
  _laplace_has title && printf '\e]2;%s\a' "${PWD/#$HOME/\~}"
  _laplace_has cursor && printf '\e[5 q'
  # Prompt start / end marks, re-added whenever something rebuilds PS1.
  [[ $PS1 == *'133;A'* ]] || PS1='\[\e]133;A\a\]'"$PS1"
  [[ $PS1 == *'133;B'* ]] || PS1="$PS1"'\[\e]133;B\a\]'
  return $ret
}

# PS0 (bash 4.4+) runs after a command is read, before it executes.
if (( BASH_VERSINFO[0] > 4 || (BASH_VERSINFO[0] == 4 && BASH_VERSINFO[1] >= 4) )); then
  if _laplace_has cursor; then
    PS0='\[\e[0 q\e]133;C\a\]'"$PS0"
  else
    PS0='\[\e]133;C\a\]'"$PS0"
  fi
  if [[ "$(declare -p PROMPT_COMMAND 2>/dev/null)" == "declare -a"* ]]; then
    PROMPT_COMMAND=(_laplace_prompt_command "${PROMPT_COMMAND[@]}")
  else
    PROMPT_COMMAND="_laplace_prompt_command${PROMPT_COMMAND:+; $PROMPT_COMMAND}"
  fi
elif [[ -z "$(trap -p DEBUG)" ]]; then
  # Older bash (macOS /bin/bash 3.2) has no PS0: a DEBUG trap armed at the end
  # of PROMPT_COMMAND marks the first command the user runs. Skipped when
  # something else (bash-preexec, ...) already owns the trap.
  _laplace_armed=
  _laplace_arm() { _laplace_armed=1; }
  _laplace_debug() {
    [[ -n $_laplace_armed && $BASH_COMMAND != _laplace_prompt_command* ]] || return 0
    _laplace_armed=
    _laplace_has cursor && printf '\e[0 q'
    printf '\e]133;C\a'
  }
  PROMPT_COMMAND="_laplace_prompt_command${PROMPT_COMMAND:+; $PROMPT_COMMAND}; _laplace_arm"
  trap '_laplace_debug' DEBUG
else
  PROMPT_COMMAND="_laplace_prompt_command${PROMPT_COMMAND:+; $PROMPT_COMMAND}"
fi
