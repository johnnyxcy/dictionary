use std::collections::HashSet;

use dictionary::{
    direction::Direction,
    history::{HistoryFilter, HistoryPage, HistoryQuery, HistoryState, PAGE_SIZE},
    model::{Attempt, AttemptStatus, ServiceTier, Setup, SkillSnapshot, Usage},
    settings::Preferences,
    store::Database,
};

struct Fixture {
    db: Database,
    setup: Setup,
    skill: SkillSnapshot,
    root: tempfile::TempDir,
}

impl Fixture {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("SKILL.md");
        std::fs::write(&path, "Dictionary fixture").unwrap();
        let setup = Setup {
            executable: "/unused-cli".into(),
            cli_version: "fixture".into(),
            model: "fixture-model".into(),
            effort: "low".into(),
            service_tier: Some(ServiceTier::Standard),
            skill_path: path,
            skill_name: "dict".into(),
        };
        let skill = SkillSnapshot::read(&setup).unwrap();
        Self {
            db: Database::open(&root.path().join("history.sqlite3")).unwrap(),
            setup,
            skill,
            root,
        }
    }

    fn insert(&mut self, input: &str, direction: Direction, status: AttemptStatus) -> Attempt {
        let mut attempt = Attempt::new(input.into(), self.setup.clone(), self.skill.hash.clone());
        attempt.direction = direction;
        self.db.insert(&attempt, &self.skill).unwrap();
        attempt.status = status;
        attempt.answer = format!(
            "# {input}\n\n{}",
            if direction == Direction::ZhEn {
                "Natural English equivalent"
            } else {
                "中文释义"
            }
        );
        self.db.save(&attempt).unwrap();
        attempt
    }

    fn search(&self, search: &str) -> HistoryPage {
        self.db
            .history_page(&HistoryQuery {
                filter: HistoryFilter {
                    search: search.into(),
                    ..Default::default()
                },
                ..Default::default()
            })
            .unwrap()
    }

    fn load(&self, state: &mut HistoryState) {
        let (request_id, query) = state.request();
        state.receive(request_id, self.db.history_page(&query).unwrap());
    }
}

fn row_ids(page: &HistoryPage) -> HashSet<String> {
    page.rows.iter().map(|row| row.entry.id.clone()).collect()
}

#[test]
fn substring_search_is_literal_unicode_normalized_and_covers_all_saved_inputs() {
    let mut f = Fixture::new();
    let old = f.insert(
        "A café by the sea",
        Direction::EnZh,
        AttemptStatus::Succeeded,
    );
    for i in 0..210 {
        f.insert(
            &format!("recent word {i}"),
            Direction::EnZh,
            AttemptStatus::Succeeded,
        );
    }
    let chinese = f.insert(
        "不慌不忙，从容应对。",
        Direction::ZhEn,
        AttemptStatus::Succeeded,
    );
    let literal = f.insert(
        "100%_literal\\ 'quoted'",
        Direction::EnZh,
        AttemptStatus::Succeeded,
    );
    f.insert(
        "100 percent literal",
        Direction::EnZh,
        AttemptStatus::Succeeded,
    );
    assert!(!f.db.history().unwrap().iter().any(|row| row.id == old.id));
    assert_eq!(
        row_ids(&f.search("  CAFE\u{301} ")),
        HashSet::from([old.id])
    );
    assert_eq!(row_ids(&f.search("从容")), HashSet::from([chinese.id]));
    assert_eq!(
        row_ids(&f.search("%_literal\\ 'quoted'")),
        HashSet::from([literal.id])
    );
    assert!(f.search("' OR 1=1 --").rows.is_empty());
    assert!(
        f.search("中文释义").rows.is_empty(),
        "search targets original inputs, not generated prose"
    );
}

