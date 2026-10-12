use dictionary::{
    model::{Activity, Attempt, AttemptStatus, HistoryEntry, Setup},
    runner::Progress,
    service::Event,
    view_state::LookupState,
};

fn attempt(input: &str) -> Attempt {
    Attempt::new(
        input.into(),
        Setup {
            executable: "/unused-cli".into(),
            cli_version: "fixture".into(),
            model: "fixture-model".into(),
            effort: "low".into(),
            service_tier: Some(dictionary::model::ServiceTier::Standard),
            skill_path: "/unused-skill".into(),
            skill_name: "dict".into(),
        },
        "fixture-hash".into(),
    )
}

fn open(view: &mut LookupState, attempt: &Attempt) {
    let request_id = view.begin_navigation();
    view.receive(&Event::Opened {
        request_id,
        attempt: Box::new(attempt.clone()),
    });
}

fn progress(attempt: &Attempt) -> Event {
    Event::Progress(Progress {
        attempt_id: attempt.id.clone(),
        answer: format!("# {}\n\n这是{}的翻译", attempt.input, attempt.input),
        activity: vec![Activity {
            id: "r1".into(),
            label: "Thinking".into(),
            text: format!("Checking {}", attempt.input),
            completed: false,
        }],
    })
}

#[test]
fn history_refresh_keeps_an_older_cached_selection_visible() {
    let mut view = LookupState::default();
    let mut old = attempt("older cached entry");
    old.status = AttemptStatus::Succeeded;
    old.answer = "已保存的翻译".into();
    open(&mut view, &old);
    let recent = attempt("recent entry");
    view.receive(&Event::History(vec![HistoryEntry::from(&recent)]));
    assert_eq!(view.selected.as_ref().unwrap().id, old.id);
    assert!(view.history.iter().any(|entry| entry.id == old.id));
    assert!(view.history.iter().any(|entry| entry.id == recent.id));
}

#[test]
fn delayed_navigation_cannot_replace_a_newer_selection_or_a_new_lookup() {
    let mut view = LookupState::default();
    let a = attempt("first");
    let b = attempt("second");
    let first_request = view.begin_navigation();
    open(&mut view, &b);
    view.receive(&Event::Opened {
        request_id: first_request,
        attempt: Box::new(a.clone()),
    });
    assert_eq!(view.selected.as_ref().unwrap().id, b.id);
    assert!(view.lookup_active(&a.lookup_id));
    assert!(view.lookup_active(&b.lookup_id));
    let delayed_request = view.begin_navigation();
    view.new_lookup();
    view.receive(&Event::Opened {
        request_id: delayed_request,
        attempt: Box::new(b),
    });
    assert!(view.selected.is_none());
    assert!(view.has_active());
    assert_eq!(view.history.len(), 2);
}

#[test]
fn background_work_preserves_selection_live_previews_and_each_activity_record() {
    let mut view = LookupState::default();
    let mut a = attempt("first");
    let b = attempt("second");
    open(&mut view, &a);
    open(&mut view, &b);
    a.status = AttemptStatus::Running;
    view.receive(&Event::Started(Box::new(a.clone())));
    view.receive(&progress(&a));
    view.receive(&progress(&b));
    // Finishing A refreshes the database while B's partial answer only exists in memory.
    a.status = AttemptStatus::Succeeded;
    a.answer = "完整翻译".into();
    view.receive(&Event::Finished {
        attempt: Box::new(a.clone()),
        save_error: None,
    });
    view.receive(&Event::History(vec![
        HistoryEntry::from(&b),
        HistoryEntry::from(&a),
    ]));
    let selected = view.selected.as_ref().unwrap();
    assert_eq!(selected.id, b.id);
    assert_eq!(selected.activity[0].text, "Checking second");
    assert!(selected.answer.contains("second"));
    assert!(view.activity_expanded);
    let row = view.history.iter().find(|entry| entry.id == b.id).unwrap();
    assert!(row.translation.contains("second"));
    assert!(row.generating);
    assert!(!view.lookup_active(&a.lookup_id));
    assert!(view.lookup_active(&b.lookup_id));
}

#[test]
fn completion_collapses_only_its_own_activity_and_late_progress_is_ignored() {
    let mut view = LookupState::default();
    let mut a = attempt("first");
    open(&mut view, &a);
    view.receive(&progress(&a));
    a.status = AttemptStatus::Succeeded;
    a.answer = "最终翻译".into();
    view.receive(&Event::Finished {
        attempt: Box::new(a.clone()),
        save_error: None,
    });
    view.receive(&progress(&a));
    assert!(!view.has_active());
    assert!(!view.activity_expanded);
    assert_eq!(view.selected.as_ref().unwrap().answer, "最终翻译");
    assert_eq!(view.history[0].translation, "最终翻译");
}

#[test]
fn background_save_failure_preserves_the_result_without_navigating_away() {
    let mut view = LookupState::default();
    let mut a = attempt("first");
    let b = attempt("second");
    open(&mut view, &a);
    open(&mut view, &b);
    let stale = a.clone();
    a.status = AttemptStatus::Succeeded;
    a.answer = "仅存于内存的翻译".into();
    view.receive(&Event::Finished {
        attempt: Box::new(a.clone()),
        save_error: Some("disk full".into()),
    });
    view.receive(&Event::History(vec![
        HistoryEntry::from(&stale),
        HistoryEntry::from(&b),
    ]));
    assert_eq!(view.selected.as_ref().unwrap().id, b.id);
    assert_eq!(view.save_error(&a.id), Some("disk full"));
    assert_eq!(
        view.history
            .iter()
            .find(|entry| entry.id == a.id)
            .unwrap()
            .translation,
        a.answer
    );
    open(&mut view, &a);
    assert_eq!(view.selected.as_ref().unwrap().answer, a.answer);
    assert!(!view.activity_expanded);
    assert!(view.has_active());
    view.receive(&Event::DeletedLookup(a.lookup_id));
    assert!(view.selected.is_none());
    assert_eq!(view.history.len(), 1);
    assert_eq!(view.history[0].id, b.id);
}
