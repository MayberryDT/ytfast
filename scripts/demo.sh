#!/bin/sh
# Short videos of Music for showing people, recorded from the real app on this
# machine's desktop:
#
#   scripts/demo.sh [flight|stage|themes|audition|mix|keys|sound ...]
#
#   flight    covers fly: Home's cards, a search, the song to the player, Now
#             Playing, the jump to the most replayed part, timed lyrics
#   stage     the most-replayed ridge, Stage with big timed lyrics, and back
#   themes    Omarchy themes switched live, then covers painted in the theme
#   audition  Alt held over songs plays them over the ducked current one
#   mix       Smooth mixes: a radio's song blends into the next
#   keys      Play anything (Ctrl+K), the shortcuts, Play next, Add to queue,
#             Up next reordered by dragging
#   sound     equalizer presets, the sleep timer
#
# With no arguments it records them all. Each runs signed out (an empty
# stand-in home with the desktop's Omarchy theme linked in), so nothing from an
# account shows. The `demo-*` scenarios in src/e2e/demo.rs cue the stretches to
# keep; the screen and the desktop's sound are recorded with wf-recorder and
# cut to them. Writes artifacts/demo/<UTC time>/<clip>.mp4 (H.264 and AAC)
# with a silent copy, <clip>-silent.mp4. `themes` switches the desktop's theme
# and back.
set -eu
cd "$(dirname "$0")/.."
clips="${*:-flight stage themes audition mix keys sound}"
out="$PWD/artifacts/demo/$(date -u +%Y%m%dT%H%M%SZ)"
mkdir -p "$out"

systemd-run --user --wait --collect --pipe -q -p MemoryHigh=4500M -p MemoryMax=5G \
	--working-directory="$PWD" -- cargo build --release -j 4 --features e2e >"$out/build.txt" 2>&1 ||
	{ tail -40 "$out/build.txt"; exit 1; }

systemctl --user stop ytfast-app 2>/dev/null || true
pkill -x ytfast 2>/dev/null && sleep 1 || true

[ -n "${HYPRLAND_INSTANCE_SIGNATURE:-}" ] ||
	HYPRLAND_INSTANCE_SIGNATURE="$(ls -t "${XDG_RUNTIME_DIR:-/run/user/$(id -u)}/hypr" 2>/dev/null | head -1)"
export HYPRLAND_INSTANCE_SIGNATURE
refresh="$(hyprctl monitors -j 2>/dev/null | python3 -c 'import json,sys; print(json.load(sys.stdin)[0]["refreshRate"])' 2>/dev/null || echo 60)"

# One stand-in home for every clip, so pages and streams fetched by one are
# ready for the next; each clip starts without saved settings or a session.
fake="$out/home"
mkdir -p "$fake/.config" "$fake/.cache" "$fake/.local/state" "$fake/.local/share"
ln -s "$HOME/.config/omarchy" "$fake/.config/omarchy"
ln -s "$HOME/.local/state/omarchy" "$fake/.local/state/omarchy"
monitor="$(pactl get-default-sink).monitor"

for clip in $clips; do
	dir="$out/$clip"
	mkdir -p "$dir"
	rm -f "$fake/.config/ytfast/settings.json" "$fake/.cache/ytfast/session.json"
	recorder="ytfast-demo-recorder-$$-$clip"
	date +%s%3N >"$dir/recording-started-ms"
	systemd-run --user --collect -q --unit="$recorder" -- \
		wf-recorder -y -a="$monitor" -c libx264 -p preset=ultrafast -p crf=12 -f "$dir/raw.mkv"
	systemd-run --user --wait --collect -q --unit="ytfast-demo-$$-$clip" -p RuntimeMaxSec=900 \
		-E YTFAST_E2E_DIR="$dir" -E YTFAST_E2E_SCENARIO="demo-$clip" -E RUST_LOG=ytfast=debug,warn \
		-E YTFAST_E2E_REFRESH_HZ="$refresh" -E HOME="$fake" -E XDG_CONFIG_HOME="$fake/.config" \
		-E XDG_CACHE_HOME="$fake/.cache" -E XDG_STATE_HOME="$fake/.local/state" \
		-E XDG_DATA_HOME="$fake/.local/share" -E YTFAST_E2E_REAL_HOME="$HOME" \
		-- "$PWD/target/release/ytfast" || true
	systemctl --user kill -s INT "$recorder" 2>/dev/null || true
	while systemctl --user is-active -q "$recorder"; do sleep 0.2; done
	cp "$fake/.cache/ytfast/ytfast.log" "$dir/app.log" 2>/dev/null || true
	python3 - "$dir" "$out/$clip.mp4" "$out/$clip-silent.mp4" <<'EOF' || echo "$clip: no video ($dir/log.txt)"
import json, subprocess, sys
dir, video, silent = sys.argv[1:4]
summary = json.load(open(f"{dir}/summary.json"))
started = int(open(f"{dir}/recording-started-ms").read())
cues = summary["measurements"].get("cues", [])
segments, roll = [], None
for cue in cues:
    at = (cue["unix_ms"] - started) / 1000
    if cue["cue"] == "roll":
        roll = at
    elif cue["cue"] == "cut" and roll is not None:
        segments.append((roll, at))
        roll = None
if not segments:
    sys.exit(f"{dir}: no stretch cued to keep")
parts, labels = [], ""
last = len(segments) - 1
for i, (start, end) in enumerate(segments):
    length = end - start
    fade_in = 0.3 if i == 0 else 0.08
    fade_out = 0.6 if i == last else 0.12
    parts.append(
        f"[0:v]trim=start={start:.3f}:end={end:.3f},setpts=PTS-STARTPTS[v{i}];"
        f"[0:a]atrim=start={start:.3f}:end={end:.3f},asetpts=PTS-STARTPTS,"
        f"afade=t=in:d={fade_in},afade=t=out:st={length - fade_out:.3f}:d={fade_out}[a{i}]"
    )
    labels += f"[v{i}][a{i}]"
graph = ";".join(parts) + f";{labels}concat=n={len(segments)}:v=1:a=1[cv][a];[cv]fps=60[v]"
encode = ["-c:v", "libx264", "-preset", "slow", "-crf", "18", "-pix_fmt", "yuv420p",
          "-movflags", "+faststart"]
subprocess.run(["ffmpeg", "-v", "error", "-y", "-i", f"{dir}/raw.mkv", "-filter_complex", graph,
                "-map", "[v]", "-map", "[a]", *encode, "-c:a", "aac", "-b:a", "192k", video], check=True)
subprocess.run(["ffmpeg", "-v", "error", "-y", "-i", video, "-map", "0:v", "-c", "copy", silent], check=True)
seconds = sum(e - s for s, e in segments)
print(f"{video}: {seconds:.1f} s from {len(segments)} stretch(es); passed={summary['passed']} {summary['failures']}")
EOF
	rm -f "$dir/raw.mkv"
done
echo "videos: $out"
