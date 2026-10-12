# Dictionary

A native macOS learning dictionary built with Rust and GPUI Kit. It runs an independent `codex exec` request with your installed dictionary skill for each lookup and keeps entries and measurements in local SQLite history.

Phase 1 implements English → Simplified Chinese words, phrases, and sentences. See the [implementation plan](plans/dictionary-app.md) for reverse lookups, settings, searchable history, and analysis charts in later phases.

## Run

Requirements:

- macOS 15 or later and Xcode Command Line Tools.
- Rust through rustup; this repository pins Rust 1.99.0.
- An installed, authenticated Codex CLI. The tested CLI version is 0.162.1.
- Your installed `dict` or `en-zh-dictionary` skill.

```sh
cargo run --locked
```

The app reads the available model catalog and your configured CLI model, then selects Low effort when supported. The chosen pair appears at the bottom of the sidebar. It reuses CLI authentication and leaves your global CLI configuration unchanged.

Enter text and press **Enter**, or click **Look up**. **Shift+Enter** inserts a newline, and the compact input expands for multiline text. **Command+Enter** also submits. **Command+L** focuses the input and **Command+B** toggles the history sidebar. The result supports selection, scrolling, and the **Copy** icon copies it as Markdown. Up to **three lookups run concurrently**; additional requests wait in a first-in, first-out queue. **Look up** always submits the current input. **Stop**, beside the selected entry’s running status, cancels only that entry (or removes it from the queue before it starts). New Lookup and switching history entries leave other requests running; their completion never changes your selected view. Select an item in History to read its saved result without generating another entry.

Searching an expression that is already running or queued opens that same attempt. Otherwise, searching an already saved expression opens its latest successful result, including while all slots are busy or the CLI is unavailable. It makes no model request and adds no attempt or token usage. Matching ignores surrounding whitespace and canonical Unicode differences, but preserves case, punctuation, and internal whitespace. Failed or incomplete attempts never replace a successful cached result. Use **Regenerate**, immediately left of Copy, to request a fresh entry for the selected result's original input using the current configuration. Earlier attempts and their measurements remain in history. Repeated clicks on Look up or Regenerate reuse an active attempt for the same expression.

Each sidebar item shows the original input and Chinese translation preview. Running and failed states appear inline with the preview; successful entries have no status label. Icons stay aligned with the first preview line when text wraps. The translation fills the main pane beneath a slim toolbar; the **Entry details** info icon opens token breakdowns, configuration, and timestamps without taking permanent space from the entry.

Hover a history row to reveal its trash icon, which **deletes that lookup and all its regenerated versions**, including their saved output, token usage, and timings. It removes them from SQLite and cached search. This is permanent; there is no archive or undo. The icon stays hidden while any version of that lookup is running or queued. Unrelated entries and their shared skill snapshots are preserved.

The search toolbar centers its icons against the input, including multiline text. New Lookup keeps the description and keyboard hints without example buttons. While a request is active and has no answer yet, a pulsing entry skeleton fills the result pane; it gives way to the first available answer and stops when the request ends. The animation respects the system's reduced-motion setting.

During a lookup, the activity panel displays reported thinking summaries, progress messages, and tool work. It appears only once real content arrives, collapses to **Worked for …** using CLI runtime (excluding queue wait) when complete, and can be expanded again from saved history. When the CLI supplies no activity, the app shows only running/completion status, without an empty disclosure. Old synthetic waiting messages are hidden when viewing existing history.

