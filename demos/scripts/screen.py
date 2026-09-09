#!/usr/bin/env python3
"""Render an asciicast to plain text, so a recording can be asserted on.

    screen.py <cast> [--at SECONDS] [--grep TEXT]

Without --at it prints the last frame; --grep looks through every frame. A
recording that ran but painted the wrong screen looks identical to a good one
on disk, so this is what tells them apart, and what `record.py` checks each
demo's `expect` strings against before publishing it.

Needs pyte (pip install pyte).
"""
import json
import sys

import pyte


def _events(path):
    with open(path) as fh:
        header = json.loads(fh.readline())
        yield header, None
        for line in fh:
            line = line.strip()
            if not line:
                continue
            try:
                when, kind, data = json.loads(line)
            except ValueError:
                continue
            if kind == "o":
                yield when, data


def frames(path, every=1):
    """Yield the screen as text after each output event of the recording."""
    stream = events = _events(path)
    header, _ = next(events)
    screen = pyte.Screen(header.get("width", 100), header.get("height", 30))
    feed = pyte.Stream(screen).feed
    for i, (_, data) in enumerate(stream):
        feed(data)
        if i % every == 0:
            yield "\n".join(row.rstrip() for row in screen.display)


def render(path, at=None):
    """The screen as it stands at `at` seconds, or at the end."""
    events = _events(path)
    header, _ = next(events)
    screen = pyte.Screen(header.get("width", 100), header.get("height", 30))
    feed = pyte.Stream(screen).feed
    for when, data in events:
        if at is not None and when > at:
            break
        feed(data)
    return "\n".join(row.rstrip() for row in screen.display)


def main():
    args = sys.argv[1:]
    if not args:
        print(__doc__, file=sys.stderr)
        return 2
    path, at, needle = args[0], None, None
    i = 1
    while i < len(args):
        if args[i] == "--at":
            at, i = float(args[i + 1]), i + 2
        elif args[i] == "--grep":
            needle, i = args[i + 1], i + 2
        else:
            i += 1
    if needle is None:
        print(render(path, at))
        return 0
    for text in frames(path):
        if needle.lower() in text.lower():
            print(f"FOUND {needle!r}")
            return 0
    print(f"MISSING {needle!r}")
    return 1


if __name__ == "__main__":
    raise SystemExit(main())
