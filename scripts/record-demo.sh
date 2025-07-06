#!/usr/bin/env bash
# Records docs/demo.gif (and docs/demo.mp4) by driving logship with scripted keystrokes.
#
# macOS only. The terminal running this needs two permissions under
# System Settings → Privacy & Security:
#   - Screen Recording  (ffmpeg screen capture)
#   - Accessibility     (sending keystrokes via System Events)
# After granting either permission, quit and reopen the terminal.
# Requires ffmpeg (`brew install ffmpeg`).
set -euo pipefail

cd "$(dirname "$0")/.."
WORK=$(mktemp -d)
LOG="$WORK/ship.log"
X=80 Y=80 W=1400 H=860
DURATION=42

cleanup() {
  kill "${LIVE_PID:-}" "${APP_PID:-}" "${REC_PID:-}" 2>/dev/null || true
  rm -rf "$WORK"
}
trap cleanup EXIT

die() { echo "✗ $*" >&2; exit 1; }

# Runs a command, killing it after $1 seconds. Fails if it times out or errors.
with_timeout() {
  local secs=$1; shift
  "$@" & local pid=$!
  ( sleep "$secs"; kill "$pid" 2>/dev/null ) & local watchdog=$!
  wait "$pid"; local rc=$?
  kill "$watchdog" 2>/dev/null || true
  return $rc
}

echo "🔎 checking permissions"
command -v ffmpeg >/dev/null || die "ffmpeg not found (brew install ffmpeg)"
[[ $(osascript -e 'tell application "System Events" to get UI elements enabled' 2>/dev/null) == true ]] ||
  die "Accessibility permission missing for this terminal (System Settings → Privacy & Security → Accessibility), then restart the terminal"
# -list_devices always exits non-zero (the dummy input fails), so don't let pipefail kill us.
SCREEN=$( (ffmpeg -hide_banner -f avfoundation -list_devices true -i "" 2>&1 || true) |
  sed -n 's/.*\[\([0-9]*\)\] Capture screen 0.*/\1/p' | head -1)
[[ -n $SCREEN ]] || die "no capturable screen found via ffmpeg avfoundation"
with_timeout 10 ffmpeg -hide_banner -loglevel error -y -f avfoundation -pixel_format uyvy422 \
  -i "$SCREEN:none" -frames:v 1 "$WORK/probe.png" 2>/dev/null ||
  die "Screen Recording permission missing for this terminal (System Settings → Privacy & Security → Screen & System Audio Recording), then restart the terminal"
# Captured frames are in pixels, window coordinates in points (2x on Retina).
DESKTOP_W=$(osascript -e 'tell application "Finder" to get item 3 of (get bounds of window of desktop)')
FRAME_W=$(ffprobe -v error -select_streams v:0 -show_entries stream=width -of csv=p=0 "$WORK/probe.png")
SCALE=$((FRAME_W / DESKTOP_W))
(( SCALE >= 1 )) || SCALE=1

echo "⚓ building release binary"
cargo build --release --quiet

echo "📜 generating 5M-line log"
python3 scripts/gen-log.py "$LOG" --lines 5000000

key() { osascript -e "tell application \"System Events\" to $1"; }
type_slow() {
  local text=$1 ch
  for ((i = 0; i < ${#text}; i++)); do
    ch=${text:i:1}
    # Escape for an AppleScript string literal.
    [[ $ch == '\' || $ch == '"' ]] && ch="\\$ch"
    key "keystroke \"$ch\""
    sleep 0.07
  done
}
pause() { sleep "$1"; }

echo "🏴‍☠️ launching"
./target/release/logship "$LOG" &
APP_PID=$!
sleep 2
key "set frontmost of process \"logship\" to true"
key "tell process \"logship\" to set position of window 1 to {$X, $Y}"
key "tell process \"logship\" to set size of window 1 to {$W, $H}"
sleep 1

echo "🎥 recording ${DURATION}s"
ffmpeg -hide_banner -loglevel error -y -f avfoundation -pixel_format uyvy422 -framerate 30 \
  -capture_cursor 0 -i "$SCREEN:none" -t "$DURATION" \
  -vf "crop=$((W * SCALE)):$((H * SCALE)):$((X * SCALE)):$((Y * SCALE))" \
  -c:v libx264 -preset ultrafast -crf 16 -pix_fmt yuv420p "$WORK/demo.mov" </dev/null &
REC_PID=$!
pause 1.5

# Scroll through five million lines.
for _ in 1 2 3 4 5 6; do key "key code 121"; pause 0.25; done          # page down
key "keystroke \"G\""; pause 1.2                                        # bottom
key "keystroke \"g\""; pause 1                                          # top

# Search (filter mode).
key "keystroke \"f\" using command down"; pause 0.4
type_slow "connection reset"; pause 1.5
for _ in 1 2 3; do key "key code 36"; pause 0.5; done                   # enter: next match

# Switch to highlight mode, jump through matches with context.
key "keystroke \"f\" using {command down, option down}"; pause 1.2
for _ in 1 2 3; do key "key code 36"; pause 0.6; done

# Regex search.
key "key code 51 using command down"; pause 0.3                         # clear query
key "keystroke \"r\" using {command down, option down}"; pause 0.3      # regex on
type_slow 'ship.*bearing=9\d\d'; pause 1.5
key "key code 51 using command down"; pause 0.3
key "keystroke \"r\" using {command down, option down}"
key "keystroke \"f\" using {command down, option down}"; pause 0.3      # back to filter mode
key "key code 48"; pause 0.5                                            # tab → list

# Live tail with follow.
python3 scripts/gen-log.py "$LOG" --live --rate 25 &
LIVE_PID=$!
key "keystroke \"f\" using {command down, shift down}"; pause 5

# Light mode and back.
key "keystroke \"d\" using {command down, shift down}"; pause 2.5
key "keystroke \"d\" using {command down, shift down}"; pause 1.5

wait "$REC_PID"

echo "🎞  encoding"
mkdir -p docs
ffmpeg -loglevel error -y -i "$WORK/demo.mov" \
  -vf "fps=30,scale=1400:-2:flags=lanczos" -c:v libx264 -crf 26 -pix_fmt yuv420p -movflags +faststart \
  docs/demo.mp4
ffmpeg -loglevel error -y -i "$WORK/demo.mov" \
  -vf "fps=12,scale=1100:-1:flags=lanczos,split[a][b];[a]palettegen=stats_mode=diff[p];[b][p]paletteuse=dither=bayer:bayer_scale=5:diff_mode=rectangle" \
  docs/demo.gif

ls -lh docs/demo.gif docs/demo.mp4
echo "🦜 done — docs/demo.gif is embedded in README.md"
