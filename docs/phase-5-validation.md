# Phase 5 validation

Validated on 2026-10-13 with macOS 26.6.2, Rust 1.99.0, and GPUI Kit 0.7.2. This phase targets a locally built macOS application on the host architecture, with a minimum OS declaration of macOS 15. It does not provide public notarized distribution or a universal binary.

## Delivered behavior

- `python3 scripts/bundle.py` builds the optimized application, generates the icon and bundle metadata, checks runtime library dependencies, and applies and verifies an ad-hoc signature. `scripts/bundle-dev.py` uses the same builder with the development profile. PR checks now validate packaging after building.
- Settings saves an absolute CLI location and reconnects discovery after it changes. Existing running/queued attempts retain their executable and generation settings. Empty location enables automatic discovery. Launch commands add the executable and symlink-target directories to PATH so adjacent runtimes work without shell initialization.
- System, Light, and Dark appearance persist independently of generation settings. Appearance-only saves work while a CLI or skill is unavailable. Save and Cancel stay visible in the Settings footer while the content scrolls.
- Native menus expose navigation and editing, including Command+N for New Lookup. Sidebar entries support Tab focus, a visible focus ring, and Enter/Space activation. Navigation and lookup submission are guarded while a modal is open.
- Application, File, Edit, View, and Window menus are registered together with a Dock menu. The app menu includes About, Settings, Services, Hide/Hide Others/Show All, and Quit. Dock shortcuts raise the existing window and route to New Lookup, History, Analysis, or Settings without replacing an active modal. Launch Services reopen events also raise the window. macOS supplies its standard full-screen and Dock management entries.
- Saved Markdown has a native accessibility label containing the entry text. Analysis has a Data/Charts switch: text rows use the same aggregate values, dates, coverage, and sample counts as the charts, without adding attempt navigation.
- Startup failures display a diagnostic window with Copy details, Quit, and Open data folder where available. Existing schemas receive a SQLite snapshot backup before upgrade; all pending migrations commit together or roll back. Newer unsupported schemas are rejected before changing journal mode.

## Automated checks

- `cargo test --locked --no-default-features`: **86 tests pass** without model requests. Two ignored tests are subprocess entry points invoked by their parent tests, not skipped acceptance cases.
- `./.githooks/pre-commit`: formatting and all-target/all-feature Clippy with warnings denied pass.
- `cargo check --locked --all-targets --all-features`: passes.
- `python3 scripts/bundle.py`: optimized native build, property-list validation, dependency inspection, and strict signature verification pass.

The existing upstream future-compatibility notice for `block 0.1.6` remains; application code passes the pinned toolchain's lint checks.

New integration cases exercise behavior rather than UI wording:

| Case | Evidence |
| --- | --- |
| Launch without a terminal environment | A separate process starts from `/` with PATH `/usr/bin:/bin` and no CLI/model/skill overrides. Saved preferences point to a symlink outside PATH, whose target uses an adjacent runtime under a path containing spaces. Catalog discovery and a multiline IPA lookup succeed; result, usage, original symlink path, and saved appearance survive reopening. |
| Relocate the CLI | Save a second executable while a request runs. The original attempt keeps its executable; discovery reconnects and a newly admitted attempt uses the new one. Relative paths fail without replacing saved preferences. |
| Offline appearance | Changing appearance with an unavailable skill succeeds without altering model, effort, or skill selection. |
| Late migration failure | Inject a conflict in migration 4 starting from schema 1 with committed WAL data. The entire upgrade rolls back to schema 1, including earlier cache changes. The backup contains the original committed input and passes SQLite integrity checks. Removing the conflict permits a later upgrade. |
| Existing preferences | Schema 3 preferences retain model, effort, Fast mode, and skill; added fields default to automatic CLI discovery and System appearance. |
| Future database | Opening an unsupported schema fails and leaves the database bytes unchanged. |

Existing tests also cover process descendants, cancellation, shutdown, crash recovery, all-version deletion, cached reads, reverse-direction prompts, and analysis accounting. No new model calls were made for this phase; real authenticated CLI and bilingual-output evidence is in [Phase 1](phase-1-validation.md) and [Phase 2](phase-2-validation.md).

## Native acceptance

Launched separate development and optimized release bundles through macOS Launch Services with an isolated SQLite profile. The final release test bundle had only a data-directory override: its CLI executable came from saved preferences. Synthetic fixtures supplied predictable activity, results, and usage; they never contacted a model.

