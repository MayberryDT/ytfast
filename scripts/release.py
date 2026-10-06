#!/usr/bin/env python3
"""Build a SemVer release with Cargo and gh. Default is a non-publishing dry run."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import platform
import re
import shutil
import subprocess
import sys
import tarfile
import tempfile
import tomllib

ROOT = Path(__file__).resolve().parents[1]
SEMVER = re.compile(r"(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)")


def require(condition, message):
    if not condition:
        raise RuntimeError(message)


def run(*args, env=None):
    p = subprocess.run(args, cwd=ROOT, env=env, text=True, capture_output=True)
    require(p.returncode == 0, f"{' '.join(map(str, args))}: {p.stderr.strip()}")
    return p.stdout.strip()


def version_tuple(value):
    require(SEMVER.fullmatch(value), f"not a plain SemVer version: {value}")
    return tuple(map(int, value.split(".")))


def remote_commit(url, ref):
    lines = run("git", "ls-remote", url, ref, ref + "^{}").splitlines()
    refs = dict(line.split()[::-1] for line in lines)
    return refs.get(ref + "^{}", refs.get(ref))


def clean():
    require(not run("git", "status", "--porcelain", "--untracked-files=all"),
            "release requires a clean checkout, including untracked files")
    require(not run("git", "submodule", "status"), "release does not support submodules")


def preflight(publish):
    clean()
    package = tomllib.loads((ROOT / "Cargo.toml").read_text())["package"]
    version = package["version"]
    version_tuple(version)
    require(package["name"] == "ytfast", "unexpected package name")
    lock = tomllib.loads((ROOT / "Cargo.lock").read_text())
    own = [p for p in lock["package"] if p["name"] == "ytfast" and "source" not in p]
    require(len(own) == 1 and own[0]["version"] == version, "Cargo.lock version differs from Cargo.toml")
    match = re.fullmatch(r"https://github.com/([\w.-]+/[\w.-]+)", package["repository"])
    require(match, "package.repository must name the canonical GitHub repository")
    repo = match[1]
    url = package["repository"] + ".git"
    tag = "v" + version
    head = run("git", "rev-parse", "HEAD")

    # Read GitHub explicitly, never infer the publication target from origin.
    releases = run("gh", "api", f"repos/{repo}/releases?per_page=100", "--paginate", "--jq",
                   '.[] | select(.draft == false and .prerelease == false) | .tag_name').splitlines()
    require(tag not in releases, f"release {tag} already exists; never rewrite a published version")
    require(not remote_commit(url, "refs/tags/" + tag), f"remote tag {tag} already exists; inspect it, do not overwrite")
    local_tag = subprocess.run(["git", "rev-parse", "--verify", "refs/tags/" + tag],
                               cwd=ROOT, capture_output=True)
    require(local_tag.returncode != 0, f"local tag {tag} already exists; inspect it before release")

    log = (ROOT / "CHANGELOG.md").read_text()
    entry = re.search(rf"^## {re.escape(version)}(?: — [^\n]+)?\n(.*?)(?=^## |\Z)", log, re.M | re.S)
    require(entry is not None and re.search(r"^\- \S", entry[1], re.M), f"missing nonempty changelog entry for {version}")
    kind = re.search(r"^Change type: (fix|feature|breaking)$", entry[1], re.M)
    require(kind, "changelog must declare Change type: fix, feature or breaking")
    prior = []
    for old_tag in releases:
        if old_tag.startswith("v") and SEMVER.fullmatch(old_tag[1:]):
            prior.append((version_tuple(old_tag[1:]), old_tag))
    require(prior, "no published SemVer baseline found; first release is already v0.1.0")
    previous, previous_tag = max(prior)
    major, minor, patch = previous
    if kind[1] == "fix":
        expected = (major, minor, patch + 1)
    elif kind[1] == "feature" or major == 0:
        expected = (major, minor + 1, 0)
    else:
        expected = (major + 1, 0, 0)
    require(version_tuple(version) == expected,
            f"{kind[1]} after {previous_tag}: expected {'.'.join(map(str, expected))}, got {version}")
    base = remote_commit(url, "refs/tags/" + previous_tag)
    require(base, f"published baseline {previous_tag} has no remote tag")
    run("git", "merge-base", "--is-ancestor", base, head)
    changed = run("git", "diff", "--name-only", base, head, "--", ".",
                  ":(exclude)Cargo.toml", ":(exclude)Cargo.lock", ":(exclude)CHANGELOG.md")
    # Dependency/metadata changes are real too; ignore just the version fields.
    old_manifest = tomllib.loads(run("git", "show", f"{base}:Cargo.toml"))
    old_manifest["package"].pop("version")
    new_manifest = tomllib.loads((ROOT / "Cargo.toml").read_text())
    new_manifest["package"].pop("version")
    old_lock = tomllib.loads(run("git", "show", f"{base}:Cargo.lock"))
    for p in old_lock["package"]:
        if p["name"] == "ytfast" and "source" not in p:
            p["version"] = version
    require(changed or old_manifest != new_manifest or old_lock != lock,
            "version/changelog-only bump has no real changes")
    if publish:
        default_branch = run("gh", "api", f"repos/{repo}", "--jq", ".default_branch")
        require(remote_commit(url, "refs/heads/" + default_branch) == head,
                "publish requires HEAD to be the canonical remote default-branch commit")
    return {"version": version, "tag": tag, "repository": repo, "source_commit": head,
            "source_tree": run("git", "rev-parse", "HEAD^{tree}"), "previous_tag": previous_tag,
            "change_type": kind[1], "notes": entry[1].strip()}


def digest(path):
    with path.open("rb") as file:
        return hashlib.file_digest(file, "sha256").hexdigest()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    modes = parser.add_mutually_exclusive_group()
    modes.add_argument("--check", action="store_true", help="preflight only (GitHub reads, no build or publication)")
    modes.add_argument("--dry-run", action="store_true", help="build/package/verify, without remote writes (default)")
    modes.add_argument("--publish", action="store_true", help="create one new release after all checks; never overwrite")
    parser.add_argument("--build-dir", type=Path, default=Path(os.environ.get("CARGO_TARGET_DIR", Path.home() / "build/ytfast")))
    parser.add_argument("--output", type=Path, help="new output directory outside the checkout")
    args = parser.parse_args()
    info = preflight(args.publish)
    print(json.dumps({k: v for k, v in info.items() if k != "notes"}, indent=2), flush=True)
    if args.check:
        return
    require(platform.system() == "Linux" and platform.machine() == "x86_64", "packaging supports Linux x86_64 only")
    build = args.build_dir.resolve()
    out = (args.output or build / "dist" / info["tag"]).resolve()
    require(not out.is_relative_to(ROOT) and not build.is_relative_to(ROOT), "build/output must be outside the checkout")
    require(not out.exists(), "output directory already exists; use a new path, never replace release artifacts")
    out.mkdir(parents=True)
    env = os.environ.copy()
    env["CARGO_TARGET_DIR"] = str(build)
    print("Checking formatting and strict Clippy, then building the normal release executable…", flush=True)
    run("cargo", "fmt", "--all", "--check", env=env)
    run("cargo", "clippy", "--locked", "--all-targets", "--features", "e2e", "-j", "1", "--", "-D", "warnings", env=env)
    messages = run("cargo", "build", "--locked", "--release", "--bin", "ytfast", "-j", "1", "--message-format=json", env=env)
    (out / "cargo-build.jsonl").write_text(messages + "\n")
    artifacts = [json.loads(line) for line in messages.splitlines() if line.startswith("{")]
    binary = [a for a in artifacts if a.get("reason") == "compiler-artifact"
              and a.get("target", {}).get("name") == "ytfast" and a.get("executable")]
    require(len(binary) == 1 and "e2e" not in binary[0]["features"], "normal release binary must not enable e2e")
    require(Path(binary[0]["manifest_path"]).resolve() == ROOT / "Cargo.toml", "Cargo built another checkout")
    clean()
    require(run("git", "rev-parse", "HEAD") == info["source_commit"], "source changed during build")
    package_name = f"ytfast-{info['tag']}-linux-x86_64"
    package = out / package_name
    package.mkdir()
    for src, dest in [(Path(binary[0]["executable"]), "ytfast"), (ROOT / "LICENSE", "LICENSE"),
                      (ROOT / "assets/ytfast.desktop", "ytfast.desktop")]:
        shutil.copy2(src, package / dest)
    deps = run("ldd", str(package / "ytfast"))
    require("not found" not in deps, "package has unresolved dynamic dependencies")
    symbols = run("objdump", "-T", str(package / "ytfast"))
    glibc = sorted(set(re.findall(r"GLIBC_([0-9.]+)", symbols)), key=lambda x: tuple(map(int, x.split("."))))[-1]
    with tempfile.TemporaryDirectory(prefix="ytfast-cli-") as tmp:
        isolated = env.copy()
        for key, folder in [("HOME", "home"), ("XDG_CONFIG_HOME", "config"), ("XDG_CACHE_HOME", "cache"),
                            ("XDG_DATA_HOME", "data"), ("XDG_RUNTIME_DIR", "runtime")]:
            path = Path(tmp) / folder
            path.mkdir(mode=0o700)
            isolated[key] = str(path)
        require("usage: ytfast" in run(str(package / "ytfast"), "--help", env=isolated), "packaged CLI smoke failed")
    (package / "README.txt").write_text(
        f"YTfast {info['version']} — Music for Linux / Omarchy\nSource: https://github.com/{info['repository']}/tree/{info['source_commit']}\n\n"
        "Install: install -Dm755 ytfast ~/.local/bin/ytfast\n"
        "Optional launcher: install -Dm644 ytfast.desktop ~/.local/share/applications/ytfast.desktop\n"
        f"Requires Linux x86_64, glibc >= {glibc}, SQLite 3, graphical session, mpv, yt-dlp, deno, secret-tool/libsecret.\n")
    archive = out / (package_name + ".tar.gz")
    with tarfile.open(archive, "w:gz") as tar:
        tar.add(package, arcname=package_name)
    info.update({"binary_sha256": digest(package / "ytfast"), "archive_sha256": digest(archive),
                 "e2e_feature": False, "rustc": run("rustc", "--version"), "cargo": run("cargo", "--version"),
                 "platform": "linux-x86_64", "minimum_glibc": glibc})
    notes = out / "release-notes.md"
    notes.write_text(f"# Music / YTfast {info['version']}\n\n{info['notes']}\n\n"
                     f"Linux x86_64, glibc >= {glibc}; dependencies and installation are in the archive README.\n"
                     "Formatting, strict Clippy, locked normal release build, isolated CLI and linkage passed. "
                     "This command does not prove native GUI, account playback or flight behavior; review those separately before publication.\n")
    provenance = out / "provenance.json"
    provenance.write_text(json.dumps({k: v for k, v in info.items() if k != "notes"}, indent=2) + "\n")
    sums = out / "SHA256SUMS"
    sums.write_text("".join(f"{digest(p)}  {p.relative_to(out)}\n" for p in [archive, provenance, package / "ytfast"]))
    command = ["gh", "release", "create", info["tag"], str(archive), str(sums), str(provenance),
               "--repo", info["repository"], "--target", info["source_commit"],
               "--title", f"Music / YTfast {info['version']}", "--notes-file", str(notes)]
    receipt = {"mode": "publish" if args.publish else "dry-run", "publication_command": command,
               "published": False, "source_commit": info["source_commit"], "version": info["version"],
               "archive_sha256": info["archive_sha256"], "package_cli_pass": True, "linkage_pass": True}
    (out / "receipt.json").write_text(json.dumps(receipt, indent=2) + "\n")
    if args.publish:
        # Recheck source and remote state after the potentially long build.
        require(preflight(True)["source_commit"] == info["source_commit"], "source changed during build")
        receipt["release_url"] = run(*command)
        require(remote_commit(f"https://github.com/{info['repository']}.git", "refs/tags/" + info["tag"]) == info["source_commit"],
                "published tag points to another commit; inspect without overwriting")
        release = json.loads(run("gh", "release", "view", info["tag"], "--repo", info["repository"], "--json", "isDraft,isPrerelease,url"))
        require(not release["isDraft"] and not release["isPrerelease"], "release is not published as a normal release")
        with tempfile.TemporaryDirectory(prefix="ytfast-published-") as tmp:
            run("gh", "release", "download", info["tag"], "--repo", info["repository"], "--dir", tmp)
            for asset in [archive, sums, provenance]:
                require(digest(Path(tmp) / asset.name) == digest(asset), f"published {asset.name} checksum differs")
        receipt["published"] = True
        (out / "receipt.json").write_text(json.dumps(receipt, indent=2) + "\n")
    print(json.dumps(receipt, indent=2))
    print(f"Artifacts: {out}")


if __name__ == "__main__":
    try:
        main()
    except (RuntimeError, OSError, ValueError, subprocess.SubprocessError) as error:
        sys.exit(f"release: {error}")
