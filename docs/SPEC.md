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
| Media | Audio only. No music videos, ever. Highest available quality (with Premium, Opus at ~256 kbps, itag 774). |
| Look | Similar to YouTube Music in places and labels; ytfast's own motion and presentation. Colours follow the active Omarchy theme everywhere, Now Playing and Stage included, and are never hard-coded; only cover art keeps its own colours. |
| Write-back | ytfast acts on the account like YouTube Music does: plays reported to history, likes and dislikes, saving albums and playlists to the library, subscribing to artists, and creating, editing and deleting playlists. |
| Desktop integration | A full Omarchy citizen: MPRIS (media keys, `playerctl`, the shell's media widget), playing on after the window closes with an icon in the tray, a command line that drives the running app, optional song-change notifications, and a compact mini player. |
| Other services | Only LRCLIB, which supplies timed lyrics when YouTube Music has none. No scrobbling or presence services (Last.fm, ListenBrainz, Discord). |

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
- **Now Playing** expands from the player bar into a large cover view with YouTube Music's three tabs: **Up next** (the queue, including autoplay continuation when enabled), **Lyrics** (when YouTube Music has them; says so when it doesn't), and **Related**. A click anywhere on the player bar that isn't one of its controls opens Now Playing, and closes it again. Clicking a song or mix opens Now Playing, as in YouTube Music. A click on Now Playing's cover plays or pauses; a double click opens Stage. Up next shows the radio's chips (All, Popular, Deep cuts, Workout and the like, as YouTube Music offers them) as square-cornered boxes: choosing one makes the songs after the current one that radio, from the current song.
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

### Control

- Keyboard-first: every common action has a shortcut, and `?` lists them. Search, play/pause, seek, previous/next, volume, mute, like, shuffle, repeat, queue, Now Playing, back and Settings never need the mouse. Typing in a field never triggers a shortcut.
- Right-clicking any song, album, artist or playlist opens its actions: Play next, Add to queue, Start radio, Like, Add to playlist, Save to library, Go to album, Go to artist, Copy link.
- The queue can be edited: drag to reorder, remove, clear the upcoming songs, and save the queue as a playlist. Play next and Add to queue keep their order, ahead of autoplay.
- Relaunching restores the last queue, song and position, paused. Volume, shuffle, repeat and autoplay are remembered.
- Loudness is levelled between songs from YouTube's own loudness data, and can be turned off in Settings.
- A sleep timer stops playback after a chosen time or at the end of the current song.
- An equalizer with presets shapes the sound; it is remembered and can be bypassed in one click.
- A cold click on any song starts audio as quickly as the stream source allows; the songs on screen that are likely to be played are prepared ahead.

### Desktop

- ytfast is an MPRIS player: media keys, `playerctl` and the Omarchy bar's media widget see the song, cover, position and controls, and can play, pause, skip, seek and raise the window.
- Closing the window keeps the music playing, with an icon in the bar's tray while the window is closed: a click brings the window back where it was, a middle-click plays or pauses, the wheel sets the volume, and its menu has Play/Pause, Next, Previous, Show Music and Quit. Launching again or MPRIS Raise also brings the window back. Quit (`Ctrl+Q`, the tray menu, `ytfast quit`) ends playback, even while the window sits on a workspace that isn't shown.
- `ytfast toggle|next|previous|like|…` drives the running app, for Hyprland bindings and scripts.
- Song-change notifications are available and off by default.
- A mini player shows cover, title, artist, progress and controls in a small window suited to floating in Hyprland.
- YouTube Music and YouTube song, album, artist and playlist links open in ytfast: pasted into search, dropped on the window, or passed as `ytfast open <link>`.

### Account

- Like and dislike a song from the player bar, any row, Now Playing, the keyboard, MPRIS clients that support it, and the command line; the state shown always matches the account. Disliking the song that is playing moves on to the next one, as YouTube Music does.
- Save albums and playlists to the library and remove them; subscribe to and unsubscribe from artists.
- Create, rename, describe and delete playlists; add songs from any menu or by dragging onto a playlist; remove and reorder songs. Changes show at once and are rolled back with a plain message if YouTube Music refuses them.

### Now Playing and pages

- Now Playing takes its colour from the current cover, kept legible over any theme.
- Lyrics follow the song line by line when timed lyrics exist (YouTube Music first, then LRCLIB), and fall back to plain lyrics otherwise.
- Library includes History. Home shows YouTube Music's mood chips (Energize, Relax, Workout…). Artist pages have See all for albums, singles and similar artists. Search remembers recent searches.

