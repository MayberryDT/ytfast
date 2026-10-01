# ytfast agent guide

ytfast is a native YouTube Music client for Omarchy (Linux): Rust + egui on [fastframe](https://github.com/crmne/fastframe), modelled on ZapFast and Spotifast. It has no browser engine, no telemetry and no server of its own. Shown to people as "Music".

## Start here

1. [docs/SPEC.md](docs/SPEC.md): the product: journeys, decisions, exclusions. Don't redefine it from the code.
2. [docs/integration.md](docs/integration.md): verified cookie, InnerTube and stream facts, the chosen design, and how the E2E runs work. Read it before touching sign-in, playback or the build.
3. If a `notes/` directory exists, it's the maintainer's private notes (gitignored). Read `notes/AGENTS.md` first and follow it as well.

## Rules

- Cookie values and the Chromium cookie key are secrets. Never log, print or commit them; derived cookie files are 0600 and short-lived. Read the browser's cookie store read-only and never restart or modify the browser.
- Nothing from a real account goes into the repository: no captured responses, screenshots or logs with account data. E2E artifacts stay in the gitignored `artifacts/`. Public screenshots come from the signed-out `showcase` scenario.
- Colours come only from the Omarchy theme palette. Never hard-code colours.
- Don't vendor or patch upstream crates here. Keep the egui/winit fork pins and the fastframe tag aligned with ZapFast/Spotifast and move them together.
- Tests: prefer E2E through the real app (`scripts/e2e.sh`), producing a repeatable artifact under `artifacts/`. No unit tests written after the code, and no tautological tests.
- `cargo fmt --all --check` and `cargo clippy --all-targets --features e2e -- -D warnings` must pass. Release builds are heavy (several minutes, a few GB of memory); cap them on small machines.

## UI copy

- Use YouTube Music's own labels for familiar places: Home, Explore, Library, Up next, Lyrics, Related, Quick picks, Listen again.
- Plain and short, sentence case. Say what happened and the one thing to do: "Signed out of YouTube Music" with Reconnect, not "Authentication error (401)".
- No exclamation marks or cleverness in errors. Technical detail (itag, codec, error text) goes behind a Copy button or in Settings.
- Never claim a state that wasn't checked, for example "Signed in" before an account request has succeeded.
