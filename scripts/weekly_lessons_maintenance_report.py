#!/usr/bin/env python3
"""Generate a review-only stale/duplicate lessons report."""

from __future__ import annotations

import argparse
import datetime as dt
import pathlib
import re
from collections import defaultdict


def parse_args() -> argparse.Namespace:
    p = argparse.ArgumentParser()
    p.add_argument("--vault-dir", default=".obsidian")
    p.add_argument("--stale-days", type=int, default=30)
    return p.parse_args()


def read_frontmatter(md: pathlib.Path) -> dict[str, str]:
    text = md.read_text(encoding="utf-8", errors="ignore")
    if not text.startswith("---\n"):
        return {}
    end = text.find("\n---\n", 4)
    if end == -1:
        return {}
    block = text[4:end]
    out: dict[str, str] = {}
    for line in block.splitlines():
        if ":" not in line:
            continue
        k, v = line.split(":", 1)
        out[k.strip()] = v.strip()
    return out


def parse_dt(s: str | None) -> dt.datetime | None:
    if not s:
        return None
    try:
        return dt.datetime.fromisoformat(s.replace("Z", "+00:00")).astimezone(dt.UTC)
    except ValueError:
        return None


def main() -> int:
    args = parse_args()
    vault = pathlib.Path(args.vault_dir)
    refs = vault / "references"
    refs.mkdir(parents=True, exist_ok=True)

    now = dt.datetime.now(dt.UTC)
    stale_cutoff = now - dt.timedelta(days=args.stale_days)

    lessons = []
    for p in vault.rglob("*.md"):
        if p.name == "Lessons-Index.md":
            continue
        fm = read_frontmatter(p)
        h = fm.get("lesson_hash")
        if not h:
            continue
        updated = parse_dt(fm.get("updated")) or parse_dt(fm.get("created"))
        lessons.append((p, h, updated))

    dup = defaultdict(list)
    stale = []
    for p, h, updated in lessons:
        dup[h].append(p)
        if updated and updated < stale_cutoff:
            stale.append((p, updated))

    duplicate_groups = [paths for paths in dup.values() if len(paths) > 1]

    out = refs / "weekly-lessons-maintenance.md"
    lines = [
        "# Weekly Lessons Maintenance",
        "",
        f"Generated: {now.isoformat()}",
        f"Stale threshold days: {args.stale_days}",
        "",
        "## Summary",
        "",
        f"- Total lesson notes: {len(lessons)}",
        f"- Stale notes: {len(stale)}",
        f"- Duplicate hash groups: {len(duplicate_groups)}",
        "",
        "## Stale Notes",
        "",
    ]

    if not stale:
        lines.append("_None_")
    else:
        for p, updated in sorted(stale, key=lambda x: x[1]):
            rel = p.relative_to(vault.parent).as_posix()
            lines.append(f"- [{rel}]({rel}) updated={updated.isoformat()}")

    lines.extend(["", "## Duplicate lesson_hash Groups", ""])
    if not duplicate_groups:
        lines.append("_None_")
    else:
        for paths in duplicate_groups:
            lines.append("- group")
            for p in sorted(paths):
                rel = p.relative_to(vault.parent).as_posix()
                lines.append(f"  - [{rel}]({rel})")

    lines.extend([
        "",
        "## Review Guidance",
        "",
        "This report is review-only. No files were deleted or modified.",
        "Decide manually whether to merge, archive, or keep duplicated lessons.",
    ])

    out.write_text("\n".join(lines) + "\n", encoding="utf-8")
    print(f"wrote {out}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
