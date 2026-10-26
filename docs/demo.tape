# The terminal part of the README's demo. docs/demo.sh sets up the shell it
# records, puts the title card in front and writes docs/demo.gif; run that
# instead.
#
# The shell is a throwaway one in target/demo: its own history, config and
# git repository, so nobody's real commands end up in the GIF.

Output target/demo-terminal.gif

Set Shell "zsh"
Set FontSize 18
Set Width 1200
Set Height 640
Set Padding 20
Set Theme "TokyoNight"
Set WindowBar Colorful
Set TypingSpeed 70ms
Set CursorBlink false

Hide
Type `ZDOTDIR="$PWD/target/demo/zsh" exec zsh -i` Enter
Sleep 2s
# Ctrl-L, not `clear`, which would land in the history.
Ctrl+L
Sleep 500ms
Show

# Stage a change, then Tab on the empty line: what usually comes next, with
# the panel beside it.
Type "git add ."
Sleep 500ms
Enter
Sleep 1s
Tab
Sleep 3s
Enter
Sleep 1.5s

# Typing opens the menu of your history; Tab moves through it and puts the
# row on the line.
Type "car"
Sleep 1.5s
Tab
Sleep 1.2s
Tab
Sleep 2.5s
Ctrl+U
Sleep 800ms

# zsh's own completions, the branch you switch to most first.
Type "git switch "
Sleep 1s
Escape
Sleep 400ms
Tab
Sleep 2.5s
Enter
Sleep 1.5s

# Flags with their descriptions; a long list scrolls.
Type "ls -"
Sleep 800ms
Tab
Sleep 1.5s
Tab
Sleep 500ms
Tab
Sleep 500ms
Tab
Sleep 2.5s
Ctrl+U
Sleep 1s
