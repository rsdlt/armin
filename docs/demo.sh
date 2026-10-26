#!/bin/sh
# Makes docs/demo.gif, the README's demo: a title card made from docs/logo.svg,
# then armin in a throwaway shell (docs/demo.tape). Run it from the repository
# root. Needs vhs, rsvg-convert and ffmpeg.
set -eu

cargo build --release
demo="$PWD/target/demo"
rm -rf "$demo"
mkdir -p "$demo/data" "$demo/zsh" "$demo/armin/src"

# A repository to work in, with a change waiting to be committed and the
# branches the history switches between.
(
    cd "$demo/armin"
    git init -q
    git config user.name Armin
    git config user.email armin@survey.corps
    touch src/main.rs src/rank.rs
    echo "# armin" > README.md
    git add . && git commit -qm "Start armin"
    git branch feature/panel && git branch fix/scroll
    echo "A menu of your next move." >> README.md
)

# A few days of history, as if armin had recorded it: the commands, how often,
# and `git commit` right after each `git add .`.
now=$(date +%s)
t=$((now - 3 * 86400))
{
    echo "# armin history v1"
    for spec in "cargo test:24" "cargo build --release:9" "cargo clippy --all-targets:6" \
        "git status:20" "git switch fix/scroll:5" "git switch main:2" "git log --oneline:5" \
        "git push:8" "nvim src/rank.rs:7"; do
        command=${spec%:*}
        n=${spec##*:}
        while [ "$n" -gt 0 ]; do
            t=$((t + 900))
            printf '%s\t1200\t0\tdemo\t%s\t%s\n' "$t" "$demo/armin" "$command"
            n=$((n - 1))
        done
    done
    for _ in 1 2 3 4 5 6; do
        t=$((t + 600))
        printf '%s\t300\t0\tdemo\t%s\tgit add .\n' "$t" "$demo/armin"
        printf '%s\t2100\t0\tdemo\t%s\tgit commit -m "wip"\n' "$((t + 40))" "$demo/armin"
    done
} > "$demo/data/history"

# The shell: emacs keys, zsh's completions, this build of armin with its
# defaults (no config file), in the repository. HOME is the demo's, so paths
# read ~/armin.
cat > "$demo/zsh/.zshrc" <<EOF
PS1='%F{green}❯%f '
bindkey -e
export HOME="$demo" ARMIN_DATA_DIR="$demo/data" ARMIN_CONFIG="$demo/no-config.toml"
autoload -Uz compinit && compinit -u -d "$demo/zcompdump"
eval "\$("$PWD/target/release/armin" init zsh)"
cd "$demo/armin"
EOF

vhs docs/demo.tape
ARMIN_DATA_DIR="$demo/data" target/release/armin daemon stop > /dev/null
rsvg-convert docs/demo-card.svg -o target/demo-card.png

# Two seconds of the card fading into the recording, then one palette for the
# whole GIF so both parts keep their colors.
ffmpeg -v error -y \
    -loop 1 -framerate 25 -t 2.4 -i target/demo-card.png \
    -i target/demo-terminal.gif \
    -filter_complex "\
[0:v]fps=25,format=rgb24,setsar=1[card];\
[1:v]fps=25,format=rgb24,setsar=1,scale=1200:640[term];\
[card][term]xfade=transition=fade:duration=0.4:offset=2,split[a][b];\
[a]palettegen=stats_mode=diff[p];\
[b][p]paletteuse=dither=bayer:bayer_scale=5:diff_mode=rectangle" \
    -loop 0 docs/demo.gif

echo "docs/demo.gif: $(du -h docs/demo.gif | cut -f1)"
