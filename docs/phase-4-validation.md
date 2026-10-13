# Phase 4 validation

Validated on macOS with Rust 1.99.0 and GPUI Kit 0.7.2 on 2026-10-13.

## Delivered behavior

- The chart icon beside Settings and Command+Shift+A open Analysis. The dictionary layout and compact History heading remain intact. Analysis shows summaries and charts; History remains the place to browse saved attempts.
- Seven-day, thirty-day, and custom inclusive date ranges use the system local timezone, with its IANA name displayed. Each local midnight is resolved independently, including ambiguous or skipped midnights and skipped dates. An unfinished day is marked with an asterisk. Requests beyond the snapshot time are excluded; custom ranges are bounded to ten years.
- Shared requested-model, effort, direction, and outcome filters apply to all charts and summaries. The dashboard includes daily reported tokens, lookup/attempt frequency, daily median latency, latency distribution, outcome counts, first-answer/queue/CLI timing medians, cached-input share, and repeated new submissions.
- Input and output counters are summed as reported subtotals. Cached input and reasoning output never enter that sum a second time. Complete-total coverage and per-counter sample counts disclose missing reports. Known zero and unavailable values remain distinct. Latency uses successful terminal attempts by default, or the explicitly selected outcome, with sample counts and nearest-rank p95.
- Large token counts use shared compact formatting in Analysis, chart axes, History, and the entry toolbar. Exact counts retain integer precision and thousands separators in hover details, accessibility labels, and entry details. Rounding carries into the next unit and removes trailing zeroes.
- The first version of a lookup counts as a new submission; regenerated attempts count separately. A regeneration inside a range cannot turn an original lookup outside it into a new submission. Repeat rate uses normalized input plus resolved direction among new submissions; reading cached entries has no effect.
- GPUI Kit bar charts provide hover details. Daily missing measurements remain gaps, and a wholly unavailable series has an explicit empty state. The most-submitted-inputs ranking is informational. Charts and rankings have no navigation or attempt-selection controls.
- SQL projection and aggregation run on the existing storage worker, excluding answer and skill contents from returned data. Reports contain aggregates plus saved counters for active attempts, so the UI can refresh on usage changes without retaining every attempt. Reports are shared by reference with the UI. Request IDs reject stale filter responses and errors; invalidations during loading trigger a fresh read. No migration or redundant aggregate cache is needed at the measured dataset size.

## Automated checks

`cargo test --locked --no-default-features` passes **80 tests**. The existing crash-test subprocess helper remains ignored by the normal runner and is explicitly invoked by its parent tests. No model requests are made.

Rust formatting, all-target/all-feature Clippy with warnings denied, and `cargo build --locked` also pass. Cargo still reports the existing upstream `block` crate future-incompatibility warning.

New behavioral coverage includes:

- Partial, missing, and zero counters; subset semantics; repeated checkpoint replacement; weighted rather than averaged cached-input share.
- Inclusive local date ranges with fractional UTC instants, 23-hour and 25-hour days, repeated clock hours, and a skipped calendar date.
- Empty ranges, invalid dates, unfinished days, unknown durations, median calculation, nearest-rank p95, and outliers.
- Combined filters, original requests outside the range, regenerated versions, cached/history reads, and cascading deletion. Daily totals, latency buckets, and outcome counts reconcile with their measured populations.
- Repeat-rate grouping across canonical Unicode equivalence and opposite translation directions.
- Stale-response/error rejection and invalidation while a query is running.
- A service integration test performs analysis during a running fixture lookup, observes cancellation and deletion in subsequent reports, and verifies that the process-start count is unchanged.

## Performance

The deterministic test creates **10,000 attempts across 5,000 lookups**, including approximately 4.8 KiB of saved answer text per attempt. The final debug-build run measured approximately **150 ms** for the full range and **57 ms** for a model filter. These are local observations, not a cross-machine timing guarantee. The test prints timings and asserts aggregate and filter correctness rather than a fragile wall-clock threshold.

Reproduce the measurement with:

```sh
cargo test --locked --no-default-features --test analysis ten_thousand_attempts -- --nocapture
```

## Native review

An isolated development bundle used a synthetic 10,000-attempt profile and `/usr/bin/false` as the CLI. No personal history or model service was used. Native screenshots and interactions verified dashboard rendering, model filtering, and lookup/attempt frequency switching. Seven-day selection updated the shared charts; choosing a custom future range in the calendar displayed the empty state. The database retained 10,000 attempts and 5,000 lookups after these read-only interactions.

The page was also resized to approximately 900 points wide with the sidebar visible. Filters wrap and summary cards remain readable. Native inspection caught unconstrained Select wrappers; explicit control bounds now keep them within the filter row. Broader theme, assistive-technology, and release acceptance remain Phase 5.

The most-submitted-inputs list was reviewed separately with 21 synthetic lookups. Ranked rows keep the input and direction on the left and counts on the right. Long sentences truncate at approximately 900 points wide with the sidebar visible, and Chinese text and singular counts render correctly. The rows are static list items, with no click handlers, button focus, hover highlighting, or navigation chevrons. Native verification confirmed that clicking a row leaves the page and scroll position unchanged. All eight analysis tests, build, formatting, and Clippy passed after removing input selection.

The simplified dashboard was reviewed with a fresh 21-lookup profile. Native inspection confirmed that only date/configuration filters, refresh, and the frequency-series toggle remain as dashboard controls. Filtering to Chinese → English updated the summary and charts from 21 to 5 attempts and from 14,700 to 3,500 tokens; the input ranking showed the two matching inputs. The page ends with the ranking and usage details. Opening a saved entry through History and returning with Back to History still works. The final full 80-test suite, build, formatting, and Clippy all passed.

A separate four-entry fixture verified large-token formatting: the dashboard displays **1.25M** with an exact **1,246,912 tokens** tooltip, History and the entry toolbar display **1.23M** for a 1,234,567-token entry, and entry details show comma-grouped totals and breakdowns. Missing usage remains **—** and known zero remains **0**. A standalone formatter preview checked unit transitions, rounding carry, and the full `u64`/`u128` range. Build, formatting, and Clippy passed after the display changes.
