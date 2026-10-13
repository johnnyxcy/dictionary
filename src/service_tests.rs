use super::*;
use sha2::{Digest, Sha256};

#[tokio::test]
async fn analysis_during_work_is_read_only_and_refreshes_after_completion_and_deletion() {
    let mut h = Harness::new();
    h.lookup(1, "analysis saved word");
    let saved = h.opened(1).await;
    h.release(&saved.input);
    let saved = h.finished(&saved.id).await;
    h.lookup(2, "analysis background word");
    let running = h.opened(2).await;
    h.streaming(&running.id).await;
    let spawned = h.spawned().len();
    let today = chrono::Utc::now().date_naive();
    for request_id in 10..=12 {
        h.send(Command::QueryAnalysis {
            request_id,
            query: crate::analysis::AnalysisQuery {
                range: crate::analysis::DateRange::recent(7, today),
                timezone: chrono_tz::UTC,
                filter: Default::default(),
                as_of: chrono::Utc::now(),
            },
        });
        h.until(|events| events.iter().any(|event| matches!(event, Event::Analysis { request_id: id, .. } if *id == request_id))).await;
        let report = h
            .seen
            .iter()
            .find_map(|event| match event {
                Event::Analysis {
                    request_id: id,
                    report,
                } if *id == request_id => Some(report),
                _ => None,
            })
            .unwrap();
        if request_id == 10 {
            assert_eq!(report.summary.attempts, 2);
            assert_eq!(
                report.summary.tokens.subtotal(),
                saved.usage.total().map(u128::from)
            );
            assert!(report.active_usage.contains_key(&running.id));
            assert!(!report.active_usage.contains_key(&saved.id));
            h.send(Command::Cancel(running.id.clone()));
            h.finished(&running.id).await;
        } else if request_id == 11 {
            assert!(report.active_usage.is_empty());
            assert_eq!(
                report
                    .outcomes
                    .iter()
                    .find(|outcome| outcome.status == AttemptStatus::Cancelled)
                    .unwrap()
                    .metrics
                    .attempts,
                1
            );
            h.send(Command::DeleteLookup(saved.lookup_id.clone()));
            h.until(|events| {
                events.iter().any(
                    |event| matches!(event, Event::DeletedLookup(id) if *id == saved.lookup_id),
                )
            })
            .await;
        } else {
            assert_eq!(report.summary.attempts, 1);
            assert_eq!(report.top_inputs[0].input, running.input);
            assert_eq!(report.summary.tokens.subtotal(), None);
        }
    }
    assert_eq!(h.spawned().len(), spawned);
}

#[tokio::test]
async fn searching_and_opening_history_during_work_never_submits_another_attempt() {
    let mut h = Harness::new();
    h.lookup(1, "saved history word");
    let saved = h.opened(1).await;
    h.release(&saved.input);
    let saved = h.finished(&saved.id).await;
    h.lookup(2, "background word");
    let running = h.opened(2).await;
    h.streaming(&running.id).await;
    let started_before = h.spawned().len();
    let attempts_before: Vec<_> = h.saved().into_iter().map(|attempt| attempt.id).collect();
    h.send(Command::QueryHistory {
        request_id: 10,
        query: HistoryQuery {
            filter: crate::history::HistoryFilter {
                search: "HISTORY".into(),
                ..Default::default()
            },
            ..Default::default()
        },
    });
    h.until(|events| {
        events
            .iter()
            .any(|event| matches!(event, Event::HistoryPage { request_id: 10, .. }))
    })
    .await;
    let page = h
        .seen
        .iter()
        .find_map(|event| match event {
            Event::HistoryPage {
                request_id: 10,
                page,
            } => Some(page),
            _ => None,
        })
        .unwrap();
    assert_eq!(page.rows.len(), 1);
    assert_eq!(page.rows[0].entry.id, saved.id);
    assert_eq!(page.rows[0].usage, saved.usage);
    h.send(Command::Open {
        request_id: 11,
        attempt_id: saved.id.clone(),
    });
    let opened = h.opened(11).await;
    assert_eq!(opened.id, saved.id);
    assert_eq!(opened.answer, saved.answer);
    assert_eq!(opened.usage, saved.usage);
    h.send(Command::Open {
        request_id: 12,
        attempt_id: running.id.clone(),
    });
    assert!(
        h.opened(12).await.status.is_active(),
        "browsing history must leave other work running"
    );
    assert_eq!(h.spawned().len(), started_before);
    h.send(Command::Cancel(running.id.clone()));
    assert_eq!(
        h.finished(&running.id).await.status,
        AttemptStatus::Cancelled
    );
    assert_eq!(
        h.saved()
            .into_iter()
            .map(|attempt| attempt.id)
            .collect::<Vec<_>>(),
        attempts_before
    );
}

