#!/usr/bin/env python3
"""Repair primitives anchors after the code they point at moved.

`scripts/check_primitives_anchors.py` fails when a snippet range or a note no
longer covers the code it was written against. Most of those failures are a pure
shift: something was inserted above, every line number below is off by the same
delta, and the code itself is unchanged. This script fixes that class
mechanically, by content, and refuses to guess at the rest.

What it can repair:
  * a snippet whose recorded content appears elsewhere in the same file
  * a snippet whose first and last lines still exist, with lines added inside
  * a note whose recorded line text appears exactly once in the new range
  * a `path:line` anchor in docs/primitives.md whose line text moved

What it deliberately cannot repair: code that changed substantively. There is no
content left to match, and the explanation written about the old code is
probably wrong now, so a human has to reread it. Those are listed and the exit
status is 1.

This is a vendored copy of tempoloss/tempoloss@08db8bea `tools/primitives/reanchor.py`
with the doc-anchor repair replaced: a range is now moved by a single shift
applied to both ends, so the span can neither change nor invert.

Usage:
    python3 scripts/reanchor_primitives.py --repo-root .            # preview
    python3 scripts/reanchor_primitives.py --write                  # apply
"""

from __future__ import annotations

import argparse
import hashlib
import json
import re
import sys
from pathlib import Path

MD = "docs/primitives.md"
CODE_JSON = "docs/primitives.code.json"
ANCHOR_IN_MD = re.compile(r"`([A-Za-z0-9_./\-]+\.[A-Za-z0-9_+]+):(\d+)(?:-(\d+))?`")


def fingerprint(lines: list[str]) -> str:
    return hashlib.sha256("\n".join(line.strip() for line in lines).encode("utf-8")).hexdigest()


def find_windows(lines: list[str], digest: str, length: int) -> list[int]:
    """Every 1-based start whose `length` lines fingerprint to `digest`."""
    return [start + 1 for start in range(max(0, len(lines) - length + 1))
            if fingerprint(lines[start:start + length]) == digest]


def find_line(lines: list[str], text: str, lo: int = 1, hi: int | None = None) -> list[int]:
    """Every 1-based line in [lo, hi] whose stripped text equals `text`."""
    hi = len(lines) if hi is None else min(hi, len(lines))
    return [n for n in range(lo, hi + 1) if lines[n - 1].strip() == text]


def closest(deltas: list[int]) -> int | None:
    """The smallest shift, when one is unambiguously smallest.

    A shift is what an insertion above does to every line below it, so the
    candidate to trust is the smallest one. Two candidates equally far in
    opposite directions are a guess, and a guess is what this refuses.
    """
    ranked = sorted(deltas, key=lambda d: (abs(d), d))
    if not ranked:
        return None
    if len(ranked) > 1 and abs(ranked[1]) == abs(ranked[0]):
        return None
    return ranked[0]