Each request selects `model_reasoning_summary="auto"` and `hide_agent_reasoning=false` through local CLI arguments, following the [configuration reference](https://learn.chatgpt.com/docs/config-file/config-reference). This requests available summaries without changing global settings or the selected thinking effort. The amount and timing of information still depend on the model and CLI stream; some completed lookups have no reported activity.

To create a local development app bundle after building:

```sh
cargo build --locked
python3 scripts/bundle-dev.py
open 'target/Dictionary Dev.app'
```

This is a development bundle. Signing, notarization, and distribution are outside Phase 1.

## Local configuration and history

Phase 1 has a visible fixed model/effort pair and a **Reconnect** button. A model picker and persisted settings arrive in Phase 2. Optional environment overrides are available now:

| Variable | Purpose |
| --- | --- |
| `DICTIONARY_CLI` | Absolute path to the CLI executable. Otherwise search PATH and common macOS installation locations. |
| `DICTIONARY_MODEL` | Select an identifier advertised by the CLI model catalog. |
| `DICTIONARY_SKILL_PATH` | Absolute path to the selected `SKILL.md`; useful if different copies are installed. |
| `DICTIONARY_DATA_DIR` | Alternate directory for history and the isolated CLI working directory. |

The default database is `~/Library/Application Support/Dictionary/history.sqlite3`. It contains original inputs, Markdown entries, outcomes, immutable request configuration, skill snapshots, reported usage, and timings. The recent list shows the latest 200 attempts; cached searches cover the entire database and reveal older matching entries in the sidebar. Schema version 2 adds an indexed cache key and preserves existing attempts. A file lock prevents competing app instances from using the same history. Pending requests left by a previous process become interrupted on startup.

The prompt reads the selected skill file for every lookup and records its SHA-256 and exact contents. Byte-identical installed copies are collapsed. The input travels as JSON-encoded text through stdin, never through shell interpolation. Each request has fresh context and uses a dedicated CLI process in a read-only sandbox with `--ephemeral` and `--no-daemon`.

New lookups use the service configured in the CLI. Saved entries remain local and readable when setup or authentication is unavailable. The app does not store authentication credentials.

## Measurements and failure handling

- Total tokens are reported input plus output. Cached input and reasoning output are subset breakdowns; they are never added again.
- Missing counters display as unavailable, rather than zero. Reported counters and partial answers survive later request failures.
- Response time runs from accepted submission through the first durable save of the outcome. A subsequent metadata write stores that measurement. Queue wait, CLI runtime, and time to the first observable answer are stored separately. Total response time and first-answer time include queue wait; the three-minute execution timeout starts only when the CLI starts.
- The tested CLI emits a completed answer item rather than a token stream. The app shows progress until answer text arrives and also accepts item snapshots when available.
- Success requires a fully delivered prompt, completed usable answer, terminal success event, and exit code zero. Login errors, process failures, malformed/incomplete output, cancellation, and the three-minute timeout have distinct outcomes.
- Quitting cancels every running and queued attempt, reaps owned processes, and saves each outcome. Queued cancellations make no model request and have no reported token usage.
- Persistence errors keep the result visible with an explicit saving warning so it can be copied. Requests are never silently retried.

## Development

Set up the Rust formatting and lint checks once per checkout:

```sh
./scripts/setup.sh
```

The setup script installs `rustfmt` and `clippy` for the pinned toolchain in `rust-toolchain.toml` and sets this repository's Git hooks path to `.githooks`. The pre-commit hook runs `cargo fmt --all -- --check` and `cargo clippy --locked --all-targets --all-features -- -D warnings`. Either failure blocks the commit. Checks use the working tree without modifying or staging files; fix formatting with `cargo fmt --all`.

Run the same checks manually with `./.githooks/pre-commit`.

The UI is a thin GPUI layer over a UI-independent library:

| Module | Responsibility |
| --- | --- |
| `discovery` | Executable, model, effort, and local skill discovery using a short-lived metadata session. |
| `model` | Request snapshots, input validation, prompt composition, and measurements. |
| `protocol` | Incremental JSONL decoding and idempotent answer/usage reconciliation. |
| `runner` | Direct subprocess execution, pipe draining, cancellation, deadlines, and cleanup. |
| `store` | Versioned SQLite schema and serialized database work on a dedicated thread. |
| `service` | Per-key admission, three concurrent CLI tasks, FIFO queue, targeted cancellation, and shutdown. |
| `view_state` | Per-attempt live state and navigation IDs; background work cannot change the selected result. |
| `ui` | Native input, Markdown result, recent history, and status/usage display. |

```sh
cargo fmt --all -- --check
cargo test --locked --no-default-features
cargo clippy --locked --all-targets --all-features -- -D warnings
cargo build --locked
```

Core tests use an observed CLI fixture and controllable local executables; they make no model requests. They cover split UTF-8/JSONL, repeated events, unknown/missing usage, literal input, durable history, failure after output, authentication errors, cancellation, timeouts, concurrent result isolation, queue advancement, active-key deduplication, shutdown, and stale navigation events. See [validation notes](docs/phase-1-validation.md).

A headless example exercises the same coordinator and persistence path. It uses a cached result when available; otherwise it makes a real request using your CLI account:

```sh
DICTIONARY_DATA_DIR=/tmp/dictionary-example \
  cargo run --locked --no-default-features --example lookup -- 'break the ice'
```

For manual UI testing with isolated history:

```sh
python3 scripts/bundle-dev.py --data-dir /tmp/dictionary-ui-test
```

Use a distinct `--name` for each test profile, because macOS can cache a bundle's launch environment. The [synthetic CLI fixture](tests/fixtures/README.md) supports checking streaming and failure states without a model request.

If a personal Cargo configuration redirects crates.io to an unavailable mirror, use a separate Cargo home and run Cargo from outside that configuration's ancestor directories with `--manifest-path /absolute/path/to/dictionary/Cargo.toml`. This avoids changing global configuration. Keep `--locked` and the pinned toolchain when doing so.
