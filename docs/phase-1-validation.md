# Phase 1 validation

Validated on 2026-10-12 with macOS 26.6.2, Rust 1.99.0, GPUI Kit 0.7.2, and Codex CLI 0.162.1.

## Automated checks

- `cargo test --locked --no-default-features`: 46 tests pass without external model calls (8 coordinator tests, 1 database-lock regression test, 32 core integration tests, and 5 view-state tests).
- `cargo clippy --locked --all-targets --all-features -- -D warnings`: passes.
- `cargo fmt --check`: passes.
- `cargo build --locked`: native application builds successfully.

Cargo reports an upstream future-compatibility warning for `block 0.1.6`. It does not prevent this pinned toolchain from building. Application code passes the strict Clippy check.

The tests cover observed JSONL byte splits and Unicode, usage subset accounting, missing counters, duplicate terminal/item snapshots, malformed or truncated output, literal stdin input, model/skill errors, SQLite reopen/recovery, the single-writer lock, durable completion timing, process spawn/exit errors, login diagnostics, blocked pipes, incomplete prompt delivery, cancellation, timeout, bounded real activity snapshots, and Chinese translation previews. Cache tests verify offline resolution, matching semantics, lookup beyond the latest 200 entries, newest-success selection, failed-regeneration fallback, unchanged previous results/usage, sequential attempts under one lookup, and transactional migration from schema version 1.

Deletion tests verify that removing a lookup deletes every version and its measurements, invalidates cached navigation, and remains deleted after reopening the database. They also verify preservation of unrelated entries and shared skill snapshots, cleanup of unused snapshots, rejection while any version is pending/running, and rollback of the entire deletion on an injected storage failure. Searching a deleted expression creates a new lookup rather than returning a stale cached result.

## Live service and native UI checks

The real CLI used the configured `gpt-6-astra` model with Low effort and the discovered local `en-zh-dictionary:dict` skill. All three lookups ran through the native app and were saved to an isolated test database.

| Input | Result reviewed | Reported input + output | Approximate duration |
| --- | --- | --- | --- |
| `serendipity` | Chinese meanings, IPA, beginner English definition, bilingual examples | 17,856 + 577 = 18,433 | 22.6 s |
| `break the ice` | Idiomatic meaning, pronunciation, usage and examples | 18,757 + 549 = 19,306 | 25.0 s |
| `I wish I had known earlier.` | Natural Chinese translation, IPA, regret/past-perfect explanation and examples | 17,858 + 644 = 18,502 | 24.4 s |

These are acceptance samples, not performance benchmarks. Cached input was 13,184 in each attempt, already included in input. The model reported zero reasoning-output tokens in these samples.

Verified Command+Enter submits without inserting a newline; multiline input remains available. Native Markdown displayed Chinese and IPA correctly, including scrolling to the sentence's grammar and usage notes. Saved history could be opened while another lookup ran, without blocking the UI or cancelling the active request.

Restarted the app with an explicitly missing CLI executable. The setup error was visible and saved entries remained readable. The three attempt IDs, answer hashes, usage counters, and durations were unchanged after reopening history; no extra attempts were created.

## Sidebar and activity behavior

The 232-point sidebar presents the original input and a Chinese gloss taken from the saved entry. Only active and failed attempts show a status, inline with the translation preview. The status spinner or icon is centered on the first preview line, including when it wraps. Sidebar rows have accessible button labels containing the input and preview, including status when applicable.

Each history row has a trash action labeled `Delete lookup and all versions`. Native inspection verified that only the hovered row shows the icon, that text does not move as it appears, and that the action is absent from every version while their lookup is running. The action removes the whole lookup from SQLite; automated deletion and rollback checks use isolated test databases.

The compact search input grows from one to four lines. The main translation fills the remaining pane under a slim toolbar; detailed usage, configuration, and timestamps are in an info popover. Verified opening and dismissing that popover, multiline input and Command+Enter submission, and hiding/restoring history using the sidebar icon and Command+B. Icon actions include new lookup, history visibility, search/stop, copy, details, and reconnect, with accessible names and tooltips.

The toolbar icons remain vertically centered against a multiline input with history both visible and hidden. New Lookup retains the descriptive text and keyboard hints without the example buttons. Active requests with no answer show the input and a pulsing dictionary-entry skeleton; actual reported work stays in the separate activity section. The skeleton yields to the answer as soon as text arrives and disappears when the request ends. It uses GPUI's reduced-motion-aware animation.

A separate synthetic CLI fixture validates the UI's intermediate states without depending on whether a particular model emits reasoning summaries. It exposes the same JSONL event families as the [official CLI documentation](https://learn.chatgpt.com/docs/non-interactive-mode#make-output-machine-readable).

- Activity appears only after reported content arrives, then expands while a lookup runs with changing thinking text and work messages.
- Partial answer text updates the entry and sidebar translation preview.
- Completion removes the inline running status and collapses the activity to `Worked for …`.
- The Show activity control reopens the retained thinking/work record.
- A failure after partial output keeps the available answer and marks the attempt Failed.

