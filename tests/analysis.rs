use std::{sync::Arc, time::Instant};

use chrono::{DateTime, Duration, Utc};
use chrono_tz::Tz;
use dictionary::{
    analysis::{AnalysisFilter, AnalysisQuery, AnalysisState, DateRange},
    direction::Direction,
    model::{Attempt, AttemptStatus, Setup, SkillSnapshot, Usage},
    store::Database,
};

struct Fixture {
    db: Database,
    setup: Setup,
    skill: SkillSnapshot,
    _root: tempfile::TempDir,
}

fn instant(value: &str) -> DateTime<Utc> {
    value.parse().unwrap()
}

fn query(start: &str, end: &str, timezone: Tz) -> AnalysisQuery {
    AnalysisQuery {
        range: DateRange {
            start: start.parse().unwrap(),
            end: end.parse().unwrap(),
        },
        timezone,
        filter: AnalysisFilter::default(),
        as_of: instant("2026-12-01T00:00:00Z"),
    }
}

impl Fixture {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let setup = Setup {
            executable: "/unused-cli".into(),
            cli_version: "fixture".into(),
            model: "model-a".into(),
            effort: "low".into(),
            service_tier: None,
            skill_path: root.path().join("SKILL.md"),
            skill_name: "dict".into(),
        };
        std::fs::write(&setup.skill_path, "Analysis test fixture").unwrap();
        let skill = SkillSnapshot::read(&setup).unwrap();
        Self {
            db: Database::open(&root.path().join("history.sqlite3")).unwrap(),
            setup,
            skill,
            _root: root,
        }
    }

    fn attempt(&self, created: &str) -> Attempt {
        let mut attempt = Attempt::new(
            "example".into(),
            self.setup.clone(),
            self.skill.hash.clone(),
        );
        attempt.created_at = created.into();
        attempt.status = AttemptStatus::Succeeded;
        attempt.answer = "中文释义".into();
        attempt
    }

    fn save_new(&mut self, attempt: &Attempt) {
        self.db.insert(attempt, &self.skill).unwrap();
        self.db.save(attempt).unwrap();
    }
}

#[test]
fn reported_subtotals_preserve_missing_counters_subsets_and_weighted_cache_share() {
    let mut f = Fixture::new();
    let mut a = f.attempt("2026-10-12T09:00:00Z");
    a.usage = Usage {
        input_tokens: Some(100),
        output_tokens: Some(50),
        cached_input_tokens: Some(90),
        reasoning_output_tokens: Some(20),
    };
    f.save_new(&a);
    let mut b = f.attempt("2026-10-12T10:00:00Z");
    b.usage = Usage {
        input_tokens: Some(900),
        output_tokens: Some(30),
        cached_input_tokens: Some(90),
        reasoning_output_tokens: None,
    };
    b.status = AttemptStatus::Failed;
    f.save_new(&b);
    let mut partial = f.attempt("2026-10-13T09:00:00Z");
    partial.usage.input_tokens = Some(12);
    f.save_new(&partial);
    let missing = f.attempt("2026-10-14T09:00:00Z");
    f.save_new(&missing);
    let mut zero = f.attempt("2026-10-15T09:00:00Z");
    zero.usage.input_tokens = Some(0);
    zero.usage.output_tokens = Some(0);
    f.save_new(&zero);
    // Repeated checkpoints replace this attempt's measurements, never add usage.
    f.db.save(&a).unwrap();
    let report =
        f.db.analysis(query("2026-10-12", "2026-10-16", chrono_tz::UTC))
            .unwrap();
    assert_eq!(report.summary.attempts, 5);
    assert_eq!(report.summary.tokens.subtotal(), Some(1092));
    assert_eq!(report.summary.tokens.complete, 3);
    assert_eq!(report.summary.tokens.cached.value(), Some(180));
    assert_eq!(report.summary.tokens.reasoning.value(), Some(20));
    assert!((report.summary.tokens.cached_share().unwrap() - 0.18).abs() < 1e-10);
    assert_eq!(report.summary.tokens.cache_samples, 2);
    assert_eq!(report.days[2].metrics.tokens.subtotal(), None);
    assert_eq!(report.days[3].metrics.tokens.subtotal(), Some(0));
    assert_eq!(report.days[4].metrics.attempts, 0);
    assert_eq!(report.days[4].metrics.tokens.subtotal(), None);
}

