#!/usr/bin/env python3
"""Preflight regressions in a disposable Git fixture; never builds or publishes."""
import json
import pathlib
import shutil
import subprocess
import re
import tomllib
import tempfile

ROOT = pathlib.Path(__file__).resolve().parents[1]


def run(*args, cwd=ROOT):
    return subprocess.run(args, cwd=cwd, text=True, capture_output=True, check=True).stdout


def main():
    tags = run("gh", "api", "--hostname", "github.com", "repos/MayberryDT/ytfast/releases?per_page=100",
               "--paginate", "--jq", '.[] | select(.draft == false and .prerelease == false) | .tag_name').splitlines()
    stable = [t for t in tags if re.fullmatch(r"v(?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*)", t)]
    previous = max(stable, key=lambda t: tuple(map(int, t[1:].split("."))))
    major, minor, patch = map(int, previous[1:].split("."))
    next_fix = f"{major}.{minor}.{patch + 1}"
    next_feature = f"{major}.{minor + 1}.0"
    build = pathlib.Path.home() / "build"
    build.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix="ytfast-release-tests-", dir=build) as tmp:
        tree = pathlib.Path(tmp) / "source"
        run("git", "clone", "--quiet", "--no-hardlinks", str(ROOT), str(tree))
        try:
            def commit():
                run("git", "add", "-A", cwd=tree)
                run("git", "-c", "user.name=Release smoke", "-c", "user.email=smoke@example.invalid",
                    "commit", "-qm", "Isolated release preflight fixture", cwd=tree)

            def check(expected, external=False, extra_releases=None):
                command = ["python3", "scripts/release.py", "--check"]
                if external:
                    # Run the real preflight on a baseline checkout with only a
                    # version/changelog edit; adding the tooling would be a real change.
                    command = ["python3", "-B", "-c",
                               "import importlib.util,pathlib; s=importlib.util.spec_from_file_location('release',"
                               + repr(str(ROOT / "scripts/release.py"))
                               + ");m=importlib.util.module_from_spec(s);s.loader.exec_module(m);"
                               "m.ROOT=pathlib.Path.cwd();m.main()", "--check"]
                if extra_releases is not None:
                    # Inject only release-list results: Git, source validation,
                    # published baseline and tag lookups still use real commands.
                    command = ["python3", "-B", "-c", f"""
import importlib.util
s = importlib.util.spec_from_file_location('release', 'scripts/release.py')
m = importlib.util.module_from_spec(s)
s.loader.exec_module(m)
original = m.run
def run(*args, **kwargs):
    result = original(*args, **kwargs)
    if args[:2] == ('gh', 'api') and any('/releases?' in arg for arg in args):
        extra = {extra_releases!r}
        if 'select(' in args[-1]:
            extra = [r for r in extra if not r['draft'] and not r['prerelease']]
        result += ''.join('\\n' + r['tag_name'] for r in extra)
    return result
m.run = run
m.main()
""", "--check"]
                p = subprocess.run(command, cwd=tree,
                                   text=True, capture_output=True)
                if expected == "pass":
                    assert p.returncode == 0, p.stderr
                else:
                    assert p.returncode != 0 and expected in p.stderr, (expected, p.stdout, p.stderr)
                print(json.dumps({"case": expected, "exit": p.returncode,
                                  "extra_releases": extra_releases}))

            manifest = tree / "Cargo.toml"
            lock = tree / "Cargo.lock"
            log = tree / "CHANGELOG.md"
            def set_version(value):
                old = tomllib.loads(manifest.read_text())["package"]["version"]
                manifest.write_text(manifest.read_text().replace(f'version = "{old}"', f'version = "{value}"', 1))
                lock.write_text(re.sub(r'(name = "ytfast"\nversion = ")[^"]+',
                                       lambda m: m[1] + value, lock.read_text(), count=1))

            # Published versions are immutable, even from a newer commit.
            set_version(previous[1:])
            if run("git", "status", "--porcelain", cwd=tree):
                commit()
            check("already exists")
            set_version(next_fix)
            log.write_text(f"# Changelog\n\n## {next_fix}\n\nChange type: fix\n\n- Isolated smoke only, never publish.\n")
            (tree / "docs/release-smoke-only.txt").write_text("Isolated regression fixture change, never shipped.\n")
            commit()
            check("pass")
            # These releases intentionally have no remote/local tag. A stable
            # baseline filter must not hide collisions or include other drafts.
            for draft, prerelease in [(True, False), (False, True)]:
                check("already exists", extra_releases=[
                    {"tag_name": "v" + next_fix, "draft": draft, "prerelease": prerelease}])
            check("pass", extra_releases=[
                {"tag_name": "v99.0.0", "draft": True, "prerelease": False},
                {"tag_name": "v98.0.0", "draft": False, "prerelease": True}])
            good = run("git", "rev-parse", "HEAD", cwd=tree).strip()

            (tree / "untracked-file").write_text("dirty")
            check("clean checkout")
            (tree / "untracked-file").unlink()

            lock.write_text(re.sub(r'(name = "ytfast"\nversion = ")[^"]+',
                                   lambda m: m[1] + "99.99.99", lock.read_text(), count=1))
            commit()
            check("Cargo.lock")
            run("git", "reset", "--hard", good, cwd=tree)

            log.write_text("# Changelog\n")
            commit()
            check("changelog entry")
            run("git", "reset", "--hard", good, cwd=tree)

            log.write_text(log.read_text().replace("Change type: fix", "Change type: feature"))
            commit()
            check(f"expected {next_feature}")
            run("git", "reset", "--hard", good, cwd=tree)

            run("git", "tag", "v" + next_fix, "HEAD~1", cwd=tree)
            try:
                check("local tag")
            finally:
                run("git", "tag", "-d", "v" + next_fix, cwd=tree)

            base = run("git", "ls-remote", "https://github.com/MayberryDT/ytfast.git", "refs/tags/" + previous).split()[0]
            # The baseline might be newer than the caller's available objects.
            run("git", "fetch", "--quiet", "--no-tags", "https://github.com/MayberryDT/ytfast.git", base, cwd=tree)
            run("git", "reset", "--hard", base, cwd=tree)
            set_version(next_fix)
            log.write_text(f"# Changelog\n\n## {next_fix}\n\nChange type: fix\n\n- Only a version edit.\n")
            commit()
            check("version/changelog-only", external=True)
        finally:
            shutil.rmtree(tree)


if __name__ == "__main__":
    main()
