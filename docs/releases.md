# Releases

`v0.1.0` is Music / YTfast's **first published release** (2026-10-06).
Keep its tag, release and assets intact. Historical internal milestones are not
published version numbers. Do not renumber, delete or rewrite that release.

## Version policy

`package.version` in `Cargo.toml` is authoritative. Update the matching `ytfast`
entry in `Cargo.lock` and the version heading in [CHANGELOG.md](../CHANGELOG.md)
in the same reviewed change. The release command derives `vVERSION`, title,
archive name and provenance from that value; there is no separate version file.

Choose a number for real changes, not for running the release command:

- Fixes after 0.1.0: **0.1.1** (`Change type: fix`).
- Features after 0.1.0: **0.2.0** (`Change type: feature`).
- Breaking changes before 1.0: next minor, patch zero (`Change type: breaking`).
  From 1.0 onward a breaking change advances the major version.

Each release advances the highest published stable SemVer by one appropriate
step. Do not skip numbers, bump automatically or release only a version edit.
Add a dated changelog section with `Change type: fix|feature|breaking` and concrete
change bullets. Preparing this workflow leaves the package at **0.1.0**.

## Prepare and rehearse

Use the existing checkout. Need Python 3.11+, Git, authenticated GitHub CLI,
Rust >=1.98, CMake/C compiler, binutils (`objdump`), `ldd`, and SQLite development
files (`libsqlite3-dev` on Debian/Ubuntu, `sqlite` on Arch), plus the normal build
dependencies from [integration.md](integration.md). GitHub authentication needs
read access for preflight; publication needs repository release/tag write access.
No credential is stored by these scripts.

1. Implement, review and verify the actual change, including relevant native
   GUI/playback evidence. Pick its version, synchronize Cargo.lock, and write
   its changelog entry. Commit everything. Fetch the preceding published tag
   from the repository named in Cargo.toml if its commit is not locally present.
2. Run `python3 scripts/release.py --check`. It requires clean tracked and
   untracked source, synchronized manifest/lock, a meaningful changelog, correct
   next SemVer, real changes since the preceding release, and no existing local
   or remote target tag/release. GitHub lookup errors stop the process.
3. Rehearse with the same command used for publication:

   ```sh
   export PATH="$HOME/.cargo/bin:$PATH" # for Rust installed with rustup
   python3 scripts/release.py --dry-run --build-dir ~/build/ytfast --output ~/build/ytfast/rehearsal
   ```

   Dry-run is also the default. It **does build and package**, but only reads
   GitHub; it never creates tags/releases. Output must be a new directory outside
   the checkout. Use a fresh output path for each attempt. Cargo dependencies can
   be reused in the build directory. On smaller machines, use the usual
   user-systemd resource limits when needed (for example MemoryMax=6G,
   CPUQuota=100%); the script serializes Cargo with `-j 1`.

The command checks formatting and strict Clippy, runs Cargo's locked normal
release build without `e2e`, and obtains the actual executable from Cargo's
artifact messages. It checks dynamic linkage and runs only `--help` with fresh
HOME/all XDG paths. It does not install, start GUI playback, inspect profiles,
stop Music, or generate marketing images. Native acceptance remains a separate
requirement before publication; neither CLI smoke nor packaging proves it.

Outputs reuse the first release's conventions:

- `ytfast-vVERSION-linux-x86_64.tar.gz`: executable, license, desktop launcher,
  README with installation and measured minimum glibc.
- `SHA256SUMS`: archive, provenance and extracted executable hashes.
- `provenance.json`: manifest version, tag, source commit/tree, previous tag,
  compiler versions, architecture, glibc floor, feature state and content hashes.
- `release-notes.md`, `cargo-build.jsonl`, `receipt.json`: reviewed notes, build
  artifact evidence and the exact future publication command.

## Publish a future release

After authorization for that release, merge/push the reviewed candidate to the
canonical repository's default branch. Its remote tip must equal local HEAD.
The publication target always comes from `package.repository`, **not origin**
(a contributor checkout may still have a different origin).

```sh
export PATH="$HOME/.cargo/bin:$PATH" # for Rust installed with rustup
python3 scripts/release.py --publish --build-dir ~/build/ytfast --output ~/build/ytfast/published-candidate
```

This repeats the checks/build/package, rechecks clean source and remote state
after the build, then uses the existing `gh release create --target COMMIT`
publication path. GitHub creates the matching tag and normal public release
with the three package assets. The command never bumps a version, force-pushes,
deletes a release, or replaces assets. It verifies the remote tag's commit,
published state and downloaded asset hashes before recording `published: true`.

If publication/upload/verification fails, inspect the recorded command and
GitHub state before acting. A partly created release may exist; the command
deliberately refuses to overwrite it or retry blindly. Keep original artifacts
and resolve the actual failure. No automatic CI publication is configured.

`python3 scripts/test-release.py` exercises rejection cases in a disposable Git
fixture under `~/build`; it performs read-only GitHub requests and never builds
or publishes. An isolated next-version full build is a rehearsal, not a release
candidate or permission to publish it.