struct Harness {
    service: Option<Service>,
    events: Receiver<Event>,
    seen: Vec<Event>,
    paths: AppPaths,
    _root: tempfile::TempDir,
}

impl Harness {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let paths = AppPaths::at(root.path().to_owned()).unwrap();
        let fixtures = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
        let setup = Setup {
            executable: fixtures.join("concurrent-cli.py"),
            cli_version: "fixture".into(),
            model: "fixture-model".into(),
            effort: "low".into(),
            service_tier: Some(crate::model::ServiceTier::Standard),
            skill_path: fixtures.join("activity-skill.md"),
            skill_name: "dict".into(),
        };
        let (service, events) =
            Service::start_inner(paths.clone(), Some(fixture_catalog(setup))).unwrap();
        Self {
            service: Some(service),
            events,
            seen: Vec::new(),
            paths,
            _root: root,
        }
    }

    fn send(&self, command: Command) {
        self.service.as_ref().unwrap().send(command).unwrap();
    }

    fn lookup(&self, request_id: RequestId, input: &str) {
        self.send(Command::Lookup {
            request_id,
            input: input.into(),
            direction: DirectionChoice::Auto,
        });
    }

    async fn until(&mut self, predicate: impl Fn(&[Event]) -> bool) {
        tokio::time::timeout(Duration::from_secs(10), async {
            while !predicate(&self.seen) {
                let event = self
                    .events
                    .recv()
                    .await
                    .expect("service closed unexpectedly");
                assert!(
                    !matches!(&event, Event::Fatal(_) | Event::RequestFailed { .. }),
                    "{event:?}"
                );
                self.seen.push(event);
            }
        })
        .await
        .expect("fixture event timed out");
    }

    async fn opened(&mut self, id: RequestId) -> Attempt {
        self.until(|events| {
            events
                .iter()
                .any(|e| matches!(e, Event::Opened { request_id, .. } if *request_id == id))
        })
        .await;
        self.seen
            .iter()
            .find_map(|event| match event {
                Event::Opened {
                    request_id,
                    attempt,
                } if *request_id == id => Some(*attempt.clone()),
                _ => None,
            })
            .unwrap()
    }

    async fn streaming(&mut self, id: &str) {
        self.until(|events| {
            events.iter().any(
                |e| matches!(e, Event::Progress(p) if p.attempt_id == id && !p.answer.is_empty()),
            )
        })
        .await;
    }

    async fn finished(&mut self, id: &str) -> Attempt {
        self.until(|events| {
            events
                .iter()
                .any(|e| matches!(e, Event::Finished { attempt, .. } if attempt.id == id))
        })
        .await;
        self.seen
            .iter()
            .find_map(|event| match event {
                Event::Finished {
                    attempt,
                    save_error,
                } if attempt.id == id => {
                    assert!(save_error.is_none(), "{save_error:?}");
                    Some(*attempt.clone())
                }
                _ => None,
            })
            .unwrap()
    }

    fn release(&self, input: &str) {
        let control = self.paths.workspace.join("fixture-control");
        std::fs::create_dir_all(&control).unwrap();
        std::fs::write(
            control.join(format!("release-{:x}", Sha256::digest(input))),
            "",
        )
        .unwrap();
    }

    fn spawned(&self) -> Vec<serde_json::Value> {
        std::fs::read_dir(self.paths.workspace.join("fixture-control"))
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .filter(|path| {
                path.extension()
                    .is_some_and(|extension| extension == "json")
            })
            .map(|path| serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap())
            .collect()
    }

    fn saved(&self) -> Vec<Attempt> {
        let db = rusqlite::Connection::open(&self.paths.database).unwrap();
        db.prepare("SELECT payload FROM attempts ORDER BY rowid")
            .unwrap()
            .query_map([], |row| row.get::<_, String>(0))
            .unwrap()
            .map(|json| serde_json::from_str(&json.unwrap()).unwrap())
            .collect()
    }
}

