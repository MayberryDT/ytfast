#!/usr/bin/env python3
"""Preflight regressions in a disposable Git fixture; never builds or publishes."""
import json
import pathlib
import shutil
import subprocess
import tempfile

ROOT = pathlib.Path(__file__).resolve().parents[1]


def run(*args, cwd=ROOT):
    return subprocess.run(args, cwd=cwd, text=True, capture_output=True, check=True).stdout


def main():
    with tempfile.TemporaryDirectory(prefix="ytfast-release-tests-", dir=pathlib.Path.home() / "build") as tmp:
        tree = pathlib.Path(tmp) / "source"
        run("git", "clone", "--quiet", "--no-hardlinks", str(ROOT), str(tree))
        try:
            def commit():
                run("git", "add", "-A", cwd=tree)
                run("git", "-c", "user.name=Release smoke", "-c", "user.email=smoke@example.invalid",
                    "commit", "-qm", "Isolated release preflight fixture", cwd=tree)

            def check(expected):
                p = subprocess.run(["python3", "scripts/release.py", "--check"], cwd=tree,
                                   text=True, capture_output=True)
                if expected == "pass":
                    assert p.returncode == 0, p.stderr
                else:
                    assert p.returncode != 0 and expected in p.stderr, (expected, p.stdout, p.stderr)
                print(json.dumps({"case": expected, "exit": p.returncode}))

            # The existing first release is immutable, even from a newer commit.
            check("already exists")
            manifest = tree / "Cargo.toml"
            lock = tree / "Cargo.lock"
            log = tree / "CHANGELOG.md"
            manifest.write_text(manifest.read_text().replace('version = "0.1.0"', 'version = "0.1.1"', 1))
            lock.write_text(lock.read_text().replace('name = "ytfast"\nversion = "0.1.0"',
                                                   'name = "ytfast"\nversion = "0.1.1"', 1))
            log.write_text("# Changelog\n\n## 0.1.1\n\nChange type: fix\n\n- Isolated smoke of the release workflow; not a release candidate.\n")
            commit()
            check("pass")
            good = run("git", "rev-parse", "HEAD", cwd=tree).strip()

            (tree / "untracked-file").write_text("dirty")
            check("clean checkout")
            (tree / "untracked-file").unlink()

            lock.write_text(lock.read_text().replace('name = "ytfast"\nversion = "0.1.1"',
                                                   'name = "ytfast"\nversion = "0.9.0"', 1))
            commit()
            check("Cargo.lock")
            run("git", "reset", "--hard", good, cwd=tree)

            log.write_text("# Changelog\n")
            commit()
            check("changelog entry")
            run("git", "reset", "--hard", good, cwd=tree)

            log.write_text(log.read_text().replace("Change type: fix", "Change type: feature"))
            commit()
            check("expected 0.2.0")
            run("git", "reset", "--hard", good, cwd=tree)

            run("git", "tag", "v0.1.1", "HEAD~1", cwd=tree)
            try:
                check("local tag")
            finally:
                run("git", "tag", "-d", "v0.1.1", cwd=tree)
        finally:
            shutil.rmtree(tree)


if __name__ == "__main__":
    main()
