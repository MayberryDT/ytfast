# ytfast specification

ytfast is a native YouTube Music client for Omarchy: a Rust + egui desktop app that looks and works like YouTube Music, starts instantly and keeps playing reliably. It follows the pattern of [ZapFast](https://github.com/crmne/zapfast) (WhatsApp) and [Spotifast](https://github.com/crmne/spotifast) (Spotify): no browser engine, no telemetry, no hosted backend.

## Why it exists

Earlier YouTube Music players for Omarchy didn't look or feel like YouTube Music, showed only a few surfaces, and had unreliable playback, including sign-in silently going stale. ytfast has to get all three right. Every surface is populated with real account content; a surface that would be empty is not a reason to remove it, but a reason to load it properly.

## Decisions

| Topic | Decision |
| --- | --- |
| Product | Lightweight native YouTube **Music** client. |
| Platform | Linux / Omarchy (Hyprland, Wayland). No macOS/Windows work. |
| Stack | Rust + egui/eframe, built like ZapFast: native window, `fastframe` crates, same egui/winit fork pins. |
| Name | Shown to people as **Music** (launcher, window title, sidebar); `ytfast` everywhere internal (binary, window class, config and cache folders). |
| Account | Signed in with cookies read from a signed-in Chromium-family browser profile. By default that's the most recently used one; Settings lists the signed-in profiles (different browsers can hold different Google accounts) and the choice is remembered. |
| Media | Audio only. No music videos. Highest available quality (with Premium, Opus at ~256 kbps, itag 774). |
| Look | Similar to YouTube Music. Colours follow the active Omarchy theme; never hard-coded. |
| Write-back | Only reporting plays to YouTube Music history. No likes/dislikes, no playlist creation or editing. |
| Desktop integration | Not in v1: no MPRIS/media keys, no tray or background playing after the window closes, no notifications. |

## Journeys

### Open and browse

- Launching ytfast shows a normal native window within a second, laid out like YouTube Music: left navigation (Home, Explore, Library), a search field at the top, the content area, and a persistent player bar at the bottom.
- **Home** shows the account's YouTube Music home shelves (e.g. Quick picks, Listen again, mixes and other personalised shelves) with cover art, in YouTube Music's order. Shelves scroll horizontally; more shelves load while scrolling down.
- **Explore** shows new releases, charts, and moods & genres, each opening its page.
- **Library** shows the account's playlists, songs, albums and artists (including Liked Music), switchable like YouTube Music's library chips.
- **Search** gives suggestions while typing and results grouped like YouTube Music (top result, songs, albums, artists, playlists), with a per-type view.
- **Album, artist and playlist pages** show header art and metadata, a Play/Shuffle action, and the full track list (long playlists load progressively). Artist pages show top songs, albums, singles and related artists.
- Every row or card that represents music can be played directly, and every artist or album name in a row, the player bar or Now Playing opens its page.
- The last loaded Home, Library and pages appear immediately from a local cache on the next launch and then refresh in place, so the app never opens to an empty or spinner-only screen.

### Play

- Clicking a song starts it and makes the surrounding list (album, playlist, search results, shelf) its queue. Starting a mix or radio loads YouTube Music's generated queue.
- The player bar shows cover, title, artist, a seek bar with elapsed and total time, previous, play/pause and next, plus shuffle, repeat and volume.
- **Now Playing** expands from the player bar into a large cover view with YouTube Music's three tabs: **Up next** (the queue, including autoplay continuation when enabled), **Lyrics** (when YouTube Music has them; says so when it doesn't), and **Related**.
- When the queue reaches its end with autoplay on, playback continues with YouTube Music's radio for the last track.
- Tracks follow each other without noticeable gaps. Seeking is responsive. Upcoming tracks are prepared ahead so a normal track change doesn't wait on network resolution.
- Quality: the highest audio format the account can get (Premium Opus ~256 kbps when offered; otherwise the best available). Settings and Now Playing show the format actually playing. Resolving a stream takes several seconds, so upcoming queue tracks and songs the pointer rests on are prepared ahead; a cold click on an unprepared song shows loading in place until it starts.
- Plays count in the account's YouTube Music history, so Home's Listen again and recommendations learn from ytfast plays.

### Sign-in and recovery

- ytfast reads YouTube cookies from a signed-in Chromium-family browser profile's live cookie store (Brave Origin, Brave, Google Chrome or Chromium) on launch and when YouTube rejects the session. It never asks you to paste headers or export files. The browser is never restarted or modified.
- Signed-in state is established by an account-only request succeeding, never by a cookie file existing. The UI shows the account (name/avatar) when signed in.
- If the session is invalid or expired, ytfast says so plainly and offers **Reconnect** (re-read the browser cookies). Public browsing and playback keep working in the meantime at the best available quality.
- If a track fails to resolve or stream, ytfast retries once with fresh data, then skips to the next track and shows a readable error with a Copy button. Playback never stalls silently.
- Network loss shows an offline state on affected content. Cached pages stay browsable, and playback resumes when the connection returns.
- ytfast is single-instance: launching it again focuses the existing window.

## Fixed architecture and protected constraints

- Native Rust + egui (eframe, glow backend) on the `crmne/egui apps-0.36` and `crmne/winit apps-0.30` fork revisions used by ZapFast and Spotifast. Reuse `fastframe` crates (theme, fonts, icons, text, log) at one pinned tag. No browser engine or webview anywhere.
- Colours come only from the active Omarchy theme through a `fastframe-theme` palette, following theme changes live. The UI must read well across any Omarchy theme.
- Credentials: cookie values never enter logs, crash reports, the repository or world-readable files. Any derived cookie file is 0600 in the user's runtime directory. The Chromium cookie key is treated the same way.
- No telemetry and no ytfast-operated services. The only network peers are YouTube/Google endpoints and the stream CDN.

## Exclusions (v1)

Video playback; MPRIS, media keys, tray, background playing after window close, notifications; likes/dislikes, library saving, playlist creation or editing; downloads/offline mode; podcasts; uploads; macOS/Windows; packaged releases (AUR, binaries, self-update); being signed in to more than one account at once; non-Chromium browsers' cookies.

## Completion evidence

A version is complete when the following are observed with a real YouTube Music Premium account:

1. **Journeys:** a scripted E2E run drives the real app through Home → shelf item play → Now Playing (Up next, Lyrics, Related) → Explore → Library (each chip) → album, artist and playlist pages → search, with a screenshot of each state and an interaction log, written to an artifact directory by a repeatable command.
2. **Playback:** a real queue plays across at least three track changes and a seek, recording the itag/codec actually streamed (774 with Premium), track-change gaps, and a continuing position.
3. **History:** a song played in ytfast appears at the top of the account's history as fetched afterwards from YouTube Music.
4. **Recovery:** with an invalidated cookie set, ytfast shows the signed-out state and Reconnect; Reconnect restores the account without restarting the browser. A forced stream failure retries, then skips with a visible error.
5. **Theme:** screenshots under a light and a dark Omarchy theme show the colours following a live theme switch.
6. **Speed:** first window with cached content in under 1 s; idle memory in the low hundreds of MB. Measured values are recorded.
