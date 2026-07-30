#!/usr/bin/env python3
"""Verify that the primitives documentation still points at the code it describes.

`docs/primitives.md` names the mechanisms this project rests on and cites
`path:line` anchors. `docs/primitives.code.json` carries the line-by-line
annotations that the reader renders on top of those entries. Both address code
by line number, so an edit anywhere above a cited line moves the target without
touching the citation. Nothing fails loudly when that happens: the reader keeps
rendering, it just renders unrelated code under the old explanation.

This script is what makes the annotations checkable instead of trusted. It never
stores code that the reader displays -- the reader extracts every rendered line
from disk -- it stores fingerprints, and fails when a fingerprint no longer
matches.

Fingerprints are taken over whitespace-stripped lines on purpose:
re-indentation is not a content change, a changed statement is.

Usage:
    python3 scripts/check_primitives_anchors.py [--repo-root .]

Exit status 0 means every anchor resolves to the code it claims. Exit status 1
prints one line per stale anchor; repair them with reanchor.py from the reader
build tooling, or by hand when the code changed substantively.
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
SCHEMA = 1
ANCHOR_RE = re.compile(r"`([A-Za-z0-9_./\-]+\.[A-Za-z0-9_+]+):(\d+)(?:-(\d+))?`")


def fingerprint(lines: list[str]) -> str:
    return hashlib.sha256("\n".join(line.strip() for line in lines).encode("utf-8")).hexdigest()


class Checker:
    def __init__(self, root: Path) -> None:
        self.root = root
        self.problems: list[str] = []
        self._files: dict[str, list[str] | None] = {}

    def fail(self, where: str, message: str) -> None:
        self.problems.append(f"{where}: {message}")

    def lines(self, rel: str) -> list[str] | None:
        """Return the file's lines, or None when the path is gone."""
        if rel not in self._files:
            path = self.root / rel
            self._files[rel] = path.read_text(encoding="utf-8").splitlines() if path.is_file() else None
        return self._files[rel]

    def check_line(self, where: str, rel: str, number: int, expected: str) -> None:
        lines = self.lines(rel)
        if lines is None:
            self.fail(where, f"{rel} does not exist")
            return
        if not 1 <= number <= len(lines):
            self.fail(where, f"{rel} has {len(lines)} lines, so line {number} cannot be cited")
            return
        actual = lines[number - 1].strip()
        if actual != expected:
            self.fail(
                where,
                f"{rel}:{number} now reads {actual!r}, the annotation was written against {expected!r}",
            )

    def check_snippets(self, cards: list[dict]) -> tuple[int, int]:
        snippets = notes = 0
        for card in cards:
            title = card["title"]
            for index, snippet in enumerate(card["snippets"]):
                snippets += 1
                where = f"{title!r} snippet {index + 1}"
                rel, start, end = snippet["file"], snippet["start"], snippet["end"]
                lines = self.lines(rel)
                if lines is None:
                    self.fail(where, f"{rel} does not exist")
                    continue
                if not 1 <= start <= end:
                    self.fail(where, f"{rel} range {start}-{end} is not a forward range")
                    continue
                if end > len(lines):
                    self.fail(where, f"{rel} has {len(lines)} lines, the snippet claims {start}-{end}")
                    continue
                block = lines[start - 1 : end]
                if fingerprint(block) != snippet["sha256"]:
                    detail = f"{rel}:{start}-{end} changed since it was annotated"
                    if block[0].strip() != snippet["first"]:
                        detail += f"; first line is now {block[0].strip()!r}, expected {snippet['first']!r}"
                    elif block[-1].strip() != snippet["last"]:
                        detail += f"; last line is now {block[-1].strip()!r}, expected {snippet['last']!r}"
                    else:
                        detail += "; the boundary lines still match, so a line inside moved or changed"
                    self.fail(where, detail)
                for note in snippet["notes"]:
                    notes += 1
                    number = note["line"]
                    if not start <= number <= end:
                        self.fail(where, f"note on {rel}:{number} sits outside the snippet range {start}-{end}")
                        continue
                    self.check_line(where, rel, number, note["code"])
        return snippets, notes

    def check_doc_anchors(self, anchors: list[dict]) -> int:
        for anchor in anchors:
            ref = anchor["ref"]
            rel, _, span = ref.rpartition(":")
            first, _, last = span.partition("-")
            self.check_line(f"{MD} anchor `{ref}`", rel, int(first), anchor["code"])
            if last:
                self.check_line(f"{MD} anchor `{ref}`", rel, int(last), anchor["end_code"])
        return len(anchors)

    def check_titles(self, cards: list[dict], markdown: str) -> None:
        headings = [line[4:].strip() for line in markdown.splitlines() if line.startswith("### ")]
        titles = [card["title"] for card in cards]
        duplicates = {t for t in titles if titles.count(t) > 1}
        for title in sorted(duplicates):
            self.fail(CODE_JSON, f"card {title!r} appears more than once")
        for title in titles:
            if title not in headings:
                self.fail(CODE_JSON, f"card {title!r} has no `### ` heading in {MD}")
        for heading in headings:
            if heading not in titles:
                self.fail(MD, f"entry {heading!r} has no annotated code in {CODE_JSON}")

    def check_md_anchors_are_recorded(self, anchors: list[dict], markdown: str) -> None:
        recorded = {anchor["ref"] for anchor in anchors}
        for match in ANCHOR_RE.finditer(markdown):
            ref = match.group(0).strip("`")
            if ref in recorded:
                continue
            rel = ref.rpartition(":")[0]
            if (self.root / rel).is_file():
                self.fail(MD, f"anchor `{ref}` is not fingerprinted in {CODE_JSON}")
            else:
                self.fail(MD, f"anchor `{ref}` points at a path that does not exist")


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--repo-root", default=".", type=Path)
    args = parser.parse_args()
    root: Path = args.repo_root.resolve()

    code_json = root / CODE_JSON
    markdown_path = root / MD
    for path in (code_json, markdown_path):
        if not path.is_file():
            print(f"missing {path.relative_to(root)}", file=sys.stderr)
            return 1

    document = json.loads(code_json.read_text(encoding="utf-8"))
    if document.get("schema") != SCHEMA:
        print(f"{CODE_JSON}: schema {document.get('schema')!r} is not the expected {SCHEMA}", file=sys.stderr)
        return 1
    markdown = markdown_path.read_text(encoding="utf-8")

    checker = Checker(root)
    checker.check_titles(document["cards"], markdown)
    snippets, notes = checker.check_snippets(document["cards"])
    checker.check_md_anchors_are_recorded(document["doc_anchors"], markdown)
    anchors = checker.check_doc_anchors(document["doc_anchors"])

    if checker.problems:
        print(f"{len(checker.problems)} stale primitives anchor(s):", file=sys.stderr)
        for problem in checker.problems:
            print(f"  {problem}", file=sys.stderr)
        return 1

    print(
        f"primitives anchors verified: {len(document['cards'])} entries, "
        f"{snippets} snippets, {notes} notes, {anchors} document anchors"
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