### Signature moments

ytfast keeps YouTube Music's map but has its own feel. Every moment answers input in the same frame, never delays audio or navigation, and can be interrupted.

- **The cover flies:** opening an album, playlist, artist or Now Playing grows the clicked cover into its new place; Back reverses it. A song's cover flies into Now Playing's.
- **The handoff:** at a song change the next cover slides in from Up next and the title rolls over, in time with the audio.
- **Stage:** `F` fills the window with the cover and large timed lyrics on the theme's background; clicking a line seeks to it.
- **Most-replayed seek bar:** where YouTube has replay data, the seek bar shows it as a ridge with a jump to the peak.
- **Audition:** holding a key on any song plays its best part over the ducked current song, without touching the queue or history.
- **Theme-painted covers:** an optional mode draws every cover in the theme's colours and repaints on a theme switch.
- **Play anything:** `Ctrl+K` searches library, history and YouTube Music and takes commands; Enter plays the top match.
- **Physical feel:** shelves fling and settle, the seek handle has weight, play/pause morphs, cards lift under the pointer.
- **Smooth mixes:** an optional crossfade on radios, mixes and autoplay; albums and playlists stay gapless.

## Fixed architecture and protected constraints

- Native Rust + egui (eframe, glow backend) on the `crmne/egui apps-0.36` and `crmne/winit apps-0.30` fork revisions used by ZapFast and Spotifast. Reuse `fastframe` crates (theme, fonts, icons, text, log) at one pinned tag. No browser engine or webview anywhere.
- Colours come only from the active Omarchy theme through a `fastframe-theme` palette, following theme changes live; cover art is the only thing shown in its own colours (Tyler, 2026-10-01). The UI must read well across any Omarchy theme.
- Credentials: cookie values never enter logs, crash reports, the repository or world-readable files. Any derived cookie file is 0600 in the user's runtime directory. The Chromium cookie key is treated the same way.
- No telemetry and no ytfast-operated services. The only network peers are YouTube/Google endpoints, the stream CDN and LRCLIB.

## Exclusions

Video playback; audio visualizers; downloads, offline mode or any on-disk audio cache; scrobbling and presence services; podcasts; uploads; macOS/Windows; AUR packaging and self-update; being signed in to more than one account at once; non-Chromium browsers' cookies.

Native Linux binary releases are supported through [the release procedure](releases.md).
The first public release is v0.1.0; package version, tag and assets follow the
same SemVer value. Release preparation never bumps or publishes automatically.

## Completion evidence

A version is complete when the following are observed with a real YouTube Music Premium account:

1. **Journeys:** a scripted E2E run drives the real app through Home → shelf item play → Now Playing (Up next, Lyrics, Related) → Explore → Library (each chip) → album, artist and playlist pages → search, with a screenshot of each state and an interaction log, written to an artifact directory by a repeatable command.
2. **Playback:** a real queue plays across at least three track changes and a seek, recording the itag/codec actually streamed (774 with Premium), track-change gaps, and a continuing position.
3. **History:** a song played in ytfast appears at the top of the account's history as fetched afterwards from YouTube Music.
4. **Recovery:** with an invalidated cookie set, ytfast shows the signed-out state and Reconnect; Reconnect restores the account without restarting the browser. A forced stream failure retries, then skips with a visible error.
5. **Theme:** screenshots under a light and a dark Omarchy theme show the colours following a live theme switch.
6. **Speed:** first window with cached content in under 1 s; idle memory in the low hundreds of MB. Measured values are recorded.
7. **Control:** an E2E run uses only the keyboard for search, playback, volume, like and navigation; uses Play next, Add to queue and queue reordering, and the songs then play in the shown order; changes an equalizer preset and observes it applied in mpv; and after a relaunch finds the same queue, song, position and equalizer.
8. **Desktop:** `playerctl` reads metadata and controls playback; with the window closed the music keeps playing and a relaunch brings the window back; the command line and mini player drive the same session; a pasted link and `ytfast open <link>` each open the linked page.
9. **Account:** a like, a library save, a subscription and a playlist created, edited and deleted in ytfast are each confirmed by fetching the account afterwards, and the run leaves the account as it found it.
10. **Now Playing and pages:** screenshots of Now Playing under a light and a dark theme, checked to sit on the theme's own background, timed lyrics advancing with the song, History, Home mood chips and artist See all pages.
11. **Signature moments:** a screen recording of each moment on the OptiPlex, with frame timing showing input answered in the same frame and no added delay to audio or first content.
