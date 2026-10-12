#!/usr/bin/env python3
"""Synthetic CLI for manual stream/collapse checks. Never calls a model."""
import json
import sys
import time
from pathlib import Path


def emit(value):
    print(json.dumps(value, ensure_ascii=False), flush=True)


def item(kind, identifier, item_type, **fields):
    emit({"type": f"item.{kind}", "item": {"id": identifier, "type": item_type, **fields}})


if "--version" in sys.argv:
    print("fixture-cli (synthetic activity test)")
elif "app-server" in sys.argv:
    for line in sys.stdin:
        request = json.loads(line)
        if "id" not in request:
            continue
        method = request["method"]
        result = {}
        if method == "config/read":
            result = {"config": {"model": "fixture-model"}}
        elif method == "model/list":
            result = {"data": [{"id": "fixture-model", "model": "fixture-model", "isDefault": True,
                               "supportedReasoningEfforts": [{"reasoningEffort": "low"}],
                               "defaultReasoningEffort": "low"}], "nextCursor": None}
        elif method == "skills/list":
            result = {"data": [{"skills": [{"name": "dict", "enabled": True,
                       "path": str(Path(__file__).with_name("activity-skill.md"))}]}]}
        emit({"id": request["id"], "result": result})
elif "exec" in sys.argv:
    prompt = sys.stdin.read()
    emit({"type": "thread.started", "thread_id": "synthetic-ui-check"})
    emit({"type": "turn.started"})
    time.sleep(2)
    item("started", "r1", "reasoning", text="Checking the phrase’s meaning.")
    time.sleep(3)
    item("updated", "r1", "reasoning", text="Checking the phrase’s meaning and register. Preparing bilingual examples.")
    time.sleep(3)
    item("completed", "r1", "reasoning", text="Checked the phrase’s meaning and register. Prepared bilingual examples.")
    item("completed", "comment", "agent_message", phase="commentary", text="Formatting pronunciation and the Chinese explanation.")
    time.sleep(2)
    partial = "# break the ice\n\n打破僵局；让气氛轻松。"
    item("started", "answer", "agent_message", phase="final_answer", text=partial)
    time.sleep(4)
    if "simulate failure" in prompt:
        emit({"type": "turn.failed", "error": {"message": "Synthetic connection failure after partial output"}})
        sys.exit(1)
    answer = partial + "\n\n- **发音**：/breɪk ði aɪs/\n\n## 例句\n\nA joke helped break the ice.\n\n一个笑话让气氛轻松了。\n\n*This entry is a synthetic UI test fixture.*"
    item("completed", "answer", "agent_message", phase="final_answer", text=answer)
    emit({"type": "turn.completed", "usage": {"input_tokens": 100, "cached_input_tokens": 60, "output_tokens": 30}})