Activity snapshots are deduplicated by item ID and stored in the attempt. Older records without activity remain readable. The initial implementation added synthetic session-start and waiting rows; those have now been removed from new runs and hidden in existing entries. Empty activity has no disclosure arrow. The app shows the CLI's exposed summaries and progress; it does not manufacture a token stream or detailed reasoning when none is supplied. The real Low-effort samples above produced a completed answer item without intermediate reasoning text.

The local catalog sets the configured model's default reasoning summary to `none`. A further real lookup of `on the fence` used explicit `model_reasoning_summary="auto"` and `hide_agent_reasoning=false`. It succeeded in approximately 26.1 seconds with 17,855 input + 599 output = 18,454 reported tokens, including 13,184 cached input and zero reasoning-output tokens. No activity events were reported; native UI inspection confirmed a static completion label with no empty disclosure. These flags request available summaries, as described in the [configuration reference](https://learn.chatgpt.com/docs/config-file/config-reference), but do not guarantee the model emits them.

See [fixture instructions](../tests/fixtures/README.md) to reproduce the controlled stream. Synthetic results and counters are kept separate from real CLI acceptance data.

## Cached search and regeneration

An isolated native test profile began with four saved attempts. Searching `break the ice` immediately selected its latest successful entry and kept the database at four attempts. Clicking Regenerate to the left of Copy created attempt 2 under that same lookup, increasing the total to five. Editing the input to a different draft before clicking Regenerate still generated the selected entry's original text. Previous results remained visible in history. Searching the expression again selected the newly completed attempt and did not generate another result. The action's accessible name and tooltip are now `Copy`.

A separate profile pointed at a missing CLI executable. Searching `  on the fence  ` still opened the saved real result, with Regenerate disabled and no empty activity control. Cache lookup ran despite the connection error; it preserved the single existing attempt and its measurements.

## Concurrent lookups

The coordinator allows three running CLI processes and queues additional attempts in submission order. Admission reserves the normalized key after saving the pending record and before accepting the next command. Look up remains available; Stop sits beside the selected entry’s running or queued status. Request IDs protect navigation, and progress, activity, usage, failures, cancellation handles, and timings belong to their individual attempts.

Automated tests use a gate-controlled local executable to prove actual process overlap and deterministic completion order. They cover ten duplicate clicks before the first response, queued and running deduplication, cache access at capacity and without setup, regeneration taking priority over older cache, queue advancement, queued cancellation without spawning, cancellation/failure isolation, and quitting with three running plus two queued attempts. Every owned process is reaped and every accepted attempt receives a terminal saved outcome. A reproduced macOS lock race is covered by a test that retains a duplicate lock descriptor; explicit unlocking now releases ownership after SQLite closes.

View-state tests verify that delayed open responses cannot override a newer selection or New Lookup, another job’s completion does not erase live previews or collapse selected activity, late progress cannot overwrite a final answer, background save failures retain their result without navigation, and cached entries outside the recent-history window stay visible while selected.

Native UI checks ran in an isolated synthetic profile:

- Submitted `alpha`, `bravo`, and `charlie` concurrently, then `delta`, which showed Queued. SQLite contained four attempts and the fixture had started exactly three processes.
- Repeated Look up clicks reused `delta`. Its toolbar Stop cancelled it without starting a fourth process.
- Queued `echo`, selected `alpha`, and finished `bravo`. `echo` started while the main pane continued to show `alpha` and its own reported activity.
- Searched completed `bravo` with three other processes active. It opened the cached result; the database stayed at five attempts and four total process launches.
- Stopped `alpha`; `charlie` and `echo` continued. New Lookup with an unsent draft remained selected when both finished.
- The completed `echo` entry retained its own output and 118 synthetic tokens, with activity collapsed. Details reported approximately 9.4 seconds queued and 39.5 seconds total response time. The Worked label uses CLI runtime (approximately 30.0 seconds), excluding queue wait.
- All four fixture process IDs were gone after completion/cancellation. The queued cancellation had no start time, CLI runtime, or reported usage. These fixture timings and token counts are test data, not performance or billing measurements.

The repository pre-commit hook passes both formatting and strict Clippy checks, and the native application builds successfully.

## Keyboard submission

The input now uses the textarea’s submit-on-enter behavior: Enter submits, Shift+Enter inserts a newline, and Command+Enter remains an additional shortcut. The existing marked-text guard is retained so composing text is not submitted. The Look up tooltip and New Lookup hints show the new primary shortcut.

An isolated native fixture profile verified empty Enter creates neither an attempt nor a newline, Shift+Enter creates a real line break without a request, Enter submits the exact two-line input without appending a newline, repeated Enter opens the same cached result, and Command+Enter still submits. SQLite contained exactly the two expected successful attempts. Formatting, strict Clippy, and the native build pass.

## Remaining phases

Reverse lookup, model/effort settings, searchable/paginated history, analytics charts, and release packaging remain in their planned phases. Basic cached navigation, regeneration, and deletion are implemented. Broad Chinese IME, accessibility, themes, and public distribution acceptance belong to the macOS release phase. The current bundle is for local development.
