# armin for zsh: eval "$(armin init zsh)" in ~/.zshrc.
#
# The menu lives in POSTDISPLAY (text zsh shows after the buffer) and its
# colors in region_highlight, tagged memo=armin so other plugins' highlights
# survive. zsh then redraws, scrolls and clears it like the buffer itself.
#
# Keys while the menu is open: Tab / Shift-Tab and Ctrl-N / Ctrl-P move,
# Enter fills the line with the selection (it does not run it), Esc closes.
# With the menu closed, every key does what it did before.

zmodload zsh/datetime zsh/parameter zsh/system zsh/net/socket
autoload -Uz add-zle-hook-widget add-zsh-hook

typeset -g  _armin_bin=__ARMIN_BIN__
typeset -g  _armin_sock=__ARMIN_SOCK__
typeset -g  _armin_version=__ARMIN_VERSION__
typeset -g  ARMIN_MAX_ROWS=${ARMIN_MAX_ROWS:-8}
typeset -g  ARMIN_HL_MATCH=${ARMIN_HL_MATCH:-bold}
typeset -g  ARMIN_HL_SELECTED=${ARMIN_HL_SELECTED:-standout}
typeset -g  ARMIN_HL_GHOST=${ARMIN_HL_GHOST:-fg=8}

typeset -ga _armin_items      # commands the open menu shows
typeset -ga _armin_marks      # per item: matched ranges, "start-end,..."
typeset -gi _armin_sel=0      # 1-based selection; 0 means nothing selected
typeset -g  _armin_seen=''    # buffer and cursor the menu was computed for
typeset -gi _armin_open=0

# ── The daemon ──────────────────────────────────────────────────────────────

