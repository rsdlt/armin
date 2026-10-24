# armin, phase 0 spike: a blink.cmp-style popup drawn inside ZLE.
#
# Pure zsh, no Rust yet. It answers one question: can a plugin draw a
# multi-line, colored menu under the prompt that zsh itself redraws, scrolls
# and clears? The menu lives in POSTDISPLAY (text zsh shows after the buffer)
# and its colors in region_highlight, tagged memo=armin so other plugins'
# highlights survive. Candidates are a plain prefix match over history; the
# real ranking comes in Rust.
#
#   source spike/armin.zsh
#
# Keys while the menu is open: Tab / Shift-Tab and Ctrl-N / Ctrl-P move,
# Enter fills the line with the selection (it does not run it), Esc closes.
# With the menu closed, every key does what it did before.

zmodload zsh/parameter
autoload -Uz add-zle-hook-widget

typeset -g  ARMIN_MAX_ROWS=${ARMIN_MAX_ROWS:-8}
typeset -g  ARMIN_HISTORY_SCAN=${ARMIN_HISTORY_SCAN:-5000}

typeset -ga _armin_hist       # unique history lines, newest first
typeset -ga _armin_items      # what the open menu shows
typeset -gi _armin_sel=0      # 1-based selection; 0 means nothing selected
typeset -g  _armin_seen=''    # buffer the menu was last computed for
typeset -gi _armin_open=0

# Refreshed before each prompt, so commands run this session show up.
_armin_load_history() {
  local -a keys=( ${(Onk)history} )
  local k
  _armin_hist=()
  for k in ${keys[1,ARMIN_HISTORY_SCAN]}; do
    _armin_hist+=( "${history[$k]}" )
  done
  _armin_hist=( ${(u)_armin_hist} )
}

_armin_clear_display() {
  POSTDISPLAY=''
  region_highlight=( ${region_highlight:#*memo=armin} )
}

_armin_close() {
  _armin_open=0
  _armin_sel=0
  _armin_items=()
  _armin_clear_display
}

# Fills _armin_items for the current buffer. Single-line buffers only.
_armin_compute() {
  _armin_items=()
  _armin_sel=0
  [[ -z $BUFFER || $BUFFER == *$'\n'* ]] && return
  local -a matches=( ${(M)_armin_hist:#${(b)BUFFER}?*} )
  _armin_items=( ${matches[1,ARMIN_MAX_ROWS]} )
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

  local -i width=0 i
  local item
  for item in ${_armin_items[1,rows]}; do
    (( ${#item} > width )) && width=${#item}
  done
  (( width > COLUMNS - 4 )) && width=$(( COLUMNS - 4 ))

  # Ghost text: the rest of the selected item, right after the cursor.
  local ghost=''
  (( _armin_sel )) && ghost=${_armin_items[_armin_sel]#$BUFFER}

  local text=$ghost
  local -i base=${#BUFFER} off
  local -a hl
  (( ${#ghost} )) && hl+=( "$base $(( base + ${#ghost} )) fg=8,memo=armin" )

  local row
  for (( i = 1; i <= rows; i++ )); do
    item=${_armin_items[i]}
    (( ${#item} > width )) && item="${item[1,width-1]}…"
    row=" ${(r:width:)item} "
    text+=$'\n'
    off=$(( base + ${#text} ))
    text+=$row
    if (( i == _armin_sel )); then
      hl+=( "$off $(( off + ${#row} )) standout,memo=armin" )
    else
      # The part already typed in bold, the rest plain.
      hl+=( "$(( off + 1 )) $(( off + 1 + ${#BUFFER} )) bold,memo=armin" )
    fi
  done

  POSTDISPLAY=$text
  region_highlight+=( $hl )
}

# Runs before every redraw: recompute only when the buffer changed.
_armin_pre_redraw() {
  [[ $BUFFER == $_armin_seen ]] && return
  _armin_seen=$BUFFER
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
    _armin_seen=$BUFFER
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

autoload -Uz add-zsh-hook
add-zsh-hook precmd _armin_load_history
_armin_load_history
