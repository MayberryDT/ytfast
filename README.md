# ytfast

A native YouTube Music player for [Omarchy](https://omarchy.org), written in Rust with egui. It looks and works like YouTube Music, opens in well under a second, plays audio only, and takes its colours from your Omarchy theme. In the app launcher it's called **Music**.

![Now Playing with lyrics](docs/screenshots/lyrics.png)

It's built on [fastframe](https://github.com/crmne/fastframe), [Carmine Paolino](https://github.com/crmne)'s foundation for native egui apps, and follows the pattern of his [ZapFast](https://github.com/crmne/zapfast) and [Spotifast](https://github.com/crmne/spotifast): no browser engine, no telemetry, no server of its own.

It's unofficial and not affiliated with YouTube or Google. It uses YouTube Music's private web API, so a change on YouTube's side can break it. It talks only to YouTube and Google, and to LRCLIB for timed lyrics (sending a song's title, artist, album and length, nothing else).

## Screenshots

| | |
| --- | --- |
| ![An artist page, with a song playing](docs/screenshots/artist.png) | ![An album page](docs/screenshots/album.png) |
| ![Explore: new releases, moods and genres](docs/screenshots/explore.png) | ![Search results](docs/screenshots/search.png) |

Colours come from your Omarchy theme and change with it while the app is open. Here's the same album in a light theme:

![The album page in a light Omarchy theme](docs/screenshots/album-light-theme.png)

These were taken signed out, so they show public YouTube Music rather than anyone's library, and the corner says "Signed out of YouTube Music". Signed in, Home, Library and the sidebar show your own music.

## What it does

The YouTube Music you know, with its own feel:

- **Covers that fly.** Open an album and its cover lifts out of the card into the page; Back sends it home. Songs fly into the player, and Now Playing opens out of it.
- **Song changes you see but never hear.** Playback is gapless; at the change the next cover and title roll into the player.
- **Now Playing** with lyrics that follow the song line by line (from YouTube Music, or [LRCLIB](https://lrclib.net) when it has none). Click a line to jump there.
- **Stage** (`F`): the cover and large lyrics fill the window. For a second screen or a party.
- **The most replayed part**: a ridge along the seek bar shows where everyone replays a song, with a jump to the peak.
- **Audition**: hold `Alt` (or the middle button) on any song to hear its best part over your music, which dips and comes back. Your queue never changes.
- **Smooth mixes**: radios and mixes can blend from song to song. Albums stay gapless.
- **Theme-painted covers**: an optional mode draws every cover in your Omarchy theme's colours.
- **Things with weight**: cards lift, play turns into pause, carousels glide and settle on a card.

And everything you'd expect:

- Home (with its moods), Explore, Library (with History), search with suggestions and recent searches, album, artist and playlist pages
- A queue you can edit: Play next, Add to queue, drag to reorder, remove, clear
- Likes and dislikes, saving albums and playlists to your library, subscribing to artists, and creating, editing and deleting your playlists
- Audio at the best quality your account gets (Opus at about 256 kbps with YouTube Music Premium), levelled between songs, with a ten-band equalizer and a sleep timer
- Songs on screen are prepared before you click them, so they start at once; Music reopens where you left off
- Plays count in your YouTube Music history, so your recommendations keep learning
- Media keys, `playerctl` and the Omarchy bar's media widget (MPRIS), playing on after you close the window, a command line, a mini player, and song-change notifications if you want them

## Keyboard, menus and Play anything

Press **?** to see every shortcut. The common ones: **Space** play or pause, **←/→** back or forward 5 seconds, **Shift+←/→** previous or next song, **+/-** volume, **M** mute, **L** like, **S** shuffle, **R** repeat, **N** Now Playing, **Q** Up next, **F** Stage (**F11** full screen inside it), **P** the most replayed part, **/** search, **Alt+←/→** back and forward, **E** equalizer, **Ctrl+,** Settings, **Esc** closes what's on top. Hold **Alt** over a song to audition it. They don't fire while you type in a field.

Right-click a song, album, playlist or artist (or use its **⋮**) for Play next, Add to queue, Start radio, Like, Add to playlist, Save to library, Go to album or artist and Copy link. Menus work from the keyboard too: arrows, Enter, Esc.

**Ctrl+K** opens Play anything: type and it finds your library, recent searches, the page you're on and YouTube Music; Enter plays the top match, Shift+Enter opens its page. It takes commands as well: `radio <song or artist>`, `like`, `next`, `pause`, `play`, `shuffle`, `repeat`, `sleep 30` (or `sleep end`), `eq bass`, `mini`.

**Save** in Up next makes the queue a playlist.
## On the desktop

Closing the window keeps the music playing, with a Music icon in the bar's tray: click it to bring the window back where you left it, middle-click to play or pause, scroll to change the volume, or right-click for Next, Previous and Quit. Launching Music again, `ytfast show` or the media widget also bring it back. **Ctrl+Q** quits and stops the music; closing the window with nothing queued quits too.

The running app takes commands, for Hyprland bindings and scripts:

```sh
ytfast toggle            # play or pause (also: play, pause)
ytfast next              # or: previous
ytfast like              # like or unlike the playing song
ytfast show              # bring back the window
ytfast open <link>       # a YouTube Music or YouTube link; starts Music if it isn't running
ytfast quit              # quit and stop the music
```

Links open in ytfast too when you paste one into the search field. Songs start playing; albums, artists, playlists and searches open their page. Dropping a link file on the window works under X11, but not on Wayland: the windowing library ytfast uses doesn't receive drops there yet.

**Ctrl+M** (or the button next to the volume) switches to the mini player, a small window with the cover, the song, a progress bar and the controls. Its button on the right brings back the full window. To keep it floating above other windows in Hyprland:

```ini
windowrule = float, class:ytfast-mini
windowrule = pin, class:ytfast-mini
```

Song-change notifications are off by default; turn them on in **Settings**. They don't appear while a ytfast window has the focus.

## How it signs in

ytfast reads your YouTube sign-in from a Chromium-family browser you're already signed in to (Brave, Brave Origin, Google Chrome or Chromium). It reads the browser's cookie store without changing it, and decrypts it with the key the browser keeps in your keyring. You never paste headers or export files.

By default it uses the browser profile you used most recently. If different browsers are signed in to different Google accounts, choose one in **Settings**; ytfast remembers it in `~/.config/ytfast/settings.json`.

Cookies are never logged or written anywhere readable by other users. While it runs, the cookie file that `yt-dlp` needs lives in `$XDG_RUNTIME_DIR/ytfast` with permissions `0600`.

## Install

You need:

- to build: Rust 1.98 or newer, CMake and a C compiler
- to run: `mpv`, `yt-dlp`, `deno` (yt-dlp uses it for YouTube's player challenges) and `secret-tool` (libsecret)

```sh
git clone https://github.com/MayberryDT/ytfast
cd ytfast
cargo build --release
install -Dm755 target/release/ytfast ~/.local/bin/ytfast
install -Dm644 assets/ytfast.desktop ~/.local/share/applications/ytfast.desktop
```

The release build takes several minutes and a few GB of memory. Logs go to `~/.cache/ytfast/ytfast.log`.

## Development

[docs/SPEC.md](docs/SPEC.md) describes the product: what each screen does and what's deliberately left out. [docs/integration.md](docs/integration.md) has the verified facts it relies on (cookie decryption, YouTube's API, stream formats) and the design. [AGENTS.md](AGENTS.md) holds the rules for coding agents, and for people too.

`scripts/e2e.sh [journey|recovery|offline|theme|showcase|motion|pages|desktop|account|engine|engine-restore|surfaces|deck]` builds with the `e2e` feature and drives the real app on your desktop. All but `showcase` use your signed-in account and leave screenshots, logs and a summary in `artifacts/e2e/`, which git ignores because they show account data. `showcase` runs signed out and takes the pictures above. `desktop` also needs `playerctl` and a notification daemon. `account` changes your account and puts it back; `engine` must be followed by `engine-restore`.

Issues and pull requests are welcome.

## Credits

- [fastframe](https://github.com/crmne/fastframe) by Carmine Paolino (MIT): the theme, fonts, icons, text, logging and shell crates, and his forks of [egui](https://github.com/crmne/egui) and [winit](https://github.com/crmne/winit). ytfast copies the shape of [ZapFast](https://github.com/crmne/zapfast) and [Spotifast](https://github.com/crmne/spotifast); its MPRIS service follows Spotifast's.
- [egui](https://github.com/emilk/egui) by Emil Ernerfeldt and contributors.
- [mpv](https://mpv.io) plays the audio and [yt-dlp](https://github.com/yt-dlp/yt-dlp) finds the streams; ytfast runs both as separate programs.
- Icons from [Lucide](https://lucide.dev) (ISC, see `assets/icons/LICENSE.txt`).

## License

[MIT](LICENSE).