#[test]
fn local_boundaries_include_fractional_instants_and_dst_days_have_their_real_lengths() {
    let mut f = Fixture::new();
    for created in [
        "2026-10-11T15:59:59.999Z",
        "2026-10-11T16:00:00.000Z",
        "2026-10-12T15:59:59.999Z",
        "2026-10-12T16:00:00Z",
    ] {
        let a = f.attempt(created);
        f.save_new(&a);
    }
    let report =
        f.db.analysis(query("2026-10-12", "2026-10-12", chrono_tz::Asia::Shanghai))
            .unwrap();
    assert_eq!(report.summary.attempts, 2);
    assert_eq!(report.days[0].metrics.attempts, 2);
    let spring = query("2026-03-08", "2026-03-08", chrono_tz::America::New_York);
    let (start, end) = spring.range.bounds(spring.timezone).unwrap();
    assert_eq!((end - start).num_hours(), 23);
    let fall = query("2026-11-01", "2026-11-01", chrono_tz::America::New_York);
    let (start, end) = fall.range.bounds(fall.timezone).unwrap();
    assert_eq!((end - start).num_hours(), 25);
    for created in ["2026-11-01T05:30:00Z", "2026-11-01T06:30:00Z"] {
        let a = f.attempt(created);
        f.save_new(&a);
    }
    assert_eq!(f.db.analysis(fall).unwrap().days[0].metrics.attempts, 2);
    let skipped = query("2011-12-30", "2011-12-30", chrono_tz::Pacific::Apia);
    let (start, end) = skipped.range.bounds(skipped.timezone).unwrap();
    assert_eq!(start, end, "a skipped local date contains no instants");
}

#[test]
fn empty_ranges_and_incomplete_days_do_not_invent_measurements() {
    let f = Fixture::new();
    let mut q = query("2026-10-10", "2026-10-12", chrono_tz::Asia::Shanghai);
    q.as_of = instant("2026-10-12T04:00:00Z");
    let report = f.db.analysis(q).unwrap();
    assert_eq!(report.days.len(), 3);
    assert_eq!(report.summary.attempts, 0);
    assert_eq!(report.summary.tokens.subtotal(), None);
    assert_eq!(report.summary.latency.median_ms, None);
    assert!(report.days[0].complete && report.days[1].complete && !report.days[2].complete);
    assert!(report.histogram.iter().all(|count| *count == 0));
    assert!(
        f.db.analysis(query("2026-10-12", "2026-10-10", chrono_tz::UTC))
            .is_err()
    );
}

#[test]
fn latency_uses_nearest_rank_p95_and_an_explicit_outcome_population() {
    let mut f = Fixture::new();
    for i in 1..=20 {
        let mut a = f.attempt("2026-10-12T10:00:00Z");
        a.duration_ms = Some(if i == 20 { 1_000_000 } else { i * 1000 });
        f.save_new(&a);
    }
    let mut failure = f.attempt("2026-10-12T11:00:00Z");
    failure.status = AttemptStatus::Failed;
    failure.duration_ms = Some(5);
    f.save_new(&failure);
    let mut active = f.attempt("2026-10-12T12:00:00Z");
    active.status = AttemptStatus::Running;
    active.usage.input_tokens = Some(77);
    f.save_new(&active);
    let missing = f.attempt("2026-10-12T13:00:00Z");
    f.save_new(&missing);
    let mut q = query("2026-10-12", "2026-10-12", chrono_tz::UTC);
    let report = f.db.analysis(q.clone()).unwrap();
    assert_eq!(report.summary.latency.samples, 20);
    assert_eq!(report.summary.latency.median_ms, Some(10_500.));
    assert_eq!(report.summary.latency.p95_ms, Some(19_000));
    assert_eq!(report.histogram.iter().sum::<usize>(), 20);
    assert_eq!(report.histogram, [4, 10, 5, 0, 0, 1]);
    assert_eq!(report.active_usage.len(), 1);
    assert_eq!(report.active_usage.get(&active.id), Some(&active.usage));
    for outcome in &report.outcomes {
        let expected = match outcome.status {
            AttemptStatus::Succeeded => 21,
            AttemptStatus::Failed | AttemptStatus::Running => 1,
            _ => 0,
        };
        assert_eq!(outcome.metrics.attempts, expected);
    }
    q.filter.status = Some(AttemptStatus::Failed);
    let report = f.db.analysis(q).unwrap();
    assert_eq!(report.summary.attempts, 1);
    assert_eq!(report.summary.latency.median_ms, Some(5.));
    assert_eq!(report.summary.latency.p95_ms, Some(5));
    assert!(report.active_usage.is_empty());
}

