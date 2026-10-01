# Integration facts and design

Read this before touching sign-in, playback or the build. Update it when a fact is disproven or a technical choice is made. Facts are dated because YouTube changes.

## Verified facts

- **Toolchain:** Rust 1.98 (ZapFast and Spotifast declare `rust-version = "1.98"`), CMake and a C compiler for the build; at runtime `mpv`, `yt-dlp`, `deno` (yt-dlp's runtime for YouTube's JS challenges) and `secret-tool` (libsecret).
- **Chromium cookie decryption on Linux** (checked 2026-09-30 with Brave Origin 153 and Google Chrome, both using `--password-store=gnome-libsecret`): password = `secret-tool lookup application <brave|chrome|chromium>` with the trailing newline stripped. Key = PBKDF2-HMAC-SHA1(password, `saltysalt`, 1 iteration, 16 bytes); AES-128-CBC with an IV of 16 spaces over the bytes after the `v11` prefix; then PKCS#7 unpadding. From cookie DB schema version 24 the plaintext starts with SHA-256(`host_key`) (32 bytes); strip it. `Local State`'s `os_crypt.portal.prev_init_success = false` means the portal key provider is not in use. Copy the DB before reading; the browser holds it open.
- **yt-dlp's own browser import doesn't help:** `--cookies-from-browser brave:<profile>` decrypted 0 cookies ("cannot decrypt v11 cookies: no key found"), and `brave+gnomekeyring` needs the Python `secretstorage` module, which a system yt-dlp may lack. So ytfast decrypts the cookies itself and gives yt-dlp a Netscape cookie file (mode 0600, a fresh copy per run, because yt-dlp rewrites the files it's given).
- **Exported cookies go stale:** YouTube rotates session cookies, and an exported `cookies.txt` is soon rejected ("cookies are no longer valid… rotated in the browser"). Re-read the live store at launch and on auth failure.
- **Premium formats:** with valid cookies yt-dlp offers `774` (webm/Opus ~271 kbps) and `141` (m4a/AAC ~258 kbps). Without them the best is `251` (Opus ~134–160 kbps).
- **InnerTube with SAPISIDHASH works:** `browse FEmusic_home` as `WEB_REMIX` `1.20260923.01.00` with the cookies and `Authorization: SAPISIDHASH <ts>_<sha1("<ts> <SAPISID> https://music.youtube.com")>` returns `logged_in=1` in about a second.
- **Stream resolution is slow:** `yt-dlp` with cookies resolves itag 774 in 4–10 s per track depending on the CPU (deno solving YouTube's JS challenges dominates; a warm in-process `yt_dlp` still takes ~6 s). **Don't force `player_client=web_music`:** it was faster on 2026-09-30, but on 2026-10-01 it needed a GVS PO token ("Missing required Data Sync ID") and yielded only a 360p video stream. yt-dlp's default clients still return 774.
- **The iOS client's direct URLs don't play:** the InnerTube `player` endpoint as `IOS` (no cookies) answers in ~0.12 s with direct URLs up to 251, and a 1 KB range fetch succeeds, but mpv fails on them after the first bytes.
- **accesskit and broken accessibility buses:** some desktops advertise an AT-SPI bus that refuses connections. accesskit_unix's D-Bus thread then panics on an `unwrap`. With `panic = "abort"` that killed the app at launch, so the release profile unwinds; only that thread ends.
- **Resolve time is yt-dlp's floor** (Dell OptiPlex i5-7500, 2026-10-01): ~3.8–4.9 s per song whether cold, warm in-process (`yt_dlp.YoutubeDL` reused: ~3.8 s) or batched (3 URLs in one run: 12.5 s). Running three resolves at once takes ~4.9 s wall in total, so throughput scales with parallel runs. `player_skip=webpage[,configs]` saves ~1 s but loses every Premium format. The InnerTube `player` endpoint as `IOS` now returns direct URLs that answer 403 at once (PO token enforced), and `ANDROID_VR` answers `LOGIN_REQUIRED`: there is no direct fast path.
- **Loudness:** the `WEB_REMIX` `player` response carries `playerConfig.audioConfig` with `loudnessDb`, `perceptualLoudnessDb` and `trackAbsoluteLoudnessLkfs` (e.g. −10.86 LKFS for "Get Lucky"), in ~0.16 s.
- **Like state:** the `WEB_REMIX` `next` response has `playerOverlays…likeButtonRenderer.likeStatus` (`LIKE`/`DISLIKE`/`INDIFFERENT`) for the requested song; its `likeEndpoint` is the `like/like` endpoint with `target.videoId`.
- **Timed lyrics:** `browse` with the lyrics browse id (`MPLY…`, from `next`) as client `ANDROID_MUSIC` `7.21.50`, **without** cookies or SAPISIDHASH (with them: HTTP 400), returns `timedLyricsData`: lines with `lyricLine` and `cueRange.startTimeMilliseconds`/`endTimeMilliseconds` (109 lines for "Get Lucky"). LRCLIB (`https://lrclib.net/api/get|search`, no key) has `syncedLyrics` in LRC form, matched by track name, artist and duration.
- **Most replayed:** the `WEB` client's `next` on `www.youtube.com` (anonymous, ~0.9 s) has `frameworkUpdates…macroMarkersListEntity.markersList` with `markerType: MARKER_TYPE_HEATMAP` and 100 markers (`startMillis`, `durationMillis`, `intensityScoreNormalized`) for a music track.
- **History and Home moods:** `browse FEmusic_history` returns shelves "Today", "Yesterday", "This week"… (200 rows). Home's header `chipCloudRenderer` holds the mood chips (Energize, Feel good, Workout, Relax, Focus, Party, Commute, Sad, Romance, Sleep, and Podcasts).

## Design

- **Metadata:** ytfast's own InnerTube client (`src/innertube.rs`) and a renderer-family parser (`src/parse.rs`), one path for every surface. rustypipe was not used (it has no Home feed, and a second session layer isn't needed).
- **Sign-in (`src/auth.rs`):** every Chromium-family profile with a cookie store, most recently used first, or the one chosen in Settings (`~/.config/ytfast/settings.json`, `src/settings.rs`). The account is confirmed with an account-menu request before the UI says "signed in".
- **Streams (`src/resolver.rs`):** yt-dlp only (`-f 774/141/251/140/250/249/139`, a per-run 0600 copy of the cookie file), one run at a time, cached until 10 minutes before URL expiry. `prepare` (pointer rests on a song) runs only when no other resolve is running or waiting.
- **Audio (`src/mpv.rs`, `src/backend.rs`):** an `mpv --idle --no-video --no-config` subprocess over JSON IPC. The current track loads with `replace`; the next track, once resolved, is `append`ed, and the move to playlist position 1 is detected and position 0 removed, so mpv's playlist never holds more than two entries. User agents are passed as `loadfile` options using mpv's `%N%` length quoting (they contain commas). Repeat-one uses `loop-file=inf`. A stream error retries once through yt-dlp, then skips with a readable error.
- **Queue invariant:** `loadfile replace` empties mpv's playlist, so any record of a track appended behind the current one must be cleared on every `replace`. As a safety net, mpv going idle while a track is believed queued advances the queue.
- **Stamps on asynchronous results:** the current track's generation, a queue epoch, mpv playlist entry ids and a page request sequence number. Every late answer is checked against them.
- **Offline:** a song that fails twice is skipped only if YouTube is reachable; otherwise playback waits on it and resumes when a reachability check succeeds.
- **History:** after 10 s of play, `WEB_REMIX` `player` (with cookies) → GET `playbackTracking.videostatsPlaybackUrl.baseUrl&ver=2&c=WEB_REMIX&cpn=<16 random>` with the same auth headers.
- **Theme:** `fastframe_theme::Catalog` with `DesktopThemes { slug: "ytfast", omarchy_template: BASE_TEMPLATE, presets: false }`. It renders Omarchy's current colours itself and watches for changes, so no hook install is needed. `ytfast reload-themes` exists for a hook.
- **egui:** egui 0.36 on the crmne fork uses `App::ui(&mut self, ui, frame)` and an optional `App::logic`, not `update`.

## Build

```sh
cargo build --release
cargo fmt --all --check
cargo clippy --all-targets --features e2e -- -D warnings
```

A release build from clean takes about 5 minutes on a 4-core desktop CPU and a few GB of memory. On a small machine, cap it, for example `systemd-run --user --wait --pipe -q -p MemoryMax=5G --working-directory=$PWD -- cargo build --release -j 4`. If two different Rust toolchains are installed, make sure one rustc builds everything (`E0514` otherwise); a machine-local `.cargo/config.toml` (gitignored) can pin `build.rustc`.

## E2E runs

`scripts/e2e.sh [journey|recovery|offline|theme|showcase]` builds with the `e2e` feature and runs the real app on the desktop with the real account, network, yt-dlp and mpv. `src/e2e.rs` clicks controls by their accessible names (`ui::named`) with synthetic pointer events through egui's own input pipeline, types, presses keys, takes framebuffer screenshots and records measurements. It writes `artifacts/e2e/<UTC>-<scenario>/` (gitignored: account data).

- Test-only hooks exist in e2e builds only: `sabotaged(video_id)` hands the resolver an unreachable URL, and `offline()` makes the resolver fail and `Client::reachable` report false.
- `recovery` runs with a stand-in `HOME` holding a copy of a browser profile whose session cookies were overwritten (an expired sign-in). Its reconnect step swaps in a symlink to the real profile.
- `offline` points every request at a dead proxy (`HTTPS_PROXY=http://127.0.0.1:9`).
- `theme` and `showcase` switch the Omarchy theme to `YTFAST_E2E_LIGHT_THEME` (default "Snow") and back.
- `showcase` runs with an empty stand-in `HOME` (the real Omarchy theme linked in), so it is signed out and its screenshots hold no account data. The README's pictures come from it.
- Never run two scenarios at once: each stops any running ytfast.
