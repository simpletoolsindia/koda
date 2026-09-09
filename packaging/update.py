#!/usr/bin/env python3
"""Regenerate the Homebrew formula and winget manifests for a release.

    packaging/update.py v0.1.0        # rewrite both from that GitHub release
    packaging/update.py v0.1.0 --check  # verify they already match it

Both files carry a version and five checksums. Editing those by hand after a
release is how a package manager ends up serving the previous version, or a
checksum that no longer matches and fails mid-install for everyone.

The checksums come from the `.sha256` files the release workflow publishes
beside each archive, so this never has to download the archives themselves.
"""
import argparse
import json
import os
import re
import sys
import urllib.request

REPO = "simpletoolsindia/koda"
ROOT = os.path.dirname(os.path.abspath(__file__))

# Homebrew's platform blocks, in the order they appear in the formula.
TARGETS = [
    ("macos", "arm", "aarch64-apple-darwin", "tar.gz"),
    ("macos", "intel", "x86_64-apple-darwin", "tar.gz"),
    ("linux", "arm", "aarch64-unknown-linux-gnu", "tar.gz"),
    ("linux", "intel", "x86_64-unknown-linux-gnu", "tar.gz"),
]
WINDOWS = ("x86_64-pc-windows-msvc", "zip")


def fetch(url):
    with urllib.request.urlopen(url, timeout=60) as r:
        return r.read().decode()


def release(tag):
    data = json.loads(fetch(f"https://api.github.com/repos/{REPO}/releases/tags/{tag}"))
    assets = {a["name"]: a["browser_download_url"] for a in data["assets"]}
    return assets, data["published_at"][:10]


def checksums(tag, version, assets):
    """The published sha256 for every archive this release should carry."""
    out = {}
    for target, ext in [(t[2], t[3]) for t in TARGETS] + [WINDOWS]:
        name = f"koda-{version}-{target}.{ext}"
        digest_url = assets.get(name + ".sha256")
        if not digest_url:
            sys.exit(f"release {tag} has no {name}.sha256 — did the build finish?")
        out[target] = fetch(digest_url).split()[0].lower()
    return out


def formula(version, sums):
    path = os.path.join(ROOT, "homebrew", "koda.rb")
    text = open(path).read()
    for _, _, target, ext in TARGETS:
        name = f"koda-{version}-{target}.{ext}"
        url = f"https://github.com/{REPO}/releases/download/v{version}/{name}"
        # Rewrite the url/sha256 pair belonging to this target, and only it.
        text = re.sub(
            rf'url "[^"]*{re.escape(target)}[^"]*"\n(\s*)sha256 "[0-9a-f]*"',
            lambda m, u=url, s=sums[target]: f'url "{u}"\n{m.group(1)}sha256 "{s}"',
            text,
        )
    return path, text


def manifests(version, sums, released):
    out = []
    winget = os.path.join(ROOT, "winget")
    for name in sorted(os.listdir(winget)):
        if not name.endswith(".yaml"):
            continue
        path = os.path.join(winget, name)
        text = open(path).read()
        text = re.sub(r"^PackageVersion: .*$", f"PackageVersion: {version}",
                      text, flags=re.M)
        zip_name = f"koda-{version}-{WINDOWS[0]}.{WINDOWS[1]}"
        text = re.sub(
            r"InstallerUrl: .*$",
            f"InstallerUrl: https://github.com/{REPO}/releases/download/v{version}/{zip_name}",
            text, flags=re.M,
        )
        text = re.sub(r"InstallerSha256: .*$",
                      f"InstallerSha256: {sums[WINDOWS[0]].upper()}", text, flags=re.M)
        text = re.sub(r"^ReleaseDate: .*$", f"ReleaseDate: {released}",
                      text, flags=re.M)
        text = re.sub(r"ReleaseNotesUrl: .*$",
                      f"ReleaseNotesUrl: https://github.com/{REPO}/releases/tag/v{version}",
                      text, flags=re.M)
        out.append((path, text))
    return out


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("tag", help="release tag, e.g. v0.1.0")
    ap.add_argument("--check", action="store_true",
                    help="fail if the files are not already up to date")
    args = ap.parse_args()

    version = args.tag.lstrip("v")
    assets, released = release(args.tag)
    sums = checksums(args.tag, version, assets)

    stale = []
    for path, text in [formula(version, sums)] + manifests(version, sums, released):
        rel = os.path.relpath(path, os.path.dirname(ROOT))
        if open(path).read() == text:
            print(f"  {rel}: already current")
            continue
        if args.check:
            stale.append(rel)
            print(f"  {rel}: STALE")
            continue
        open(path, "w").write(text)
        print(f"  {rel}: updated")

    if stale:
        print(f"\n{len(stale)} file(s) do not match {args.tag}; run without "
              f"--check to fix", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
