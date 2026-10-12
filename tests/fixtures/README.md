# CLI contract fixtures

`exec-0.162.1-success.jsonl` was captured on 2026-10-12 from the installed Codex CLI 0.162.1, using `gpt-6-astra` at `low` effort and the user's local `dict` skill to look up “serendipity”. Only the opaque thread ID was replaced with `fixture-thread`.

The observed stream delivers one completed agent message, then terminal usage. Cached input is already included in input tokens. The additional `cache_write_input_tokens` field demonstrates that the parser tolerates counters it does not yet consume. Reasoning output was reported as zero in this capture; fixtures and validation also cover nonzero subset counts and absent fields.

Process tests replay this capture through a local fake executable. Normal tests perform no model calls and do not depend on a logged-in Codex account.

`activity-cli.py` is a **synthetic** manual UI fixture. It implements just enough metadata discovery and delayed JSONL events to check thinking, work messages, partial answers, completion, and automatic activity collapse. The generated entry is explicitly marked as a fixture. Its companion `activity-skill.md` is also test-only. The supported event families are described in the [official non-interactive documentation](https://learn.chatgpt.com/docs/non-interactive-mode#make-output-machine-readable).

After `cargo build --locked`, launch an isolated profile:

```sh
python3 scripts/bundle-dev.py --name 'Dictionary Stream Test' \
  --data-dir /tmp/dictionary-stream-test \
  --cli "$PWD/tests/fixtures/activity-cli.py"
open 'target/Dictionary Stream Test.app'
```

Submit `break the ice` for success, or `simulate failure` for a failure after partial output. Each run takes approximately 14 seconds. No credentials or external service are used. Never use these synthetic counters as real usage data.

## Concurrent requests

`concurrent-cli.py` supplies per-input partial answers, activity, and token counters. Each process waits for a release file, allowing tests to control completion order without timing guesses. Automated service tests exercise three simultaneous processes, queue advancement, active-key deduplication, cached navigation at capacity, regeneration, targeted cancellation, failure isolation, and shutdown cleanup. View-state tests separately cover delayed navigation and background updates.

```sh
python3 scripts/bundle-dev.py --name 'Dictionary Concurrency Review' \
  --data-dir /tmp/dictionary-concurrency-ui-qa \
  --cli "$PWD/tests/fixtures/concurrent-cli.py"
open 'target/Dictionary Concurrency Review.app'
```

Submit several distinct inputs. The first three run; subsequent entries show Queued. To finish one, create its gate file:

```sh
python3 - <<'PY'
from hashlib import sha256
from pathlib import Path
text = 'alpha'  # Exact submitted text, including whitespace.
control = Path('/tmp/dictionary-concurrency-ui-qa/runtime/fixture-control')
control.mkdir(parents=True, exist_ok=True)
(control / ('release-' + sha256(text.encode()).hexdigest())).touch()
PY
```

Use Stop to cancel the selected request. An input of `simulate failure` fails when released. Gates remain on disk, so reuse a fresh data directory for another controlled run. The fixture exits after two minutes if never released. `started-*.json` files record process IDs and original inputs; queued cancellations never produce them. This fixture never calls a model.
