# Phase 2 validation

Validated on macOS with Rust 1.99.0, GPUI Kit 0.7.2, and Codex CLI 0.162.1 on 2026-10-12.

## Delivered behavior

- Auto direction displays its resolved target beside the input. Mixed scripts, numbers alone, and unsupported scripts require an explicit English → Chinese or Chinese → English choice. Original input remains unchanged.
- Cache matching and active-request deduplication use normalized input **and resolved direction**. Regeneration retains the original input and direction while capturing current model, effort, and skill contents.
- Settings opens from the sidebar gear or Command+comma. It provides the advertised model catalog, supported Low/Med/High/Max efforts, discovered skill sources, a native file picker, catalog refresh, and CLI diagnostics.
- Med sends `medium`. Max sends literal `max` and is disabled for models advertising only `xhigh`. A model switch that invalidates the effort requires an explicit supported selection.
- Fast mode uses advertised `serviceTiers` (with the older `additionalSpeedTiers` fallback), preserves effort, and saves a requested Fast/Standard tier per new attempt. On sends `service_tier="fast"` with `features.fast_mode=true`; off sends `service_tier="default"`. Older records without a captured tier remain unknown. Save results appear in toasts rather than inline modal messages.
- Preferences and catalog metadata survive restart. The catalog records retrieval time and CLI version; failed refreshes retain it with a cached label and diagnostic. Skill-discovery errors leave the model catalog and Browse fallback available.
- SQLite schema 3 preserves earlier attempt payloads and their English → Chinese direction. New attempts record both requested and resolved direction, immutable settings, skill SHA-256, and direction-prompt version. English composition is version `en-zh-v2`; reverse composition is `zh-en-v1`.

## Automated checks

`cargo test --locked --no-default-features` passes **60 tests** and covers the existing lifecycle and protocol suite plus direction detection, literal prompt input, reverse prompt precedence, catalog effort and Fast-mode validation, backward-compatible preferences and tier records, initial defaults, skill-source deduplication, settings persistence, immutable old records, new skill snapshots, and direction-scoped cache/regeneration.

Service tests start real local fixture processes. Changing settings with three running requests and another queued request leaves their original arguments intact; a later admitted request uses the new effort and Fast tier. The test inspects actual CLI arguments, completes the queue, restarts the service immediately, and checks the saved preferences and prior attempts. Unsupported settings are rejected before replacing saved preferences.

The immediate-restart check exposed a storage-thread cleanup race. The store now joins its worker when the last owner drops, completing pending writes and releasing the database lock before restart returns.

Formatting, all-target/all-feature Clippy with warnings denied, and a native desktop build are required alongside these tests. Tests make no model requests.

## Native checks

An isolated fixture-backed macOS app was used to inspect the default layout and Settings modal. Checks included:

- Model selection, disabled unsupported effort levels, explicit High selection, and saving to SQLite.
- Fast-mode on/off saves, disabled switch for a model without Fast support, and enabled state restored after restarting the native app.
- Success toast, failure toast triggered by an empty isolated skill file, preserved draft and prior saved preferences after failure, and a successful retry replacing the error toast. The modal keeps its layout and has no inline save-result text.
- A deliberately long fixture error reproduced toast text being covered by Settings. An app-owned notification list now paints at deferred priority above dialogs and menus; the same overlapping toast is fully visible and dismissible while Settings remains open.
- Cancel, the close icon, and Escape show **Discard changes?** with **Keep editing** / **Discard** only for unsaved values. Native checks cover unchanged settings, reverted edits, keeping the draft, discarding without altering saved preferences, closing after a successful save, and retaining the warning after a failed save. Escape from the confirmation returns to editing.
- Auto Chinese detection, Enter submission, rejection of ambiguous mixed input, explicit Chinese → English selection, and a second concurrent lookup.
- Target-language sidebar previews, per-entry real activity, and collapsed activity after completion.
- Native Browse selection of a different SKILL.md path and persistence of that preference.
- Restarting the native app restores the selected model, effort, and relocated skill. Both saved attempt payloads remained byte-for-byte unchanged; opening real sample history left the request count at three.

GPUI Kit's select component uses a full-size outer element; fixed-width/height wrappers keep it from consuming the input row. UI events use the captured window context so initial metadata can arrive before the view's first paint.

## Real bilingual output review

Three independent requests ran through the same service in an isolated profile, using `gpt-6-astra` at Low effort and the installed `dict` skill. All succeeded with saved usage and timings:

| Input | Observed result | Total tokens | Response time |
| --- | --- | ---: | ---: |
| 从容 | “calm and composed,” with distinctions from “unhurried” and “with ease,” pronunciation, Chinese explanations, and four bilingual examples | 18,804 | 32.0 s |
| 顺其自然 | “Let things take their course,” with register/context differences from “go with the flow” and “let things happen naturally,” plus bilingual examples | 19,643 | 33.3 s |
| 如果你方便的话，我们可以把会议改到明天下午。 | “If that works for you, we could move the meeting to tomorrow afternoon,” with Chinese explanations of politeness, grammar, and two bilingual variations | 18,593 | 25.7 s |

The implementation agent reviewed these samples for natural equivalents, preserved meaning, Chinese explanations, and useful examples. Generated prose is not asserted byte-for-byte. These are observations for this run, not latency or quality guarantees; user language-quality review remains useful.

Model/effort discovery follows the official [app-server model/list contract](https://learn.chatgpt.com/docs/app-server#list-models-modellist). The advertised catalog supplies picker options; a successful request establishes access for that request.

Fast-mode wiring follows the official [speed settings](https://learn.chatgpt.com/docs/agent-configuration/speed#fast-mode) and [CLI 0.162.1 configuration implementation](https://github.com/openai/codex/blob/rust-v0.162.1/codex-rs/core/src/config/mod.rs). Live metadata on the installed CLI advertises the Fast tier with request ID `priority`. Fast-mode argument tests use local fixtures and do not measure actual service-tier latency.

## Remaining phases

Searchable/paginated history belongs to Phase 3; analysis charts to Phase 4; distribution, appearance settings, and broader macOS release validation to Phase 5. CLI relocation remains available through `DICTIONARY_CLI`; Settings displays the resolved executable. Model and skill environment overrides seed first-launch preferences, while persisted app settings govern later launches.