class Reanchor:
    def __init__(self, root: Path) -> None:
        self.root = root
        self.unresolved: list[str] = []
        self.changes: list[str] = []
        self._files: dict[str, list[str]] = {}

    def lines(self, rel: str) -> list[str] | None:
        if rel not in self._files:
            path = self.root / rel
            if not path.is_file():
                return None
            self._files[rel] = path.read_text(encoding="utf-8").splitlines()
        return self._files.get(rel)

    # -- snippets ---------------------------------------------------------

    def relocate(self, rel: str, snippet: dict) -> tuple[int, int] | None:
        """Find the snippet's new range, or None when the code changed."""
        lines = self.lines(rel)
        length = snippet["end"] - snippet["start"] + 1
        exact = find_windows(lines, snippet["sha256"], length)
        if len(exact) == 1:
            return exact[0], exact[0] + length - 1
        if len(exact) > 1:
            self.unresolved.append(f"{rel}:{snippet['start']}-{snippet['end']} matches {len(exact)} places, "
                                   "too ambiguous to move automatically")
            return None
        start, end = snippet["start"], snippet["end"]
        in_place = (end <= len(lines) and lines[start - 1].strip() == snippet["first"]
                    and lines[end - 1].strip() == snippet["last"])
        if in_place:
            self.unresolved.append(
                f"{rel}:{start}-{end} still starts and ends where it did, so a line inside it changed; "
                "reread the notes on this snippet before moving anything")
            return None
        # Lines were added or removed inside the block: pin the boundaries.
        pairs = [(first, last)
                 for first in find_line(lines, snippet["first"])
                 for last in find_line(lines, snippet["last"], first, first + 4 * length)]
        if len(pairs) == 1:
            return pairs[0]
        shift = closest([first - start for first, _ in pairs])
        candidates = [pair for pair in pairs if shift is not None and pair[0] == start + shift]
        if len(candidates) == 1:
            return candidates[0]
        reason = ("its first and last line no longer bound a block" if not pairs
                  else f"its first and last line bound {len(pairs)} blocks, none of them clearly the old one")
        self.unresolved.append(f"{rel}:{start}-{end} changed: {reason}")
        return None

    def move_note(self, rel: str, note: dict, start: int, end: int, delta: int) -> bool:
        lines = self.lines(rel)
        shifted = note["line"] + delta
        if start <= shifted <= end and lines[shifted - 1].strip() == note["code"]:
            note["line"] = shifted
            return True
        hits = find_line(lines, note["code"], start, end)
        if len(hits) == 1:
            note["line"] = hits[0]
            return True
        self.unresolved.append(
            f"{rel}: note on {note['code']!r} has {len(hits)} matches in the new range {start}-{end}; "
            "reread the note, the code it explains changed")
        return False

    def fix_snippets(self, cards: list[dict]) -> None:
        for card in cards:
            for snippet in card["snippets"]:
                rel = snippet["file"]
                lines = self.lines(rel)
                if lines is None:
                    self.unresolved.append(f"{rel} no longer exists; the entry {card['title']!r} needs rewriting")
                    continue
                start, end = snippet["start"], snippet["end"]
                if end <= len(lines) and fingerprint(lines[start - 1:end]) == snippet["sha256"]:
                    continue
                moved = self.relocate(rel, snippet)
                if moved is None:
                    continue
                new_start, new_end = moved
                delta = new_start - start
                for note in snippet["notes"]:
                    self.move_note(rel, note, new_start, new_end, delta)
                snippet["start"], snippet["end"] = new_start, new_end
                block = lines[new_start - 1:new_end]
                snippet["sha256"] = fingerprint(block)
                snippet["first"], snippet["last"] = block[0].strip(), block[-1].strip()
                self.changes.append(f"{rel}: snippet {start}-{end} -> {new_start}-{new_end}")

    # -- document anchors -------------------------------------------------

    def fix_doc_anchors(self, anchors: list[dict], markdown: str) -> str:
        for anchor in anchors:
            rel, _, span = anchor["ref"].rpartition(":")
            lines = self.lines(rel)
            if lines is None:
                self.unresolved.append(f"{MD}: anchor `{anchor['ref']}` points at a path that is gone")
                continue
            head, _, tail = span.partition("-")
            first = int(head)
            last = int(tail) if tail else None

            def holds(number: int, expected: str) -> bool:
                return 1 <= number <= len(lines) and lines[number - 1].strip() == expected

            if holds(first, anchor["code"]) and (last is None or holds(last, anchor["end_code"])):
                continue
            deltas = [hit - first for hit in find_line(lines, anchor["code"])]
            if last is not None:
                deltas = [d for d in deltas if holds(last + d, anchor["end_code"])]
            shift = closest(deltas)
            if shift is None:
                self.unresolved.append(
                    f"{MD}: anchor `{anchor['ref']}` cited {anchor['code']!r} and "
                    f"{len(deltas)} shift(s) fit; the code it points at changed")
                continue
            new_ref = f"{rel}:{first + shift}" + (f"-{last + shift}" if last is not None else "")
            if new_ref == anchor["ref"]:
                continue
            markdown = markdown.replace(f"`{anchor['ref']}`", f"`{new_ref}`")
            self.changes.append(f"{MD}: `{anchor['ref']}` -> `{new_ref}`")
            anchor["ref"] = new_ref
        return markdown


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--repo-root", type=Path, default=Path("."))
    parser.add_argument("--write", action="store_true", help="apply the repairs (default: preview only)")
    args = parser.parse_args()
    root = args.repo_root.resolve()

    code_path, md_path = root / CODE_JSON, root / MD
    document = json.loads(code_path.read_text(encoding="utf-8"))
    markdown = md_path.read_text(encoding="utf-8")

    fixer = Reanchor(root)
    fixer.fix_snippets(document["cards"])
    markdown = fixer.fix_doc_anchors(document["doc_anchors"], markdown)

    # An anchor added to the prose by hand has no fingerprint yet; record it so
    # the checker starts guarding it too.
    recorded = {anchor["ref"] for anchor in document["doc_anchors"]}
    for match in ANCHOR_IN_MD.finditer(markdown):
        ref = match.group(0).strip("`")
        if ref in recorded:
            continue
        rel, first, last = match.group(1), int(match.group(2)), match.group(3)
        lines = fixer.lines(rel)
        if lines is None or first > len(lines) or (last and int(last) > len(lines)):
            fixer.unresolved.append(f"{MD}: new anchor `{ref}` does not resolve in {rel}")
            continue
        entry = {"ref": ref, "code": lines[first - 1].strip()}
        if last:
            entry["end_code"] = lines[int(last) - 1].strip()
        document["doc_anchors"].append(entry)
        recorded.add(ref)
        fixer.changes.append(f"{MD}: fingerprinted new anchor `{ref}`")

    for line in fixer.changes:
        print(("applied  " if args.write else "would fix ") + line)
    if not fixer.changes:
        print("no anchor moved")

    crossed = [anchor["ref"] for anchor in document["doc_anchors"]
               if "-" in anchor["ref"].rpartition(":")[2]
               and int(anchor["ref"].rpartition(":")[2].split("-")[1])
               < int(anchor["ref"].rpartition(":")[2].split("-")[0])]
    for ref in crossed:
        fixer.unresolved.append(f"{MD}: anchor `{ref}` ends before it starts; refusing to write it")

    if args.write and fixer.changes and not crossed:
        # newline="": the documents are LF in the repository, and a repair run on
        # Windows would otherwise rewrite every line of both of them.
        code_path.write_text(
            json.dumps(document, ensure_ascii=False, indent=1) + "\n",
            encoding="utf-8",
            newline="",
        )
        md_path.write_text(markdown, encoding="utf-8", newline="")

    if fixer.unresolved:
        print(f"\n{len(fixer.unresolved)} anchor(s) need a human:", file=sys.stderr)
        for problem in fixer.unresolved:
            print(f"  {problem}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
