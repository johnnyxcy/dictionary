# Phase 3 validation

Validated on macOS with Rust 1.99.0 and GPUI Kit 0.7.2 on 2026-10-13.

## Delivered behavior

- A compact History heading and ellipsis icon button above recent sidebar entries open the full History page. The icon has a hover highlight and a Show all history tooltip. Command+F also opens it; the page's back arrow or Command+L returns to the current lookup and input. Opening a saved row shows that attempt, with Back to History retaining the query, filters, page, and scroll position.
- Literal substring search across all saved inputs, with NFC normalization, case-insensitive English matching, and direction/outcome filters. Search does not inspect generated prose or alter the stricter lookup-cache key.
- Twenty-five attempts per page, ordered by creation time and attempt ID. Cursors retain those immutable values, so deleting an anchor or adding a newer attempt does not shift the next page. Previous-page cursors remain in view state. If deletion empties an older page, the view returns to the nearest populated page.
- Per-row translation/outcome previews, regenerated version numbers, requested model/effort, recorded Fast tier, tokens, response time, and local date. Counts distinguish attempts from distinct lookups. Missing measurements remain unavailable.
- The existing all-versions delete action is available on hover, except when any version is active, including versions outside the visible page/filter. Opening history or searching while another lookup runs adds no attempt or token usage and does not cancel background work.
- SQLite schema 4 adds ordering/outcome indexes. Queries execute on the existing storage worker and return one page of display metadata. Earlier settings, skill snapshots, and attempt payloads are preserved.
- Live snapshots now include available timing, thread/model metadata, activity, text, and reported usage. The service saves a checkpoint before publishing progress. Usage-only protocol updates also produce checkpoints. Graceful quit retains cancelled outcomes; restart after an unexpected exit marks unfinished attempts interrupted and preserves their last saved checkpoint without replaying inference.
- Entry details expose CLI runtime and first-answer time alongside response time and queue wait, with bounded scrolling for the expanded metadata.

## Automated checks

`cargo test --locked --no-default-features` passes **71 tests**. The recovery subprocess entry point is ignored by the normal runner and invoked explicitly by the two crash tests. No model requests are made.

The new history suite covers:

- English case and canonical Unicode equivalence, Chinese substrings, literal SQL wildcard/quote characters, and entries beyond the 200-row recent list.
- Complete, duplicate-free pagination with tied timestamps, insertion above the current page, and deletion of the cursor row, without depending on hard-coded row IDs or page boundaries.
- Combined filters, version numbers, immutable settings, unknown usage, subset token semantics, and deletion counts agreeing with persisted aggregates.
- Deletion protection when an active sibling version is hidden by filters.
- Schema-3 migration preserving immutable attempt payloads and loadable preferences across reopen.
- Stale query/error rejection, failed-page retry, and filter/cursor preservation using real database pages. Deletion during an outstanding query forces a fresh read and returns an emptied page to its previous cursor.

A coordinator integration test searches and opens saved history while another fixture process runs. It verifies unchanged request counts, saved output and usage, and cancellation of only the intended active attempt. Existing tests continue to cover timeout, process failure, shutdown, queues, and regeneration.

Two crash tests start a separate service process and kill it after a known checkpoint: once during partial output and once after reported usage but before CLI exit. Each restarts against the same database, opens the recovered attempt, and verifies preserved identifiers, text, activity, skill, start/first-answer metadata, and nullable or reported usage. The row becomes interrupted, its unmeasured final duration remains unknown, and the fixture start count stays at one. Because SIGKILL bypasses application cleanup, the harness explicitly terminates only its recorded fixture process groups; this does not establish that abruptly killing the app terminates external descendants.

Formatting, all-target/all-feature Clippy with warnings denied, and `cargo build --locked` pass. The upstream `block` crate emits Cargo's existing future-incompatibility warning.

## Native inspection limit

An isolated profile with 302 synthetic attempts, multiple versions, both directions, and all terminal outcomes was prepared. A native development bundle launched and reached GPUI's successful window-open path. The computer-use tool returned `cgWindowNotFound`; the screenshot fallback reported missing macOS Screen Recording permission. No visual or click-through pass is claimed for this phase. The remaining native review is search/filter layout, opening and returning to an older page, keyboard navigation, narrow-window scrolling, and activity/deletion behavior during concurrent work. Personal history was not used or changed.

## Remaining phases

Analysis charts and aggregate exploration remain Phase 4. Broader macOS appearance, accessibility, packaging, and release acceptance remain Phase 5.
