# armin for zsh: eval "$(armin init zsh)" in ~/.zshrc.
#
# The menu lives in POSTDISPLAY (text zsh shows after the buffer) and its
# colors in region_highlight, tagged memo=armin so other plugins' highlights
# survive. zsh then redraws, scrolls and clears it like the buffer itself.
#
# Keys while the menu is open: Tab / Shift-Tab and Ctrl-N / Ctrl-P move, and
# the selection is put on the line as you move. Enter runs the line, Esc puts
# back what you typed and closes. With the menu closed, Tab opens it with zsh's
# own completions for the word you're on, or, on an empty line, with what
# usually comes next. With the menu closed, every other key does what it did
# before.

zmodload zsh/datetime zsh/parameter zsh/system zsh/net/socket
autoload -Uz add-zle-hook-widget add-zsh-hook

typeset -g  _armin_bin=__ARMIN_BIN__
typeset -g  _armin_sock=__ARMIN_SOCK__
typeset -g  _armin_version=__ARMIN_VERSION__
typeset -g  ARMIN_MAX_ROWS=${ARMIN_MAX_ROWS:-8}
typeset -g  ARMIN_HL_MATCH=${ARMIN_HL_MATCH:-bold}
typeset -g  ARMIN_HL_SELECTED=${ARMIN_HL_SELECTED:-standout}
typeset -g  ARMIN_HL_DESCRIPTION=${ARMIN_HL_DESCRIPTION:-fg=8}

typeset -ga _armin_items      # what the open menu shows
typeset -ga _armin_marks      # per item: matched ranges, "start-end,..."
typeset -ga _armin_lines      # per item: the line that picking it leaves
typeset -ga _armin_descs      # per item: where its description starts, if any
typeset -gi _armin_col=0      # the buffer column the menu lines up under
typeset -gi _armin_prompt=0   # the width of the prompt's last line
typeset -gi _armin_sel=0      # 1-based selection; 0 means nothing selected
typeset -gi _armin_top=1      # the item on the menu's first row
typeset -g  _armin_typed=''   # the buffer the menu was computed for
typeset -g  _armin_seen=''    # buffer and cursor the menu was computed for
typeset -gi _armin_open=0
typeset -g  _armin_previous='' # the command that ran last in this shell

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
  _armin_top=1
  _armin_items=()
  _armin_marks=()
  _armin_lines=()
  _armin_descs=()
  _armin_col=0
  _armin_clear_display
}