#[test]
fn retries_filters_history_reads_and_deletion_share_the_same_underlying_attempts() {
    let mut f = Fixture::new();
    let mut first = f.attempt("2026-10-01T10:00:00Z");
    first.direction = Direction::ZhEn;
    first.input = "从容不迫".into();
    first.usage.input_tokens = Some(50);
    first.usage.output_tokens = Some(10);
    f.save_new(&first);
    let mut retry = f.attempt("2026-10-12T10:00:00Z");
    retry.lookup_id = first.lookup_id.clone();
    retry.input = first.input.clone();
    retry.direction = first.direction;
    retry.setup.model = "model-b".into();
    retry.setup.effort = "high".into();
    retry.usage = first.usage.clone();
    retry.duration_ms = Some(800);
    f.save_new(&retry);
    let unrelated = f.attempt("2026-10-12T10:00:00Z");
    f.save_new(&unrelated);
    let q = query("2026-10-12", "2026-10-12", chrono_tz::UTC);
    let report = f.db.analysis(q.clone()).unwrap();
    assert_eq!(report.summary.attempts, 2);
    assert_eq!(report.summary.new_lookups, 1);
    assert_eq!(report.summary.tokens.subtotal(), Some(60));
    f.db.get(&first.id).unwrap();
    f.db.cached(&retry.input, retry.direction).unwrap();
    f.db.history_page(&Default::default()).unwrap();
    assert_eq!(f.db.analysis(q.clone()).unwrap().summary.attempts, 2);
    let mut filtered = q.clone();
    filtered.filter = AnalysisFilter {
        model: Some("model-b".into()),
        effort: Some("high".into()),
        direction: Some(Direction::ZhEn),
        status: Some(AttemptStatus::Succeeded),
    };
    let report = f.db.analysis(filtered.clone()).unwrap();
    assert_eq!(report.summary.attempts, 1);
    assert_eq!(report.summary.new_lookups, 0);
    assert_eq!(report.summary.tokens.subtotal(), Some(60));
    assert_eq!(report.summary.latency.median_ms, Some(800.));
    filtered.filter.effort = Some("low".into());
    assert_eq!(f.db.analysis(filtered).unwrap().summary.attempts, 0);
    f.db.delete_lookup(&first.lookup_id).unwrap();
    let report = f.db.analysis(q).unwrap();
    assert_eq!(report.summary.attempts, 1);
    assert_eq!(report.summary.new_lookups, 1);
    assert_eq!(report.top_inputs[0].input, unrelated.input);
    assert_eq!(report.summary.tokens.subtotal(), None);
}