#[test]
fn pagination_is_deterministic_with_tied_timestamps_new_entries_and_deleted_cursors() {
    let mut f = Fixture::new();
    let mut expected = HashSet::new();
    for i in 0..PAGE_SIZE * 2 + 3 {
        let mut attempt = Attempt::new(format!("entry {i}"), f.setup.clone(), f.skill.hash.clone());
        attempt.created_at = "2026-10-12T09:00:00Z".into();
        f.db.insert(&attempt, &f.skill).unwrap();
        attempt.status = AttemptStatus::Succeeded;
        attempt.answer = "中文释义".into();
        f.db.save(&attempt).unwrap();
        expected.insert(attempt.id);
    }
    let first = f.db.history_page(&HistoryQuery::default()).unwrap();
    assert!(first.rows.len() <= PAGE_SIZE);
    assert!(first.next.is_some(), "remaining entries must be reachable");
    let mut seen = row_ids(&first);
    let mut newer = Attempt::new(
        "newly arrived".into(),
        f.setup.clone(),
        f.skill.hash.clone(),
    );
    newer.created_at = "2026-10-12T09:01:00Z".into();
    f.db.insert(&newer, &f.skill).unwrap();
    newer.status = AttemptStatus::Succeeded;
    f.db.save(&newer).unwrap();
    f.db.delete_lookup(&first.rows.last().unwrap().entry.lookup_id)
        .unwrap();
    let mut next = first.next;
    while let Some(cursor) = next {
        let page =
            f.db.history_page(&HistoryQuery {
                before: Some(cursor),
                ..Default::default()
            })
            .unwrap();
        assert!(page.rows.len() <= PAGE_SIZE);
        for row in &page.rows {
            assert!(
                seen.insert(row.entry.id.clone()),
                "pagination repeated an attempt"
            );
        }
        next = page.next;
    }
    assert_eq!(
        seen, expected,
        "continuing the original page must neither skip old attempts nor include the newly inserted one"
    );
}