# Asks armin for the current buffer. Only with the cursor at the end of a
# one-line buffer: anywhere else, a suggestion has nowhere to go.
_armin_compute() {
  _armin_items=()
  _armin_marks=()
  _armin_lines=()
  _armin_descs=()
  _armin_col=0
  _armin_sel=0
  _armin_top=1
  _armin_typed=$BUFFER
  [[ -z $BUFFER || $BUFFER == *$'\n'* ]] && return
  (( CURSOR == ${#BUFFER} )) || return
  _armin_fetch suggest "$BUFFER"
}

# On an empty line: what usually comes after the command that ran last.
_armin_predict() {
  _armin_items=()
  _armin_marks=()
  _armin_lines=()
  _armin_descs=()
  _armin_col=0
  _armin_sel=0
  _armin_top=1
  _armin_typed=$BUFFER
  _armin_fetch next "$_armin_previous"
}

# Asks for the menu, `suggest` with what's typed or `next` with the previous
# command, and reads it into _armin_items and _armin_marks.
_armin_fetch() {
  local out item
  if _armin_ask $1 "$ARMIN_MAX_ROWS" "$PWD" "$2"; then
    out=$_armin_reply
  else
    # No daemon answered: start one for the next keystroke, and ask armin
    # directly for this one.
    _armin_spawn
    if [[ $1 == next ]]; then
      out=$("$_armin_bin" suggest --cwd "$PWD" --limit $ARMIN_MAX_ROWS --after "$2" -- '' 2>/dev/null) || return
    else
      out=$("$_armin_bin" suggest --cwd "$PWD" --limit $ARMIN_MAX_ROWS -- "$2" 2>/dev/null) || return
    fi
  fi
  # One item per NUL: matched ranges, a tab, the command.
  for item in ${(0)out}; do
    _armin_marks+=( "${item%%$'\t'*}" )
    _armin_items+=( "${item#*$'\t'}" )
  done
  _armin_lines=( "${_armin_items[@]}" )
}

# ── zsh's completions ───────────────────────────────────────────────────────

# What a capture found, per match: the row to show, its matched ranges, the
# text that replaces the word, and how many characters of the word that is.
typeset -ga _armin_cshow _armin_cmark _armin_ctext _armin_ccut _armin_cdesc
typeset -g  _armin_cquote=''

# Stands in for compadd while armin captures: lets zsh decide which matches
# fit, keeps them, and adds none. Locals carry armin's prefix, since -d names
# an array of the caller's.
_armin_compadd() {
  local -A _armin_apre _armin_hpre _armin_dscrs _armin_oad _armin_sfx _armin_wdir
  local -a _armin_isfile _armin_noquote _armin_opts
  zparseopts -E -a _armin_opts P:=_armin_apre p:=_armin_hpre d:=_armin_dscrs \
    S:=_armin_sfx W:=_armin_wdir O:=_armin_oad A:=_armin_oad D:=_armin_oad \
    f=_armin_isfile Q=_armin_noquote i: s: I: x: r: R: F: M+: E: q e n U C X+: J: V: a l k o 1 2
  # Asked only to filter, not to add: compadd's own job.
  if (( $#_armin_oad )); then builtin compadd "$@"; return; fi
  local -a _armin_hits _armin_dscr
  (( $#_armin_dscrs == 1 )) && _armin_dscr=( "${(@P)${(v)_armin_dscrs}}" )
  builtin compadd -A _armin_hits -D _armin_dscr "$@"
  local ret=$?
  (( $#_armin_hits )) || return $ret
  local hidden=${(v)_armin_hpre} word text show mark rest dir=${(v)_armin_wdir}
  local typed=${PREFIX#"$hidden"}
  local -i i
  [[ -z $dir && -n $_armin_isfile ]] && dir=${(Qe)~${:-$IPREFIX$hidden}}
  for (( i = 1; i <= $#_armin_hits; i++ )); do
    word=$_armin_hits[i]
    # With -Q the match comes quoted already, as file names with spaces do.
    if (( $#_armin_noquote )); then text=$word word=${(Q)word}; else text=${(q)word}; fi
    show=${_armin_dscr[i]:-$word}
    text=$hidden${(v)_armin_apre}$text
    # What zsh would put after it: the suffix asked for, / after a folder,
    # or a space before the next word.
    if (( ${+_armin_sfx[-S]} )); then text+=${_armin_sfx[-S]}
    elif [[ -n $_armin_isfile && -d ${dir:+$dir/}$word ]]; then text+=/ show+=/
    else text+=' '; fi
    show=${show//$'\n'/ }
    _armin_cshow+=( "$show" )
    # zsh writes a description after " -- ".
    rest=${show#*' -- '}
    _armin_cdesc+=( $(( ${#rest} < ${#show} ? ${#show} - ${#rest} - 4 : -1 )) )
    mark=''
    [[ -n $typed && $show == "$typed"* ]] && mark=0-${#typed}
    _armin_cmark+=( "$mark" )
    _armin_ctext+=( "$text" )
    _armin_ccut+=( ${#PREFIX} )
  done
  return 0
}

# The completion widget: zsh's whole completion run, with compadd swapped.
_armin_capture_widget() {
  # Grouped, zsh lists descriptions apart from their matches; a row per match
  # needs each to carry its own.
  local grouped
  zstyle -s ':completion:*' list-grouped grouped
  zstyle ':completion:*' list-grouped false
  functions[compadd]=$functions[_armin_compadd]
  {
    _main_complete 2>/dev/null
  } always {
    unfunction compadd
    if [[ -n $grouped ]]; then zstyle ':completion:*' list-grouped $grouped
    else zstyle -d ':completion:*' list-grouped; fi
  }
  _armin_cquote=$compstate[quote]
  compstate[insert]=''
  compstate[list]=''
}

# Says once per shell that zsh's completion system isn't loaded.
typeset -gi _armin_hinted=0
_armin_hint() {
  (( _armin_hinted )) && return
  _armin_hinted=1
  zle -M "armin: for zsh's completions in the menu, add  autoload -Uz compinit && compinit  to ~/.zshrc"
}

# Opens the menu with zsh's completions for the word before the cursor; fails
# when there are none, or zsh's own Tab knows better.
_armin_complete() {
  [[ $BUFFER == *$'\n'* ]] && return 1
  (( CURSOR == ${#BUFFER} )) || return 1
  (( ${+functions[_main_complete]} )) || { _armin_hint; return 1 }
  # Defined only now: a completion widget loads zsh/complete, which turns off
  # the plain completion zsh falls back to without compinit.
  (( ${+widgets[_armin_capture]} )) || zle -C _armin_capture complete-word _armin_capture_widget
  _armin_cshow=() _armin_cmark=() _armin_ctext=() _armin_ccut=() _armin_cdesc=() _armin_cquote=''
  zle _armin_capture
  # In an open quote, zsh's own Tab closes it.
  [[ -n $_armin_cquote ]] && return 1
  (( ${#_armin_cshow} )) || return 1

  local -a items marks lines texts descs
  local -A seen
  local line
  local -i i
  for (( i = 1; i <= ${#_armin_cshow}; i++ )); do
    line=${BUFFER[1,${#BUFFER}-_armin_ccut[i]]}$_armin_ctext[i]
    (( ${+seen[$line]} )) && continue
    seen[$line]=1
    items+=( "$_armin_cshow[i]" ) marks+=( "$_armin_cmark[i]" ) lines+=( "$line" )
    descs+=( $_armin_cdesc[i] )
    texts+=( "${${_armin_ctext[i]% }//$'\n'/ }" )
  done
  _armin_typed=$BUFFER
  if (( ${#lines} == 1 )); then
    _armin_set_buffer "$lines[1]"
    return 0
  fi
  _armin_items=() _armin_marks=() _armin_lines=() _armin_descs=()
  # The ones you've used with this command first; a list too long to send
  # stays in zsh's order.
  local -a order
  if (( ${#texts} <= 1000 )) &&
     _armin_ask order "${BUFFER[1,${#BUFFER}-_armin_ccut[1]]}" "${(pj:\n:)texts}"; then
    order=( ${(f)_armin_reply} )
  fi
  if (( ${#order} != ${#lines} )); then
    order=( {0..$(( ${#lines} - 1 ))} )
  fi
  for i in $order; do
    _armin_items+=( "$items[i+1]" ) _armin_marks+=( "$marks[i+1]" ) _armin_lines+=( "$lines[i+1]" )
    _armin_descs+=( $descs[i+1] )
  done
  _armin_sel=0 _armin_top=1
  # Under the word being completed.
  _armin_col=$(( ${#BUFFER} - _armin_ccut[1] ))
  _armin_move 1
}

_armin_render() {
  _armin_clear_display
  (( ${#_armin_items} )) || { _armin_open=0; return }
  _armin_open=1

  # Leave room for the prompt and buffer lines, and for a count when there
  # are more items than rows, so the menu never needs more height than the
  # pane has.
  local -i total=${#_armin_items} rows=${#_armin_items}
  (( rows > ARMIN_MAX_ROWS )) && rows=ARMIN_MAX_ROWS
  (( rows > LINES - 4 )) && rows=$(( LINES - 4 ))
  (( rows < 1 )) && { _armin_open=0; return }
  # Scroll so the selection stays in view.
  (( _armin_sel && _armin_sel < _armin_top )) && _armin_top=_armin_sel
  (( _armin_sel >= _armin_top + rows )) && _armin_top=$(( _armin_sel - rows + 1 ))
  (( _armin_top > total - rows + 1 )) && _armin_top=$(( total - rows + 1 ))
  (( _armin_top < 1 )) && _armin_top=1

  # A command spanning lines shows on one row, each newline as ↵.
  local -a shown=( "${(@)${(@)_armin_items[_armin_top,_armin_top+rows-1]//$'\n'/↵}//$'\t'/ }" )
  local -i width=0 i s e at
  local item
  for item in $shown; do
    (( ${#item} > width )) && width=${#item}
  done
  (( width > COLUMNS - 4 )) && width=$(( COLUMNS - 4 ))

  # Line up under the word: past the prompt and the buffer before it, and
  # left of the pane's edge.
  local -i indent=$(( (_armin_prompt + _armin_col) % COLUMNS ))
  (( indent > COLUMNS - width - 2 )) && indent=$(( COLUMNS - width - 2 ))
  (( indent < 0 )) && indent=0
  local pad=${(l:indent:)}

  local text='' range
  local -i base=${#BUFFER} off d
  local -a hl parts seg
  local part

  for (( i = 1; i <= rows; i++ )); do
    at=$(( _armin_top + i - 1 ))
    item=${shown[i]}
    (( ${#item} > width )) && item="${item[1,width-1]}…"
    text+=$'\n'$pad
    off=$(( base + ${#text} ))
    text+=" ${(r:width:)item} "
    # The row's styled stretches, in order and apart: where one range ends
    # inside another, zsh resets every attribute and never restores the outer
    # one. So the selected row is cut into pieces, each with its whole style.
    parts=()
    for range in ${(s:,:)_armin_marks[at]}; do
      s=${range%-*} e=${range#*-}
      (( e > width )) && e=width
      (( s < e )) && parts+=( "$(( 1 + s )) $(( 1 + e )) $ARMIN_HL_MATCH" )
    done
    d=${_armin_descs[at]:--1}
    if (( d >= 0 && d < width )); then
      # A description starts after the last matched letter.
      e=${${parts[-1]:-0 0}[(w)2]}
      (( d + 1 < e )) && d=$(( e - 1 ))
      parts+=( "$(( 1 + d )) $(( 1 + width )) $ARMIN_HL_DESCRIPTION" )
    fi
    if (( at == _armin_sel )); then
      s=0
      for part in $parts; do
        seg=( ${=part} )
        (( s < seg[1] )) && hl+=( "$(( off + s )) $(( off + seg[1] )) $ARMIN_HL_SELECTED,memo=armin" )
        hl+=( "$(( off + seg[1] )) $(( off + seg[2] )) $ARMIN_HL_SELECTED,$seg[3],memo=armin" )
        s=$seg[2]
      done
      (( s < width + 2 )) && hl+=( "$(( off + s )) $(( off + width + 2 )) $ARMIN_HL_SELECTED,memo=armin" )
    else
      for part in $parts; do
        seg=( ${=part} )
        hl+=( "$(( off + seg[1] )) $(( off + seg[2] )) $seg[3],memo=armin" )
      done
    fi
  done
  (( total > rows )) && text+=$'\n'$pad" ${(l:${#total}:)$(( _armin_sel ? _armin_sel : _armin_top ))}/$total"

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
  _armin_measure_prompt
}

# The width of the prompt's last line, where the buffer starts. A prompt that
# runs a program to draw itself, like starship's, isn't run again to measure
# it: ARMIN_PROMPT_WIDTH says, or armin guesses 2, as for "❯ ".
_armin_measure_prompt() {
  if [[ -n $ARMIN_PROMPT_WIDTH ]]; then
    _armin_prompt=$ARMIN_PROMPT_WIDTH
    return
  fi
  local p=$PS1
  if [[ -o prompt_subst && $p == *[\$\`]* ]]; then
    _armin_prompt=2
    return
  fi
  setopt local_options extended_glob
  p=${${(%)p}##*$'\n'}
  p=${p//$'\e'\[[0-9;?]#[a-zA-Z]/}
  _armin_prompt=${(m)#p}
}

# Clear before zsh draws the finished line, or the menu stays in scrollback.
_armin_line_finish() {
  _armin_close
}

# Puts `text` on the line without asking armin again, so the menu stays the
# one computed for what was typed.
_armin_set_buffer() {
  BUFFER=$1
  CURSOR=${#BUFFER}
  _armin_seen="$CURSOR:$BUFFER"
}

# Moves the selection and puts it on the line.
_armin_move() {
  local -i n=${#_armin_items}
  _armin_sel=$(( (_armin_sel - 1 + $1 + n) % n + 1 ))
  _armin_set_buffer "${_armin_lines[_armin_sel]}"
  _armin_render
}

# Esc puts back what was typed before Tab filled the line.
_armin_dismiss() {
  (( _armin_sel )) && _armin_set_buffer "$_armin_typed"
  _armin_close
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

# Tab with the menu closed opens it, with the first row on the line: zsh's
# completions for the word you're on, or, on an empty line, what usually comes
# next. With nothing to offer, Tab does what it did before.
_armin_next() {
  if (( _armin_open )); then
    _armin_move 1
    return
  fi
  if [[ -z $BUFFER ]]; then
    _armin_predict
    (( ${#_armin_items} )) && { _armin_move 1; return }
  elif _armin_complete; then
    return
  fi
  [[ -n ${_armin_fallback[next]} ]] && zle "${_armin_fallback[next]}"
}
_armin_prev()   { _armin_or_fallback prev   _armin_move -1 }
_armin_down()   { _armin_or_fallback down   _armin_move 1 }
_armin_up()     { _armin_or_fallback up     _armin_move -1 }
_armin_escape() { _armin_or_fallback escape _armin_dismiss }

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
  # A typo that never ran doesn't say what comes next.
  (( exit == 126 || exit == 127 )) || _armin_previous=$_armin_cmd
  _armin_cmd=''
}

# precmd first in line, so $? is still the command's.
precmd_functions=( _armin_precmd ${precmd_functions:#_armin_precmd} )
add-zsh-hook preexec _armin_preexec

# Have the daemon ready before the first keystroke.
_armin_ask ping || _armin_spawn