#[test]
fn old_results_and_errors_cannot_replace_new_filters_and_inflight_changes_are_refetched() {
    let f = Fixture::new();
    let q = query("2026-10-12", "2026-10-12", chrono_tz::UTC);
    let mut state = AnalysisState::new(q.clone());
    let (old, old_query) = state.request(q.as_of);
    state.change(
        q.range.clone(),
        AnalysisFilter {
            model: Some("new-model".into()),
            ..Default::default()
        },
    );
    let (new, new_query) = state.request(q.as_of);
    assert!(!state.receive(old, Arc::new(f.db.analysis(old_query).unwrap())));
    state.fail(old, "stale error".into());
    assert!(state.loading && state.error.is_none());
    state.invalidate();
    assert!(state.receive(new, Arc::new(f.db.analysis(new_query).unwrap())));
    assert!(state.needs_refresh());
    let (retry, _) = state.request(q.as_of);
    state.fail(retry, "storage unavailable".into());
    assert!(state.needs_refresh() && !state.loading);
}

#[test]
fn repeat_rate_uses_normalized_new_submissions_not_regenerations_or_opposite_directions() {
    let mut f = Fixture::new();
    let mut first = f.attempt("2026-10-12T09:00:00Z");
    first.input = "café".into();
    f.save_new(&first);
    let mut retry = f.attempt("2026-10-12T10:00:00Z");
    retry.lookup_id = first.lookup_id.clone();
    retry.input = first.input.clone();
    f.save_new(&retry);
    let mut repeated = f.attempt("2026-10-12T11:00:00Z");
    repeated.input = "  cafe\u{301}  ".into();
    f.save_new(&repeated);
    let mut reverse = f.attempt("2026-10-12T12:00:00Z");
    reverse.input = first.input.clone();
    reverse.direction = Direction::ZhEn;
    f.save_new(&reverse);
    let report =
        f.db.analysis(query("2026-10-12", "2026-10-12", chrono_tz::UTC))
            .unwrap();
    assert_eq!(report.summary.new_lookups, 3);
    assert_eq!(report.repeated_submissions, 1);
    assert_eq!(report.top_inputs.len(), 2);
    assert_eq!(report.top_inputs[0].input, first.input);
    assert_eq!(report.top_inputs[0].direction, Direction::EnZh);
    assert_eq!(report.top_inputs[0].metrics.new_lookups, 2);
    assert_eq!(report.top_inputs[0].metrics.attempts, 3);
    assert_eq!(report.top_inputs[1].direction, Direction::ZhEn);
    assert_eq!(report.top_inputs[1].metrics.new_lookups, 1);
}

#[test]
fn ten_thousand_attempts_can_be_filtered_without_loading_answers() {
    let mut f = Fixture::new();
    let mut previous: Option<Attempt> = None;
    for i in 0..10_000 {
        let mut a =
            f.attempt(&(instant("2026-08-01T00:00:00Z") + Duration::minutes(i * 10)).to_rfc3339());
        a.input = format!("sample {}", i / 2);
        a.setup.model = format!("model-{}", i % 4);
        a.setup.effort = if i % 2 == 0 { "low" } else { "high" }.into();
        if i % 2 == 1 {
            a.lookup_id = previous.as_ref().unwrap().lookup_id.clone();
        }
        a.usage.input_tokens = Some(100);
        a.usage.output_tokens = Some(30);
        a.duration_ms = Some(1000 + i as u64);
        a.answer = "Large saved answer. ".repeat(256);
        f.save_new(&a);
        previous = Some(a);
    }
    let q = query("2026-08-01", "2026-11-01", chrono_tz::Asia::Shanghai);
    let start = Instant::now();
    let report = f.db.analysis(q.clone()).unwrap();
    let full_time = start.elapsed();
    assert_eq!(report.summary.attempts, 10_000);
    assert_eq!(report.summary.new_lookups, 5_000);
    assert_eq!(report.summary.tokens.subtotal(), Some(1_300_000));
    assert!(report.active_usage.is_empty());
    let mut filtered = q;
    filtered.filter.model = Some("model-1".into());
    let start = Instant::now();
    let filtered = f.db.analysis(filtered).unwrap();
    let filtered_time = start.elapsed();
    assert_eq!(filtered.summary.attempts, 2500);
    assert_eq!(filtered.summary.new_lookups, 0);
    assert_eq!(filtered.summary.tokens.subtotal(), Some(325_000));
    println!("10,000 attempts: full query {full_time:?}; model filter {filtered_time:?}");
}
