#!/usr/bin/env python3
"""iterapp statusline collector.

Claude Code (v2.1.80+) pipes a JSON status payload to the configured statusline
command after each API response; on Pro/Max accounts it includes server-authoritative
`rate_limits` (5-hour and 7-day used_percentage + resets_at). This script tees that
object into a machine-wide snapshot file the iterloop engine reads for tiered agent
throttling, then prints a short status line.

Usage: statusline-collector.py [snapshot_path]
Default snapshot path: ~/.claude/iter-usage-snapshot.json

Wire it up in any Claude Code settings.json (the engine does this automatically for
its probe session; adding it to your own ~/.claude/settings.json statusLine makes
every interactive session refresh the snapshot for free):
  { "statusLine": { "type": "command", "command": "/path/to/statusline-collector.py" } }
"""
import datetime
import json
import os
import sys
import tempfile

out = sys.argv[1] if len(sys.argv) > 1 else os.path.expanduser("~/.claude/iter-usage-snapshot.json")

try:
    data = json.load(sys.stdin)
except Exception:
    data = {}

rl = data.get("rate_limits")
if isinstance(rl, dict) and rl:
    snap = {
        "ts": datetime.datetime.now(datetime.timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ"),
        "rate_limits": rl,
    }
    try:
        os.makedirs(os.path.dirname(out), exist_ok=True)
        fd, tmp = tempfile.mkstemp(dir=os.path.dirname(out), prefix=".iter-usage-")
        with os.fdopen(fd, "w") as f:
            json.dump(snap, f)
        os.replace(tmp, out)
    except Exception:
        pass  # never break the statusline over a snapshot write

def pct(key):
    w = (rl or {}).get(key) or {}
    v = w.get("used_percentage")
    return f"{round(v)}%" if isinstance(v, (int, float)) else "?"

model = (data.get("model") or {}).get("display_name", "")
print(f"{model} · 5h {pct('five_hour')} · 7d {pct('seven_day')} · iter-collector")
