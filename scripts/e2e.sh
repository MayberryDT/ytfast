#!/bin/sh
# The scripted end-to-end runs of ytfast (docs/SPEC.md § Completion evidence),
# on this machine's desktop with the signed-in browser session. Run them from
# the repository, on a machine you don't mind it taking over:
#
#   scripts/e2e.sh [journey|recovery|offline|theme|showcase|motion|pages|desktop|account|engine|engine-restore|surfaces|deck|control]
#
#   journey   every surface, playback (Premium, gapless, seek, next), history
#   recovery  signed out → Reconnect; a stream that fails once, one that keeps failing
#   offline   no connection: saved copies, offline account state
#   theme     the Omarchy theme changes while ytfast runs (changes it back)
#   showcase  README pictures with no account (public YouTube Music)
#   pages     Now Playing and pages: cover colours (dark and light theme), timed
#             lyrics, History, Home moods, artist See all, recent searches
#   desktop   MPRIS via playerctl, playing on after close, `ytfast show|next|open`,
#             a pasted link, the mini player, a song-change notification
#             (needs playerctl and a notification daemon)
#   account   like, library save, subscription, playlist create/edit/delete,
#             each confirmed by refetching; leaves the account as it found it
#   engine    cold-click start times, queue edits played as shown, equalizer and
#             loudness levelling in mpv, sleep timer; leaves a session (sets the
#             volume to 63 and the Rock preset until engine-restore)
#   engine-restore  run right after engine: the session came back; puts settings back
#   surfaces  the most-replayed ridge (rest, hover) and the jump to the peak; Stage
#             (flight, timed lyrics, frame times, chrome fading); theme-painted
#             covers under a light theme and back (changes the theme back)
#   deck      Audition (Alt held over a song: main deck ducked and back, both songs
#             moving, queue unchanged) and Smooth mixes (a radio's equal-power
#             blend across two mpv decks; Next during a blend completes it, Next
#             again moves on; an album stays gapless on one); puts the volume,
#             repeat, shuffle and the setting back
#   control   the keyboard map and `?` overlay, Play anything (Ctrl+K, type, Enter
#             timed to audible), right-click menus (Play next, Add to queue, Go to
#             artist, Copy link), the queue saved as a playlist and deleted again;
#             leaves the account, volume, shuffle and repeat as found
#
# Each builds with the `e2e` feature, runs the real app through the steps in
# src/e2e.rs, and leaves screenshots, logs and summary.json in
# artifacts/e2e/<UTC time>-<scenario>/ (gitignored: they show account data;
# showcase's don't, and the README's pictures in docs/screenshots come from it).
set -eu
cd "$(dirname "$0")/.."
scenario="${1:-journey}"
stamp="$(date -u +%Y%m%dT%H%M%SZ)-$scenario"
dir="$PWD/artifacts/e2e/$stamp"
mkdir -p "$dir"
commit="$(git rev-parse --short HEAD)$(git diff --quiet HEAD -- src Cargo.toml || echo -dirty)"

systemd-run --user --wait --collect --pipe -q -p MemoryHigh=5G -p MemoryMax=6G \
	--working-directory="$PWD" -E YTFAST_COMMIT="$commit" -- cargo build -j 4 --features e2e >"$dir/build.txt" 2>&1 ||
	{ tail -40 "$dir/build.txt"; exit 1; }

# One instance at a time: a running ytfast would just be asked to show itself.
systemctl --user stop ytfast-app 2>/dev/null || true
pkill -x ytfast 2>/dev/null && sleep 1 || true

# The display's refresh rate, for frame-time checks (a virtual display may run at 30 Hz).
# Over SSH Hyprland's instance isn't in the environment: use the newest one.
[ -n "${HYPRLAND_INSTANCE_SIGNATURE:-}" ] ||
	HYPRLAND_INSTANCE_SIGNATURE="$(ls -t "${XDG_RUNTIME_DIR:-/run/user/$(id -u)}/hypr" 2>/dev/null | head -1)"
