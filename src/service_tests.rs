use super::*;
use sha2::{Digest, Sha256};

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
            skill_path: fixtures.join("activity-skill.md"),
            skill_name: "dict".into(),
        };
        let (service, events) = Service::start_inner(paths.clone(), Some(setup)).unwrap();
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
            .accept(None, LookupRequest::Search(" break the ice \n".into()))
            .await
            .unwrap();
        assert_eq!(cached.id, original.id);
        assert_eq!(cached.usage.total(), Some(120));
        assert_eq!(store.history().await.unwrap().len(), 1);
    }
    assert!(
        coordinator
            .accept(None, LookupRequest::Search("new expression".into()))
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
            LookupRequest::Search(original.input.clone()),
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
            .cached(original.input.clone())
            .await
            .unwrap()
            .unwrap()
            .id,
        original.id
    );
    assert_eq!(store.get(original.id).await.unwrap().answer, "打破僵局");
}