#[tokio::test]
async fn rapid_duplicate_commands_admit_and_start_exactly_one_attempt() {
    let mut h = Harness::new();
    // Queue every click before consuming the first response, including the
    // interval while admission is awaiting its SQLite write.
    for id in 1..=10 {
        h.lookup(id, "rapid repeat");
    }
    let first = h.opened(1).await;
    for id in 2..=10 {
        assert_eq!(h.opened(id).await.id, first.id);
    }
    h.streaming(&first.id).await;
    assert_eq!(h.saved().len(), 1);
    assert_eq!(h.spawned().len(), 1);
    h.release("rapid repeat");
    assert_eq!(h.finished(&first.id).await.status, AttemptStatus::Succeeded);
}

#[tokio::test]
async fn concurrent_results_are_isolated_and_a_free_slot_starts_the_next_lookup() {
    let mut h = Harness::new();
    for (id, input) in [(1, "alpha"), (2, "bravo"), (3, "charlie"), (4, "delta")] {
        h.lookup(id, input);
    }
    let a = h.opened(1).await;
    let b = h.opened(2).await;
    let c = h.opened(3).await;
    let d = h.opened(4).await;
    for attempt in [&a, &b, &c] {
        h.streaming(&attempt.id).await;
    }
    assert_eq!(d.status, AttemptStatus::Pending);
    assert_eq!(h.spawned().len(), MAX_CONCURRENT_LOOKUPS);
    // Duplicate admission reserves both running and queued keys before any await can race.
    h.lookup(5, " alpha \n");
    h.lookup(6, "delta");
    assert_eq!(h.opened(5).await.id, a.id);
    assert_eq!(h.opened(6).await.id, d.id);
    assert_eq!(h.saved().len(), 4);
    h.release("bravo");
    h.finished(&b.id).await;
    h.streaming(&d.id).await;
    assert_eq!(h.spawned().len(), 4);
    // Cache is checked before capacity, even with three other processes active.
    h.lookup(7, "bravo");
    let cached = h.opened(7).await;
    assert_eq!(cached.id, b.id);
    assert_eq!(cached.status, AttemptStatus::Succeeded);
    assert_eq!(h.saved().len(), 4);
    for attempt in [&d, &c, &a] {
        h.release(&attempt.input);
        let finished = h.finished(&attempt.id).await;
        assert_eq!(finished.status, AttemptStatus::Succeeded);
        assert!(
            finished
                .answer
                .starts_with(&format!("# {}\n", attempt.input))
        );
        assert_eq!(
            finished.usage.total(),
            Some(110 + 2 * attempt.input.len() as u64)
        );
        assert!(
            finished
                .activity
                .iter()
                .all(|item| item.text.contains(&attempt.input))
        );
        assert!(finished.queue_duration_ms.is_some());
        assert!(finished.duration_ms >= finished.cli_duration_ms);
    }
    let completed: Vec<_> = h
        .seen
        .iter()
        .filter_map(|event| match event {
            Event::Finished { attempt, .. } => Some(attempt.id.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(
        completed,
        vec![b.id.as_str(), d.id.as_str(), c.id.as_str(), a.id.as_str()]
    );
    assert_eq!(h.saved().len(), 4);
}

#[tokio::test]
async fn regeneration_reserves_its_key_before_cached_search_and_repeated_clicks() {
    let mut h = Harness::new();
    h.lookup(1, "saved");
    let original = h.opened(1).await;
    h.streaming(&original.id).await;
    h.release("saved");
    h.finished(&original.id).await;
    for (id, input) in [(2, "one"), (3, "two"), (4, "three")] {
        h.lookup(id, input);
    }
    for id in 2..=4 {
        let attempt = h.opened(id).await;
        h.streaming(&attempt.id).await;
    }
    h.send(Command::Regenerate {
        request_id: 5,
        attempt_id: original.id.clone(),
    });
    let regenerated = h.opened(5).await;
    assert_eq!(regenerated.status, AttemptStatus::Pending);
    assert_eq!(regenerated.lookup_id, original.lookup_id);
    assert_ne!(regenerated.id, original.id);
    h.lookup(6, "saved");
    h.send(Command::Regenerate {
        request_id: 7,
        attempt_id: original.id.clone(),
    });
    assert_eq!(h.opened(6).await.id, regenerated.id);
    assert_eq!(h.opened(7).await.id, regenerated.id);
    assert_eq!(h.saved().len(), 5);
    assert_eq!(h.spawned().len(), 4);
    // Cancelling a queued regeneration leaves the older successful cache usable.
    h.send(Command::Cancel(regenerated.id.clone()));
    assert_eq!(
        h.finished(&regenerated.id).await.status,
        AttemptStatus::Cancelled
    );
    h.lookup(8, "saved");
    assert_eq!(h.opened(8).await.id, original.id);
    assert_eq!(h.spawned().len(), 4);
}

#[tokio::test]
async fn cancellation_and_failure_only_affect_their_own_attempt_and_advance_the_queue() {
    let mut h = Harness::new();
    for (id, input) in [
        (1, "first"),
        (2, "second"),
        (3, "simulate failure"),
        (4, "skip queued"),
        (5, "next queued"),
    ] {
        h.lookup(id, input);
    }
    let a = h.opened(1).await;
    let b = h.opened(2).await;
    let c = h.opened(3).await;
    let d = h.opened(4).await;
    let e = h.opened(5).await;
    for attempt in [&a, &b, &c] {
        h.streaming(&attempt.id).await;
    }
    h.send(Command::Cancel(d.id.clone()));
    let cancelled = h.finished(&d.id).await;
    assert_eq!(cancelled.status, AttemptStatus::Cancelled);
    assert!(cancelled.started_at.is_none());
    assert!(cancelled.usage.total().is_none());
    assert_eq!(h.spawned().len(), 3);
    h.send(Command::Cancel(b.id.clone()));
    let stopped = h.finished(&b.id).await;
    assert_eq!(stopped.status, AttemptStatus::Cancelled);
    assert!(stopped.answer.contains("second"));
    h.streaming(&e.id).await;
    assert_eq!(h.spawned().len(), 4);
    h.release(&c.input);
    let failed = h.finished(&c.id).await;
    assert_eq!(failed.status, AttemptStatus::Failed);
    assert!(failed.answer.contains("simulate failure"));
    for attempt in [&e, &a] {
        h.release(&attempt.input);
        assert_eq!(
            h.finished(&attempt.id).await.status,
            AttemptStatus::Succeeded
        );
    }
    assert!(
        !h.spawned()
            .iter()
            .any(|process| process["input"] == "skip queued")
    );
    assert_eq!(h.saved().len(), 5);
}

#[tokio::test]
async fn quitting_finalizes_all_running_and_queued_work_and_reaps_every_process() {
    let mut h = Harness::new();
    for id in 1..=5 {
        h.lookup(id, &format!("shutdown {id}"));
    }
    let mut ids = Vec::new();
    for id in 1..=5 {
        ids.push(h.opened(id).await.id);
    }
    for id in ids.iter().take(3) {
        h.streaming(id).await;
    }
    let processes = h.spawned();
    assert_eq!(processes.len(), 3);
    drop(h.service.take());
    let saved = h.saved();
    assert_eq!(saved.len(), 5);
    assert!(
        saved
            .iter()
            .all(|attempt| attempt.status == AttemptStatus::Cancelled)
    );
    assert!(
        saved
            .iter()
            .skip(3)
            .all(|attempt| attempt.started_at.is_none())
    );
    assert_eq!(h.spawned().len(), 3);
    #[cfg(unix)]
    for process in processes {
        let pid = process["pid"].as_i64().unwrap() as i32;
        assert_eq!(
            unsafe { libc::kill(pid, 0) },
            -1,
            "child {pid} survived shutdown"
        );
    }
}

async fn saved_entry(root: &std::path::Path) -> (Store, Attempt) {
    let skill_path = root.join("SKILL.md");
    std::fs::write(&skill_path, "Explain the expression in Chinese.").unwrap();
    let setup = Setup {
        executable: root.join("missing-cli"),
        cli_version: "fixture".into(),
        model: "original-model".into(),
        effort: "low".into(),
        service_tier: Some(crate::model::ServiceTier::Standard),
        skill_path,
        skill_name: "dict".into(),
    };
    let skill = SkillSnapshot::read(&setup).unwrap();
    let store = Store::open(root.join("history.sqlite3")).await.unwrap();
    let mut original = Attempt::new("break the ice".into(), setup, skill.hash.clone());
    store.insert(original.clone(), skill).await.unwrap();
    original.status = AttemptStatus::Succeeded;
    original.answer = "打破僵局".into();
    original.usage.input_tokens = Some(100);
    original.usage.output_tokens = Some(20);
    store.save(original.clone()).await.unwrap();
    (store, original)
}

#[tokio::test]
async fn cached_navigation_needs_no_setup_skill_or_new_attempt() {
    let root = tempfile::tempdir().unwrap();
    let (store, original) = saved_entry(root.path()).await;
    let mut coordinator = Coordinator::new(store.clone(), root.path().to_owned());
    std::fs::remove_file(&original.setup.skill_path).unwrap();
    for _ in 0..3 {
        let cached = coordinator
            .accept(
                None,
                LookupRequest::Search(" break the ice \n".into(), DirectionChoice::Auto),
            )
            .await
            .unwrap();
        assert_eq!(cached.id, original.id);
        assert_eq!(cached.usage.total(), Some(120));
        assert_eq!(store.history().await.unwrap().len(), 1);
    }
    assert!(
        coordinator
            .accept(
                None,
                LookupRequest::Search("new expression".into(), DirectionChoice::Auto)
            )
            .await
            .is_err()
    );
    assert!(
        coordinator
            .accept(None, LookupRequest::Regenerate(original.id))
            .await
            .is_err()
    );
    assert_eq!(store.history().await.unwrap().len(), 1);
}

#[tokio::test]
async fn deleted_lookup_is_not_reused_by_search_or_regeneration() {
    let root = tempfile::tempdir().unwrap();
    let (store, original) = saved_entry(root.path()).await;
    let mut coordinator = Coordinator::new(store.clone(), root.path().to_owned());
    store
        .delete_lookup(original.lookup_id.clone())
        .await
        .unwrap();
    assert!(store.history().await.unwrap().is_empty());
    assert!(
        coordinator
            .accept(
                Some(&original.setup),
                LookupRequest::Regenerate(original.id.clone())
            )
            .await
            .is_err()
    );
    let attempt = coordinator
        .accept(
            Some(&original.setup),
            LookupRequest::Search(original.input.clone(), DirectionChoice::Auto),
        )
        .await
        .unwrap();
    assert_ne!(attempt.lookup_id, original.lookup_id);
    assert_ne!(attempt.id, original.id);
    assert_eq!(attempt.input, original.input);
}

#[tokio::test]
async fn regenerate_bypasses_cache_with_fresh_settings_and_original_input() {
    let root = tempfile::tempdir().unwrap();
    let (store, original) = saved_entry(root.path()).await;
    let mut coordinator = Coordinator::new(store.clone(), root.path().to_owned());
    let mut current = original.setup.clone();
    current.model = "current-model".into();
    current.effort = "high".into();
    std::fs::write(&current.skill_path, "Updated dictionary instructions.").unwrap();
    let attempt = coordinator
        .accept(
            Some(&current),
            LookupRequest::Regenerate(original.id.clone()),
        )
        .await
        .unwrap();
    let prompt = coordinator.jobs[&attempt.id].prompt.as_ref().unwrap();
    assert_ne!(attempt.id, original.id);
    assert_eq!(attempt.lookup_id, original.lookup_id);
    assert_eq!(attempt.input, original.input);
    assert_eq!(attempt.setup.model, "current-model");
    assert_eq!(attempt.setup.effort, "high");
    assert_ne!(attempt.skill_hash, original.skill_hash);
    assert!(prompt.contains("Updated dictionary instructions."));
    assert!(prompt.contains("break the ice"));
    assert_eq!(
        store.get(attempt.id.clone()).await.unwrap().status,
        AttemptStatus::Pending
    );
    assert_eq!(store.history().await.unwrap().len(), 2);
    // A pending regeneration never displaces the last successful cached answer.
    assert_eq!(
        store
            .cached(original.input.clone(), original.direction)
            .await
            .unwrap()
            .unwrap()
            .id,
        original.id
    );
    assert_eq!(store.get(original.id).await.unwrap().answer, "打破僵局");
}

fn fixture_catalog(setup: Setup) -> Catalog {
    Catalog {
        skill_diagnostic: None,
        executable: setup.executable,
        cli_version: setup.cli_version,
        retrieved_at: now(),
        configured_model: Some(setup.model.clone()),
        models: vec![crate::settings::ModelOption {
            id: setup.model.clone(),
            model: setup.model,
            display_name: "Fixture model".into(),
            efforts: vec!["low".into(), "medium".into(), "high".into(), "max".into()],
            default_effort: "low".into(),
            is_default: true,
            supports_fast: true,
        }],
        skills: vec![crate::settings::SkillSource {
            path: setup.skill_path,
            name: setup.skill_name,
            hash: "fixture".into(),
        }],
    }
}

#[tokio::test]
async fn settings_changes_preserve_running_and_queued_snapshots_and_survive_restart() {
    let mut h = Harness::new();
    for (id, input) in [
        (1, "first"),
        (2, "second"),
        (3, "third"),
        (4, "queued before"),
    ] {
        h.lookup(id, input);
    }
    let first = h.opened(1).await;
    let second = h.opened(2).await;
    let third = h.opened(3).await;
    let fourth = h.opened(4).await;
    h.streaming(&first.id).await;
    assert_eq!(fourth.status, AttemptStatus::Pending);
    let state = h
        .seen
        .iter()
        .find_map(|event| match event {
            Event::Settings(state) => Some(*state.clone()),
            _ => None,
        })
        .unwrap();
    let catalog = state.catalog.unwrap();
    let mut preferences = state.preferences.unwrap();
    preferences.effort = "medium".into();
    preferences.fast_mode = true;
    h.send(Command::SaveSettings(preferences.clone()));
    h.until(|events| events.iter().any(|e| matches!(e, Event::SettingsSaved)))
        .await;
    h.lookup(5, "queued after");
    let fifth = h.opened(5).await;
    assert_eq!(fifth.setup.effort, "medium");
    assert_eq!(fourth.setup.effort, "low");
    assert_eq!(
        fifth.setup.service_tier,
        Some(crate::model::ServiceTier::Fast)
    );
    assert_eq!(
        fourth.setup.service_tier,
        Some(crate::model::ServiceTier::Standard)
    );
    h.release("first");
    h.finished(&first.id).await;
    h.streaming(&fourth.id).await;
    h.release("second");
    h.finished(&second.id).await;
    h.streaming(&fifth.id).await;
    for (input, effort, tier) in [
        ("first", "low", "default"),
        ("queued before", "low", "default"),
        ("queued after", "medium", "fast"),
    ] {
        let started = h
            .spawned()
            .into_iter()
            .find(|s| s["input"] == input)
            .unwrap();
        assert!(
            started["args"]
                .as_array()
                .unwrap()
                .iter()
                .any(|a| a == &format!("model_reasoning_effort=\"{effort}\""))
        );
        let args = started["args"].as_array().unwrap();
        assert!(
            args.iter()
                .any(|arg| arg == &format!("service_tier=\"{tier}\""))
        );
        assert_eq!(
            args.iter().any(|arg| arg == "features.fast_mode=true"),
            tier == "fast"
        );
    }
    for attempt in [&third, &fourth, &fifth] {
        h.release(&attempt.input);
        h.finished(&attempt.id).await;
    }
    let old = h.saved();
    drop(h.service.take());
    let (service, events) = Service::start_inner(h.paths.clone(), Some(catalog)).unwrap();
    h.service = Some(service);
    h.events = events;
    h.seen.clear();
    h.until(|events| {
        events
            .iter()
            .any(|e| matches!(e, Event::Ready(setup) if setup.effort == "medium" && setup.service_tier == Some(crate::model::ServiceTier::Fast)))
    })
    .await;
    assert_eq!(
        serde_json::to_value(h.saved()).unwrap(),
        serde_json::to_value(old).unwrap()
    );
}

#[tokio::test]
async fn unsupported_settings_are_rejected_without_replacing_saved_preferences() {
    let mut h = Harness::new();
    h.until(|events| events.iter().any(|e| matches!(e, Event::Ready(_))))
        .await;
    let mut preferences = h
        .seen
        .iter()
        .find_map(|e| match e {
            Event::Settings(state) => state.preferences.clone(),
            _ => None,
        })
        .unwrap();
    preferences.effort = "xhigh".into();
    h.send(Command::SaveSettings(preferences));
    h.until(|events| events.iter().any(|e| matches!(e, Event::SettingsFailed(_))))
        .await;
    h.lookup(1, "still low");
    let attempt = h.opened(1).await;
    assert_eq!(attempt.setup.effort, "low");
    h.release(&attempt.input);
    h.finished(&attempt.id).await;
    let db = rusqlite::Connection::open(&h.paths.database).unwrap();
    let json: String = db
        .query_row(
            "SELECT payload FROM app_state WHERE key='preferences'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        serde_json::from_str::<Preferences>(&json).unwrap().effort,
        "low"
    );
}

#[tokio::test]
async fn missing_discovered_skill_can_be_repaired_without_losing_the_model_catalog() {
    let mut h = Harness::new();
    h.until(|events| events.iter().any(|e| matches!(e, Event::Ready(_))))
        .await;
    let mut catalog = h
        .seen
        .iter()
        .find_map(|e| match e {
            Event::Settings(state) => state.catalog.clone(),
            _ => None,
        })
        .unwrap();
    let path = catalog.skills[0].path.clone();
    catalog.skills.clear();
    catalog.skill_diagnostic = Some("Skill discovery unavailable; use Browse".into());
    drop(h.service.take());
    let mut db = crate::store::Database::open(&h.paths.database).unwrap();
    db.write_state("preferences", &Preferences::initial(&catalog))
        .unwrap();
    drop(db);
    let (service, events) = Service::start_inner(h.paths.clone(), Some(catalog)).unwrap();
    h.service = Some(service);
    h.events = events;
    h.seen.clear();
    h.until(|events| events.iter().any(|e| matches!(e, Event::SetupFailed(_))))
        .await;
    let state = h
        .seen
        .iter()
        .find_map(|e| match e {
            Event::Settings(state) => Some(*state.clone()),
            _ => None,
        })
        .unwrap();
    assert!(!state.catalog.unwrap().models.is_empty());
    let mut preferences = state.preferences.unwrap();
    preferences.skill_path = path;
    h.send(Command::SaveSettings(preferences));
    h.until(|events| events.iter().any(|e| matches!(e, Event::SettingsSaved)))
        .await;
    h.lookup(1, "repaired skill");
    let attempt = h.opened(1).await;
    h.release(&attempt.input);
    assert_eq!(
        h.finished(&attempt.id).await.status,
        AttemptStatus::Succeeded
    );
}

#[tokio::test]
async fn active_and_cached_lookups_are_separate_in_each_direction_and_regeneration_preserves_direction()
 {
    let mut h = Harness::new();
    let input = "你好 hello";
    h.send(Command::Lookup {
        request_id: 1,
        input: input.into(),
        direction: DirectionChoice::EnZh,
    });
    h.send(Command::Lookup {
        request_id: 2,
        input: input.into(),
        direction: DirectionChoice::ZhEn,
    });
    let forward = h.opened(1).await;
    let reverse = h.opened(2).await;
    assert_ne!(forward.id, reverse.id);
    assert_eq!(reverse.direction, Direction::ZhEn);
    h.send(Command::Lookup {
        request_id: 3,
        input: input.into(),
        direction: DirectionChoice::ZhEn,
    });
    assert_eq!(h.opened(3).await.id, reverse.id);
    h.streaming(&forward.id).await;
    h.streaming(&reverse.id).await;
    h.release(input);
    h.finished(&forward.id).await;
    h.finished(&reverse.id).await;
    h.send(Command::Lookup {
        request_id: 4,
        input: input.into(),
        direction: DirectionChoice::EnZh,
    });
    h.send(Command::Lookup {
        request_id: 5,
        input: input.into(),
        direction: DirectionChoice::ZhEn,
    });
    assert_eq!(h.opened(4).await.id, forward.id);
    assert_eq!(h.opened(5).await.id, reverse.id);
    assert_eq!(h.spawned().len(), 2);
    h.send(Command::Regenerate {
        request_id: 6,
        attempt_id: reverse.id.clone(),
    });
    let regenerated = h.opened(6).await;
    assert_eq!(regenerated.lookup_id, reverse.lookup_id);
    assert_eq!(regenerated.direction, Direction::ZhEn);
    assert_eq!(regenerated.input, input);
    assert_eq!(
        h.finished(&regenerated.id).await.status,
        AttemptStatus::Succeeded
    );
    let prompts = h.spawned();
    assert_eq!(
        prompts
            .iter()
            .filter(|p| p["prompt"]
                .as_str()
                .unwrap()
                .starts_with("Produce one Chinese-to-English"))
            .count(),
        2
    );
}
