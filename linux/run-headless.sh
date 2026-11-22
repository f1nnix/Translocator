#!/bin/sh
# Inside the dev container (linux/dev.sh run): builds the GTK app, starts it
# under Xvfb with a session bus in a fresh home with sample folders, runs
# $TL_SCRIPT (or waits $TL_WAIT seconds), screenshots into /shots, quits.
set -eu

cargo build -p translocator-gtk
bin=/target/debug/translocator

export HOME=/tmp/home
rm -rf "$HOME" && mkdir -p "$HOME"
cd "$HOME"
mkdir -p Documents Downloads Desktop Music Pictures Projects/translocator/src \
    Projects/website .config
for name in README.md notes.txt todo.txt report-2026.pdf budget.csv \
    photo-001.jpg photo-002.jpg song.mp3 archive.tar.gz build.sh Cargo.toml; do
    head -c $(( ($(printf '%s' "$name" | wc -c) * 977) % 40000 + 120 )) /dev/urandom > "Documents/$name"
done
printf 'fn main() {\n    println!("hello");\n}\n' > Projects/translocator/src/main.rs
printf '[package]\nname = "demo"\n' > Projects/translocator/Cargo.toml
printf '# Website\n' > Projects/website/index.md
chmod +x Documents/build.sh
# Settings to start with (a colour scheme, say), as the settings file holds them.
if [ -n "${TL_SETTINGS:-}" ]; then
    mkdir -p .config/Translocator
    printf '%s\n' "$TL_SETTINGS" > .config/Translocator/settings.json
fi
ln -s Documents/notes.txt notes-link.txt
printf 'secret\n' > .hidden-file
mkdir -p .cache
for i in $(seq 1 40); do : > "Downloads/file-$(printf %03d "$i").log"; done

export GDK_BACKEND=x11 GSK_RENDERER=cairo NO_AT_BRIDGE=1
export DISPLAY=:99
Xvfb :99 -screen 0 1400x880x24 -nolisten tcp >/tmp/xvfb.log 2>&1 &
xvfb=$!
for _ in $(seq 1 50); do
    xdpyinfo -display :99 >/dev/null 2>&1 && break
    sleep 0.1
done

shot() {
    import -display :99 -window root "/shots/$1.png"
    echo "shot: $1.png"
}
win() { xdotool search --onlyvisible --name Translocator | head -1; }
# Synthetic events sent to a window are not trusted; with no window
# manager, give the window the X focus and type through XTEST instead.
focus() { w=$(win); [ -n "$w" ] && xdotool windowfocus --sync "$w" 2>/dev/null || true; }
key() { focus; xdotool key --delay 80 "$@"; sleep 0.4; }
text() { focus; xdotool type --delay 80 "$*"; sleep 0.4; }
click() { focus; xdotool mousemove "$1" "$2" click "${3:-1}"; sleep 0.4; }
drag() {
    focus
    xdotool mousemove "$1" "$2" mousedown 1
    for step in 1 2 3 4 5 6 7 8; do
        xdotool mousemove $(( $1 + ($3 - $1) * step / 8 )) $(( $2 + ($4 - $2) * step / 8 ))
        sleep 0.08
    done
    sleep 0.3
    xdotool mouseup 1
    sleep 0.6
}
pause() { sleep "$1"; }

dbus-run-session -- sh -c "exec $bin" >/tmp/app.log 2>&1 &
app=$!
# The window appears, then the panes list their folders.
for _ in $(seq 1 100); do
    [ -n "$(win)" ] && break
    sleep 0.2
done
w=$(win || true)
if [ -n "$w" ]; then
    xdotool windowmove "$w" 0 0 windowsize "$w" 1400 880 2>/dev/null || true
    focus
fi

if [ "${TL_SCRIPT:-/dev/null}" != /dev/null ]; then
    sleep 2
    . "$TL_SCRIPT"
else
    sleep "${TL_WAIT:-6}"
    shot main
fi

kill "$app" 2>/dev/null || true
sleep 0.5
cp /tmp/app.log /shots/app.log
echo "--- app log (all of it in app.log beside the screenshots)"
grep -v "^$" /tmp/app.log | tail -n 30
kill "$xvfb" 2>/dev/null || true