- Recovered from a missing configured CLI through Settings. Saving the fixture executable populated the catalog and preserved appearance. Changed model and effort, observed unsupported choices disabled, and restarted with the saved choices intact.
- Submitted multiline English containing IPA and multiline Chinese using explicit directions. Shift+Enter added a line break without a request; Enter submitted. Real fixture activity appeared while running, partial output rendered, and activity collapsed after completion. These checks exercise submission and rendering; fixture prose is not evidence of translation quality.
- Copied the full Markdown entry and a selected Chinese passage, then pasted into the input to verify Unicode preservation. Native accessibility inspection exposed the entry's full Markdown text.
- Inspected Light and Dark appearance, reduced the window to its 900 × 620 minimum, scrolled Settings, and verified that its footer remained available. History's wide metadata rows remained horizontally scrollable.
- Saved valid settings and triggered an invalid relative CLI path. Success/error toasts appeared above the modal, errors retained the draft, and the saved executable stayed unchanged. Enter inside Settings did not submit a background lookup. Cancel/Escape offered Keep editing and Discard for unsaved changes.
- Used Command+N and Command+L, tabbed to a sidebar entry, observed its focus ring, and opened it with Enter without creating a request.
- Opened Analysis and its seven-day Data view. Native accessibility rows exposed exact counts, latency values, and coverage; missing measurements stayed distinct from zero. The input ranking remained informational.
- Canceled a running fixture lookup, reopened its partial result in History, then removed that disposable lookup with the sidebar's hover trash control.
- Opened a second bundle against the same profile. It showed startup lock diagnostics and recovery controls instead of resetting history or silently exiting.
- Closed the optimized app's window with **three running and one queued** request. All four saved as Canceled. All three recorded child PIDs were gone; the queued request had no process, start time, CLI duration, or reported usage. Partial results were retained, SQLite integrity was `ok`, and reopening showed all four canceled entries without resubmission.

### Native menu follow-up

Inspected the registered macOS menus in a separate optimized fixture bundle. About opened with the package version, View → Analysis changed the page, and Edit → Undo removed the pasted input. Window → Minimize removed keyboard focus; Window → Show Dictionary restored the window and accepted keyboard input again. File → Close Window during a running lookup quit the app, saved Canceled with its partial result, reaped the fixture PID, and left SQLite integrity `ok`.

The first native pass exposed reentrant window access in global menu handlers. Window work is now deferred until the action's current window update finishes. Dock commands have distinct global actions, then forward to existing view actions; they cannot recursively forward themselves when no normal view handler exists. No navigation action submits a lookup.

The automation connector could not attach to macOS Dock itself, so the Dock context menu's direct click path remains a manual UI check. Its registration compiles against GPUI's native Dock API; the shared window-activation path was exercised through Window → Show Dictionary (now Show JDict). Verify the Dock shortcuts while JDict is hidden/minimized and with an unsaved Settings draft.

## Remaining manual checks

The release implementation and automated checks are complete, but the input/accessibility acceptance is not fully signed off:

1. **Chinese IME composition:** activate Simplified Chinese Pinyin, type a candidate sequence, and confirm a candidate with Enter. Confirm this commits text without submitting a lookup; a subsequent Enter should submit once. Repeat with a multiline input and Shift+Enter. The existing marked-text guard remains, but the automation session could only verify Unicode paste, not an active candidate window.
2. **VoiceOver:** read a saved entry, navigate Settings and History, and read Analysis in Data mode. Accessible names and roles were inspected in the native tree; end-to-end spoken reading and focus order still need a VoiceOver pass.

System appearance was initialized from macOS and explicit Light/Dark choices were exercised; changing the host's system theme while the app follows System was not part of this isolated test. macOS 15 and additional architectures require their own runtime acceptance if distribution expands.

To repeat native tests without touching personal history:

```sh
python3 scripts/bundle.py --name 'JDict Release QA' \
  --data-dir /tmp/dictionary-release-qa \
  --cli "$PWD/tests/fixtures/activity-cli.py"
```

Launch that named app from `target`. Use a new name for another profile because Launch Services can cache bundle environments. Choose the fixture skill in Settings when needed. Remove `--cli` and select a saved executable to test Finder-style connection recovery; a normal release bundle has no development environment overrides.