export HYPRLAND_INSTANCE_SIGNATURE
refresh="$(hyprctl monitors -j 2>/dev/null | python3 -c 'import json,sys; print(json.load(sys.stdin)[0]["refreshRate"])' 2>/dev/null || echo 60)"
set -- -E YTFAST_E2E_DIR="$dir" -E YTFAST_E2E_SCENARIO="$scenario" -E RUST_LOG=ytfast=debug,warn -E YTFAST_E2E_REFRESH_HZ="$refresh"
log="${XDG_CACHE_HOME:-$HOME/.cache}/ytfast/ytfast.log"
case "$scenario" in
recovery)
	# A copy of the browser profile whose session cookies no longer hold a valid
	# sign-in (an expired session); the run swaps in the real one before Reconnect.
	fake="$dir/home"
	browser=""
	for b in google-chrome chromium BraveSoftware/Brave-Origin BraveSoftware/Brave-Browser; do
		[ -f "$HOME/.config/$b/Default/Cookies" ] && { browser="$b"; break; }
	done
	[ -n "$browser" ] || { echo "no Chromium-family browser profile to copy" >&2; exit 1; }
	copy="$fake/.config/$browser"
	mkdir -p "$copy/Default" "$fake/.cache"
	cp "$HOME/.config/$browser/Local State" "$copy/"
	cp "$HOME/.config/$browser/Default/Cookies" "$copy/Default/"
	python3 - "$copy/Default/Cookies" <<'EOF'
import sqlite3, sys
db = sqlite3.connect(sys.argv[1])
names = ("SID", "HSID", "SSID", "APISID", "LOGIN_INFO", "SIDCC", "__Secure-1PSID", "__Secure-3PSID",
         "__Secure-1PSIDTS", "__Secure-3PSIDTS", "__Secure-1PSIDCC", "__Secure-3PSIDCC")
db.execute(f"UPDATE cookies SET value = 'expired', encrypted_value = X'' WHERE name IN ({','.join('?' * len(names))})", names)
db.commit()
EOF
	set -- "$@" -E HOME="$fake" -E XDG_CONFIG_HOME="$fake/.config" -E XDG_CACHE_HOME="$fake/.cache" \
		-E YTFAST_E2E_REAL_HOME="$HOME" -E YTFAST_E2E_BROWSER="$browser"
	log="$fake/.cache/ytfast/ytfast.log"
	;;
offline)
	# Every request goes to a proxy that isn't there.
	set -- "$@" -E HTTPS_PROXY=http://127.0.0.1:9 -E HTTP_PROXY=http://127.0.0.1:9 -E ALL_PROXY=http://127.0.0.1:9
	;;
showcase)
	# No browser profiles at all: ytfast stays signed out. The desktop's
	# Omarchy theme is linked in so the colours are the real ones.
	fake="$dir/home"
	mkdir -p "$fake/.config" "$fake/.cache" "$fake/.local/state" "$fake/.local/share"
	ln -s "$HOME/.config/omarchy" "$fake/.config/omarchy"
	ln -s "$HOME/.local/state/omarchy" "$fake/.local/state/omarchy"
	set -- "$@" -E HOME="$fake" -E XDG_CONFIG_HOME="$fake/.config" -E XDG_CACHE_HOME="$fake/.cache" \
		-E XDG_STATE_HOME="$fake/.local/state" -E XDG_DATA_HOME="$fake/.local/share" -E YTFAST_E2E_REAL_HOME="$HOME"
	log="$fake/.cache/ytfast/ytfast.log"
	;;
esac

# The user manager has the desktop session's environment (Wayland, D-Bus).
systemd-run --user --wait --collect -q --unit="ytfast-e2e-$stamp" -p RuntimeMaxSec=1200 "$@" -- "$PWD/target/debug/ytfast" || true
cp "$log" "$dir/app.log" 2>/dev/null || true
echo "artifacts: $dir"
cat "$dir/summary.json"
