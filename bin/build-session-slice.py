#!/usr/bin/env python3
"""Build a YAML session slice from a Claude Code JSONL transcript.

Filters to user + assistant turns only, distils tool-use turns, and
truncates per-turn text. Output goes to stdout in the shape the
facet-extract-v2 pattern expects.
"""
import json
import sys
from pathlib import Path

import yaml


def turn_text(entry: dict) -> str:
    msg = entry.get("message", {})
    content = msg.get("content")
    if isinstance(content, str):
        return content
    if isinstance(content, list):
        parts = []
        for blk in content:
            t = blk.get("type")
            if t == "text":
                parts.append(blk.get("text", ""))
            elif t == "tool_use":
                name = blk.get("name", "?")
                inp = blk.get("input", {})
                first_arg = next(iter(inp.values()), "") if isinstance(inp, dict) else ""
                if isinstance(first_arg, str) and len(first_arg) > 120:
                    first_arg = first_arg[:120] + "…"
                parts.append(f"[tool_use:{name}({first_arg!r})]")
            elif t == "tool_result":
                content_val = blk.get("content", "")
                if isinstance(content_val, list):
                    content_val = " ".join(
                        c.get("text", "")[:200] for c in content_val if isinstance(c, dict)
                    )
                if isinstance(content_val, str) and len(content_val) > 200:
                    content_val = content_val[:200] + "…"
                parts.append(f"[tool_result: {content_val}]")
        return "\n".join(parts)
    return ""


def main():
    if len(sys.argv) < 3:
        print("usage: build-session-slice.py <jsonl> <start-line> [end-line]", file=sys.stderr)
        sys.exit(1)
    src = Path(sys.argv[1])
    start = int(sys.argv[2])
    end = int(sys.argv[3]) if len(sys.argv) > 3 else None

    turns = []
    with src.open() as f:
        for i, line in enumerate(f, start=1):
            if i < start:
                continue
            if end is not None and i > end:
                break
            try:
                entry = json.loads(line)
            except json.JSONDecodeError:
                continue
            kind = entry.get("type")
            if kind not in ("user", "assistant"):
                continue
            msg = entry.get("message", {})
            role = msg.get("role", kind)
            text = turn_text(entry)
            if not text.strip():
                continue
            # Cap each turn at 3000 chars for the prompt budget.
            if len(text) > 3000:
                text = text[:3000] + "…"
            turns.append({
                "uuid": entry.get("uuid", ""),
                "role": role,
                "timestamp": entry.get("timestamp", ""),
                "text": text,
            })

    out = {
        "workitem_slug": "facet-v2-design-and-prototype",
        "workitem_title": "facet v2: dialog-slice gems and narrative spectra design",
        "repo_slug": "scottidler/second-brain",
        "turns": turns,
    }
    print(yaml.safe_dump(out, sort_keys=False, allow_unicode=True, width=10**9))


if __name__ == "__main__":
    main()
