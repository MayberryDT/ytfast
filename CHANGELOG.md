# Changelog

## 0.2.0 — 2026-10-09

Change type: feature

- Fix playback stalling with the play button and spinner flickering: Repeat one
  no longer loops a stream URL in mpv after YouTube expires it (about six hours);
  the song is loaded again with a fresh URL.
- Fix songs ending early or being skipped when their stream URL expires part way
  (a long mix or a long pause): playback picks up where it stopped.
- Clicking a song or mix flies its cover into Now Playing and opens it; a click
  on Now Playing's cover plays or pauses (a double click still opens Stage).
- Add the radio chips over Up next (All, Popular, Deep cuts, Workout…): one
  chosen makes the songs after the current one that radio.
- Buttons and chips share one square-cornered box shape instead of pills.
- Add a repeatable SemVer release command and documented preflight, packaging,
  provenance, checksum verification and publication procedure.

## 0.1.0 — 2026-10-06

- First published Music / YTfast release for Linux / Omarchy.
- Include PR #1 by dedez1nn and its reviewed follow-up: preserve artwork
  proportions in cards, flying-cover transitions and Stage using centered crops,
  and improve the image-loading handoff.
- Publish the native Linux x86_64 archive, checksums, source provenance and a
  real signed-out Home screenshot.

This is the first release, irrespective of earlier internal milestones. Its
published tag, version and assets remain unchanged.

[Release](https://github.com/MayberryDT/ytfast/releases/tag/v0.1.0)