# Sends a request (the arguments) to the daemon and leaves what follows its
# "ok" in _armin_reply. Fails when no daemon answers, or it answers anything
# else, such as "restart" after an upgrade.
typeset -g _armin_reply=''
_armin_ask() {
  local fd chunk req=$_armin_version$'\0' arg
  for arg in "$@"; do req+=$arg$'\0'; done
  zsocket $_armin_sock 2>/dev/null || return 1
  fd=$REPLY
  _armin_reply=''
  if syswrite -o $fd -- $req 2>/dev/null; then
    while sysread -i $fd -t 0.5 chunk 2>/dev/null; do _armin_reply+=$chunk; done
  fi
  exec {fd}>&-
  [[ $_armin_reply == ok$'\0'* ]] || return 1
  _armin_reply=${_armin_reply#ok$'\0'}
}

# Starts a daemon, detached from this shell, at most once every few seconds:
# while it loads, each keystroke would try again.
typeset -gi _armin_spawned=0
_armin_spawn() {
  (( EPOCHSECONDS - _armin_spawned < 5 )) && return
  _armin_spawned=$EPOCHSECONDS
  nohup "$_armin_bin" daemon </dev/null &>/dev/null &!
}

# ── Suggesting ──────────────────────────────────────────────────────────────

_armin_clear_display() {
  POSTDISPLAY=''
  region_highlight=( ${region_highlight:#*memo=armin} )
}

_armin_close() {
  _armin_open=0
  _armin_sel=0
  _armin_items=()
  _armin_marks=()
  _armin_clear_display
}

# Asks armin for the current buffer. Only with the cursor at the end of a
# one-line buffer: anywhere else, a suggestion has nowhere to go.
_armin_compute() {
  _armin_items=()
  _armin_marks=()
  _armin_sel=0
  [[ -z $BUFFER || $BUFFER == *$'\n'* ]] && return
  (( CURSOR == ${#BUFFER} )) || return
  local out item
  if _armin_ask suggest "$ARMIN_MAX_ROWS" "$PWD" "$BUFFER"; then
    out=$_armin_reply
  else
    # No daemon answered: start one for the next keystroke, and ask armin
    # directly for this one.
    _armin_spawn
    out=$("$_armin_bin" suggest --cwd "$PWD" --limit $ARMIN_MAX_ROWS -- "$BUFFER" 2>/dev/null) || return
  fi
  # One item per NUL: matched ranges, a tab, the command.
  for item in ${(0)out}; do
    _armin_marks+=( "${item%%$'\t'*}" )
    _armin_items+=( "${item#*$'\t'}" )
  done
}

_armin_render() {
  _armin_clear_display
  (( ${#_armin_items} )) || { _armin_open=0; return }
  _armin_open=1

  # Leave room for the prompt and buffer lines, so the menu never needs more
  # height than the pane has.
  local -i rows=${#_armin_items}
  (( rows > LINES - 3 )) && rows=$(( LINES - 3 ))
  (( rows < 1 )) && { _armin_open=0; return }

  # A command spanning lines shows on one row, each newline as ↵.
  local -a shown=( "${(@)${(@)_armin_items[1,rows]//$'\n'/↵}//$'\t'/ }" )
  local -i width=0 i s e
  local item
  for item in $shown; do
    (( ${#item} > width )) && width=${#item}
  done
  (( width > COLUMNS - 4 )) && width=$(( COLUMNS - 4 ))

  # Ghost text: the rest of the selected item, right after the cursor. A fuzzy
  # match doesn't continue what was typed, so it gets none.
  local ghost='' sel=${_armin_items[_armin_sel]}
  (( _armin_sel )) && [[ $sel == "$BUFFER"* ]] && ghost=${sel#"$BUFFER"}

  local text=$ghost range
  local -i base=${#BUFFER} off
  local -a hl
  (( ${#ghost} )) && hl+=( "$base $(( base + ${#ghost} )) $ARMIN_HL_GHOST,memo=armin" )

  for (( i = 1; i <= rows; i++ )); do
    item=${shown[i]}
    (( ${#item} > width )) && item="${item[1,width-1]}…"
    text+=$'\n'
    off=$(( base + ${#text} ))
    text+=" ${(r:width:)item} "
    (( i == _armin_sel )) && hl+=( "$off $(( off + width + 2 )) $ARMIN_HL_SELECTED,memo=armin" )
    for range in ${(s:,:)_armin_marks[i]}; do
      s=${range%-*} e=${range#*-}
      (( e > width )) && e=width
      (( s < e )) && hl+=( "$(( off + 1 + s )) $(( off + 1 + e )) $ARMIN_HL_MATCH,memo=armin" )
    done
  done

  POSTDISPLAY=$text
  region_highlight+=( $hl )
}

# Runs before every redraw: ask armin again only when the buffer or the
# cursor moved.
_armin_pre_redraw() {
  [[ "$CURSOR:$BUFFER" == $_armin_seen ]] && return
  _armin_seen="$CURSOR:$BUFFER"
  _armin_compute
  _armin_render
}

_armin_line_init() {
  _armin_seen=''
  _armin_close
}

# Clear before zsh draws the finished line, or the menu stays in scrollback.
_armin_line_finish() {
  _armin_close
}

_armin_move() {
  local -i n=${#_armin_items}
  _armin_sel=$(( (_armin_sel - 1 + $1 + n) % n + 1 ))
  _armin_render
}

# ── Keys ────────────────────────────────────────────────────────────────────

# Each key acts on the menu when it is open. Otherwise it runs the widget it
# was bound to before armin, or nothing if it had none.
typeset -gA _armin_fallback

_armin_or_fallback() {
  local name=$1; shift
  if (( _armin_open )); then "$@"
  elif [[ -n ${_armin_fallback[$name]} ]]; then zle "${_armin_fallback[$name]}"; fi
}

_armin_next()   { _armin_or_fallback next   _armin_move 1 }
_armin_prev()   { _armin_or_fallback prev   _armin_move -1 }
_armin_down()   { _armin_or_fallback down   _armin_move 1 }
_armin_up()     { _armin_or_fallback up     _armin_move -1 }
_armin_escape() { _armin_or_fallback escape _armin_close }

# Enter fills the line with the selection; with nothing selected it runs the
# line as usual.
_armin_accept() {
  if (( _armin_open && _armin_sel )); then
    BUFFER=${_armin_items[_armin_sel]}
    CURSOR=${#BUFFER}
    _armin_seen="$CURSOR:$BUFFER"
    _armin_close
  else
    _armin_close
    zle "${_armin_fallback[accept]:-accept-line}"
  fi
}

# Bind in main, the keymap the user types in (emacs, or viins when $EDITOR
# names vi). A key's old widget becomes its fallback; a prefix key such as Esc
# in emacs reads back as a quoted string and counts as unbound.
_armin_bind() {
  local name=$1 key=$2 w
  w=${${(z)$(bindkey -M main -- $key)}[2]}
  [[ $w == undefined-key || $w == \"* ]] && w=''
  _armin_fallback[$name]=$w
  zle -N _armin_$name
  bindkey -M main -- $key _armin_$name
}

_armin_bind next   '^I'
_armin_bind prev   '^[[Z'
_armin_bind down   '^N'
_armin_bind up     '^P'
_armin_bind accept '^M'
_armin_bind escape '^['
# Esc only fires after KEYTIMEOUT (default 0.4s) in case it starts a longer
# sequence such as an arrow key. Shorten it so closing feels instant.
(( KEYTIMEOUT > 10 )) && KEYTIMEOUT=10

add-zle-hook-widget line-pre-redraw _armin_pre_redraw
add-zle-hook-widget line-init       _armin_line_init
add-zle-hook-widget line-finish     _armin_line_finish

# Ctrl-C aborts the line without running line-finish, so the menu would stay
# on screen. Clear it from the interrupt trap, then let the signal go on to
# any trap that was already there.
_armin_interrupt() { _armin_close; zle -R }
zle -N _armin_interrupt
(( ${+functions[TRAPINT]} )) && functions -c TRAPINT _armin_prev_trapint
TRAPINT() {
  zle && zle _armin_interrupt
  (( ${+functions[_armin_prev_trapint]} )) && { _armin_prev_trapint "$@"; return }
  return $(( 128 + $1 ))
}

# ── Recording ───────────────────────────────────────────────────────────────

typeset -g _armin_session="$$-$EPOCHSECONDS"
typeset -g _armin_cmd='' _armin_cmd_pwd='' _armin_start=''

# The command as typed, and where it started: a cd changes $PWD before it ends.
_armin_preexec() {
  [[ -o hist_ignore_space && $1 == ' '* ]] && return
  _armin_cmd=$1
  _armin_cmd_pwd=$PWD
  _armin_start=$EPOCHREALTIME
}

# Records in the background, so the prompt never waits on the disk.
_armin_precmd() {
  local -i exit=$?
  [[ -n $_armin_cmd ]] || return
  local -i ms=$(( (EPOCHREALTIME - _armin_start) * 1000 ))
  "$_armin_bin" record --time ${_armin_start%.*} --duration $ms --exit $exit \
    --session $_armin_session --cwd "$_armin_cmd_pwd" -- "$_armin_cmd" &>/dev/null &!
  _armin_cmd=''
}

# precmd first in line, so $? is still the command's.
precmd_functions=( _armin_precmd ${precmd_functions:#_armin_precmd} )
add-zsh-hook preexec _armin_preexec

# Have the daemon ready before the first keystroke.
_armin_ask ping || _armin_spawn
