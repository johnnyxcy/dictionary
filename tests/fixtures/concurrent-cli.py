#!/usr/bin/env python3
"""Gate-controlled concurrent CLI fixture. Never calls a model."""
import hashlib
import json
import os
import sys
import time
from pathlib import Path


if "exec" not in sys.argv:
    os.execv(sys.executable, [sys.executable, str(Path(__file__).with_name("activity-cli.py")), *sys.argv[1:]])


def emit(value):
    print(json.dumps(value, ensure_ascii=False), flush=True)


def item(kind, identifier, item_type, **fields):
    emit({"type": f"item.{kind}", "item": {"id": identifier, "type": item_type, **fields}})


prompt = sys.stdin.read()
text = json.loads(prompt.rsplit("lookup_input (JSON string): ", 1)[1].strip())
control = Path.cwd() / "fixture-control"
control.mkdir(exist_ok=True)
started = control / f"started-{os.getpid()}.json"
# Atomic publication lets the test reader inspect complete JSON immediately.
temporary = started.with_suffix(".tmp")
temporary.write_text(json.dumps({"input": text, "pid": os.getpid(), "args": sys.argv[1:], "prompt": prompt}))
temporary.replace(started)
emit({"type": "thread.started", "thread_id": f"fixture-{os.getpid()}"})
emit({"type": "turn.started"})
item("started", "reasoning", "reasoning", text=f"Checking the meaning of {text} (synthetic fixture).")
reverse = "Produce one Chinese-to-English" in prompt
partial = f"# {text}\n\n" + (f"Natural English equivalent of {text}" if reverse else f"测试翻译：{text}")
item("started", "answer", "agent_message", phase="final_answer", text=partial)
release = control / ("release-" + hashlib.sha256(text.encode()).hexdigest())
deadline = time.monotonic() + 120
while not release.exists():
    if time.monotonic() > deadline:
        raise SystemExit("Fixture was not released within 120 seconds")
    time.sleep(0.01)
if text == "simulate failure":
    emit({"type": "turn.failed", "error": {"message": "Synthetic concurrent failure"}})
    raise SystemExit(1)
item("completed", "reasoning", "reasoning", text=f"Checked the meaning of {text} (synthetic fixture).")
item("completed", "answer", "agent_message", phase="final_answer", text=partial + "\n\n*This is a synthetic concurrency test.*")
emit({"type": "turn.completed", "usage": {"input_tokens": 100 + len(text), "cached_input_tokens": 60, "output_tokens": 10 + len(text)}})