#[test]
fn filters_versions_measurements_and_deletion_share_the_same_records() {
    let mut f = Fixture::new();
    let mut original = f.insert("shared input", Direction::EnZh, AttemptStatus::Succeeded);
    original.usage = Usage {
        input_tokens: Some(100),
        cached_input_tokens: Some(60),
        output_tokens: Some(20),
        reasoning_output_tokens: Some(5),
    };
    original.duration_ms = Some(1250);
    f.db.save(&original).unwrap();
    let mut retry = Attempt::new(
        original.input.clone(),
        f.setup.clone(),
        f.skill.hash.clone(),
    );
    retry.lookup_id = original.lookup_id.clone();
    retry.setup.model = "second-model".into();
    retry.setup.effort = "high".into();
    retry.setup.service_tier = Some(ServiceTier::Fast);
    f.db.insert(&retry, &f.skill).unwrap();
    retry.status = AttemptStatus::Failed;
    f.db.save(&retry).unwrap();
    let mut reverse = f.insert("shared input", Direction::ZhEn, AttemptStatus::Succeeded);
    reverse.usage = Usage {
        input_tokens: Some(40),
        output_tokens: Some(5),
        ..Default::default()
    };
    f.db.save(&reverse).unwrap();
    let filter = HistoryFilter {
        search: "SHARED".into(),
        direction: Some(Direction::EnZh),
        status: Some(AttemptStatus::Failed),
    };
    let page =
        f.db.history_page(&HistoryQuery {
            filter,
            before: None,
        })
        .unwrap();
    let row = &page.rows[0];
    assert_eq!((page.attempts, page.lookups), (1, 1));
    assert_eq!(row.entry.id, retry.id);
    assert_eq!(row.model, retry.setup.model);
    assert_eq!(row.effort, retry.setup.effort);
    assert_eq!(row.tier, retry.setup.service_tier);
    assert_eq!(row.usage.total(), None);
    let all = f.search("shared");
    assert_eq!((all.attempts, all.lookups), (3, 2));
    let original_row = all.rows.iter().find(|r| r.entry.id == original.id).unwrap();
    assert_eq!(row.version, original_row.version + 1);
    assert_eq!(original_row.usage, original.usage);
    assert_eq!(original_row.duration_ms, original.duration_ms);
    f.db.delete_lookup(&original.lookup_id).unwrap();
    let retained = f.search("shared");
    assert_eq!((retained.attempts, retained.lookups), (1, 1));
    assert_eq!(retained.rows[0].entry.id, reverse.id);
    let connection = rusqlite::Connection::open(f.root.path().join("history.sqlite3")).unwrap();
    let totals: (u64, Option<u64>) = connection
        .query_row(
            "SELECT COUNT(*), SUM(input_tokens + output_tokens) FROM attempts",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(totals, (retained.attempts, reverse.usage.total()));
    assert!(f.db.get(&original.id).is_err());
    assert!(f.db.get(&retry.id).is_err());
}

#[test]
fn deletion_is_disabled_for_an_active_version_hidden_by_filters() {
    let mut f = Fixture::new();
    let old = f.insert("hello", Direction::EnZh, AttemptStatus::Succeeded);
    let mut queued = Attempt::new(old.input.clone(), f.setup.clone(), f.skill.hash.clone());
    queued.lookup_id = old.lookup_id.clone();
    f.db.insert(&queued, &f.skill).unwrap();
    let query = HistoryQuery {
        filter: HistoryFilter {
            status: Some(AttemptStatus::Succeeded),
            ..Default::default()
        },
        ..Default::default()
    };
    let rows = f.db.history_page(&query).unwrap().rows;
    assert_eq!(rows.len(), 1);
    assert!(rows[0].lookup_active);
    assert!(f.db.delete_lookup(&old.lookup_id).is_err());
    queued.status = AttemptStatus::Cancelled;
    f.db.save(&queued).unwrap();
    assert!(!f.db.history_page(&query).unwrap().rows[0].lookup_active);
}

#[test]
fn migration_from_phase_two_preserves_attempt_payloads_and_settings() {
    let f = Fixture::new();
    let path = f.root.path().join("legacy.sqlite3");
    let mut attempt = Attempt::new("older result".into(), f.setup.clone(), f.skill.hash.clone());
    attempt.status = AttemptStatus::Succeeded;
    attempt.answer = "中文释义".into();
    let payload = serde_json::to_string(&attempt).unwrap();
    let preferences = Preferences {
        version: 1,
        model: f.setup.model.clone(),
        effort: f.setup.effort.clone(),
        fast_mode: true,
        skill_path: f.setup.skill_path.clone(),
    };
    {
        let db = rusqlite::Connection::open(&path).unwrap();
        db.execute_batch(include_str!("../migrations/001_initial.sql"))
            .unwrap();
        db.execute_batch(include_str!("../migrations/002_lookup_cache.sql"))
            .unwrap();
        db.execute_batch(include_str!("../migrations/003_settings.sql"))
            .unwrap();
        db.execute(
            "INSERT INTO skill_snapshots VALUES (?1,'dict','SKILL.md','fixture')",
            [&f.skill.hash],
        )
        .unwrap();
        db.execute(
            "INSERT INTO lookups VALUES (?1,?2,?2,'en_zh',?3,?2,'en_zh')",
            rusqlite::params![attempt.lookup_id, attempt.input, attempt.created_at],
        )
        .unwrap();
        db.execute("INSERT INTO attempts(id,lookup_id,attempt_number,state,created_at,model,effort,skill_hash,payload) VALUES (?1,?2,1,'succeeded',?3,'fixture-model','low',?4,?5)", rusqlite::params![attempt.id, attempt.lookup_id, attempt.created_at, f.skill.hash, payload]).unwrap();
        db.execute(
            "INSERT INTO app_state VALUES ('preferences',?1)",
            [serde_json::to_string(&preferences).unwrap()],
        )
        .unwrap();
    }
    let migrated = Database::open(&path).unwrap();
    let page = migrated
        .history_page(&HistoryQuery {
            filter: HistoryFilter {
                search: "OLDER".into(),
                ..Default::default()
            },
            ..Default::default()
        })
        .unwrap();
    assert_eq!(row_ids(&page), HashSet::from([attempt.id.clone()]));
    assert_eq!(migrated.get(&attempt.id).unwrap().answer, attempt.answer);
    assert_eq!(
        migrated.read_state::<Preferences>("preferences").unwrap(),
        Some(preferences.clone())
    );
    let db = rusqlite::Connection::open(&path).unwrap();
    assert_eq!(
        db.query_row("SELECT payload FROM attempts", [], |r| r
            .get::<_, String>(0))
            .unwrap(),
        payload
    );
    drop(migrated);
    let reopened = Database::open(&path).unwrap();
    assert_eq!(
        row_ids(&reopened.history_page(&HistoryQuery::default()).unwrap()),
        row_ids(&page)
    );
    assert_eq!(
        reopened.read_state::<Preferences>("preferences").unwrap(),
        Some(preferences)
    );
}

#[test]
fn delayed_search_responses_do_not_replace_new_filters_or_pages() {
    let mut f = Fixture::new();
    f.insert("unrelated word", Direction::EnZh, AttemptStatus::Succeeded);
    for i in 0..PAGE_SIZE + 3 {
        f.insert(
            &format!("从容 {i}"),
            Direction::ZhEn,
            AttemptStatus::Succeeded,
        );
    }
    let mut state = HistoryState::default();
    let (old_request, old_query) = state.request();
    state.filter(HistoryFilter {
        search: "从容".into(),
        ..Default::default()
    });
    let (current, query) = state.request();
    state.receive(old_request, f.db.history_page(&old_query).unwrap());
    assert!(
        state.loading && state.page.rows.is_empty(),
        "a superseded query cannot replace the pending search"
    );
    state.receive(current, f.db.history_page(&query).unwrap());
    let first_ids = row_ids(&state.page);
    state.fail(old_request, "stale error".into());
    assert!(state.error.is_none());
    assert!(state.next_page());
    let (next, second) = state.request();
    assert_eq!(second.filter, query.filter);
    assert!(
        !state.previous_page(),
        "avoid changing pages during an outstanding request"
    );
    state.receive(next, f.db.history_page(&second).unwrap());
    let second_ids = row_ids(&state.page);
    assert!(first_ids.is_disjoint(&second_ids));
    state.receive(current, f.db.history_page(&query).unwrap());
    assert_eq!(
        row_ids(&state.page),
        second_ids,
        "late page results cannot replace the current page"
    );
    assert!(state.previous_page());
    f.load(&mut state);
    assert_eq!(row_ids(&state.page), first_ids);
    assert_eq!(state.query.filter, query.filter);
}

#[test]
fn a_failed_page_can_be_retried_or_left_without_skipping_history() {
    let mut f = Fixture::new();
    for i in 0..PAGE_SIZE * 2 + 1 {
        f.insert(
            &format!("entry {i}"),
            Direction::EnZh,
            AttemptStatus::Succeeded,
        );
    }
    let mut state = HistoryState::default();
    f.load(&mut state);
    let first_ids = row_ids(&state.page);
    assert!(state.next_page());
    let (request, query) = state.request();
    state.fail(request, "storage unavailable".into());
    assert!(
        !state.next_page(),
        "a failed query must not reuse the previous page's cursor"
    );
    assert!(state.needs_refresh());
    f.load(&mut state);
    assert_eq!(state.query, query);
    assert!(first_ids.is_disjoint(&row_ids(&state.page)));
    assert!(state.error.is_none());
    assert!(state.previous_page());
    f.load(&mut state);
    assert_eq!(row_ids(&state.page), first_ids);
}

#[test]
fn deleting_the_last_page_recovers_the_previous_page_and_keeps_the_filter() {
    let mut f = Fixture::new();
    for i in 0..PAGE_SIZE + 1 {
        f.insert(
            &format!("entry {i}"),
            Direction::EnZh,
            AttemptStatus::Succeeded,
        );
    }
    let mut state = HistoryState::default();
    state.filter(HistoryFilter {
        search: "entry".into(),
        ..Default::default()
    });
    f.load(&mut state);
    let first_ids = row_ids(&state.page);
    assert!(state.next_page());
    let (request, query) = state.request();
    let outdated = f.db.history_page(&query).unwrap();
    for row in &outdated.rows {
        f.db.delete_lookup(&row.entry.lookup_id).unwrap();
    }
    state.invalidate();
    state.receive(request, outdated);
    assert!(
        state.needs_refresh(),
        "a change during a query requires a fresh read"
    );
    f.load(&mut state);
    assert!(
        state.needs_refresh(),
        "the emptied page must return to its previous cursor"
    );
    f.load(&mut state);
    assert_eq!(row_ids(&state.page), first_ids);
    assert_eq!(state.query.filter, query.filter);
    assert!(!state.can_previous());
    assert!(!state.needs_refresh());
}
