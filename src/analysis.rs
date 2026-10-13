//! Measurement definitions and immutable analysis snapshots, independent of GPUI.
use std::{collections::BTreeMap, sync::Arc};

use anyhow::{Context, Result, ensure};
use chrono::{DateTime, Days, Duration, NaiveDate, TimeZone, Utc};
use chrono_tz::Tz;

use crate::{
    direction::Direction,
    model::{AttemptStatus, Usage},
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DateRange {
    pub start: NaiveDate,
    pub end: NaiveDate,
}

impl DateRange {
    pub fn recent(days: u64, today: NaiveDate) -> Self {
        Self {
            start: today - Days::new(days.saturating_sub(1)),
            end: today,
        }
    }

    pub fn bounds(&self, timezone: Tz) -> Result<(DateTime<Utc>, DateTime<Utc>)> {
        ensure!(
            self.start <= self.end,
            "The start date must precede the end date."
        );
        ensure!(
            (self.end - self.start).num_days() < 3660,
            "Choose a date range of at most 10 years."
        );
        Ok((
            day_start(self.start, timezone)?,
            day_start(
                self.end.succ_opt().context("End date is out of range")?,
                timezone,
            )?,
        ))
    }
}

/// Use the first valid instant of a local date, including zones that skip
/// midnight or a whole date. Never approximate a local day as 24 UTC hours.
fn day_start(date: NaiveDate, timezone: Tz) -> Result<DateTime<Utc>> {
    let midnight = date.and_hms_opt(0, 0, 0).context("Invalid date")?;
    for minute in 0..=2880 {
        let local = midnight
            .checked_add_signed(Duration::minutes(minute))
            .context("Date is out of range")?;
        if let Some(time) = timezone.from_local_datetime(&local).earliest() {
            return Ok(time.with_timezone(&Utc));
        }
    }
    anyhow::bail!("Cannot resolve this date in {timezone}")
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AnalysisFilter {
    pub model: Option<String>,
    pub effort: Option<String>,
    pub direction: Option<Direction>,
    pub status: Option<AttemptStatus>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AnalysisQuery {
    pub range: DateRange,
    pub timezone: Tz,
    pub filter: AnalysisFilter,
    pub as_of: DateTime<Utc>,
}

impl AnalysisQuery {
    pub fn latency_status(&self) -> AttemptStatus {
        self.filter
            .status
            .clone()
            .unwrap_or(AttemptStatus::Succeeded)
    }
}

/// A projection of persisted measurements, never the answer or skill contents.
#[derive(Clone, Debug)]
pub(crate) struct AnalysisRecord {
    pub id: String,
    pub version: u64,
    pub input: String,
    pub direction: Direction,
    pub created_at: DateTime<Utc>,
    pub status: AttemptStatus,
    pub usage: Usage,
    pub duration_ms: Option<u64>,
    pub first_answer_ms: Option<u64>,
    pub queue_duration_ms: Option<u64>,
    pub cli_duration_ms: Option<u64>,
}

#[derive(Clone, Debug, Default)]
pub struct ReportedCounter {
    pub sum: u128,
    pub samples: usize,
}

impl ReportedCounter {
    fn add(&mut self, value: Option<u64>) {
        if let Some(value) = value {
            self.sum += u128::from(value);
            self.samples += 1;
        }
    }

    pub fn value(&self) -> Option<u128> {
        (self.samples > 0).then_some(self.sum)
    }
}

#[derive(Clone, Debug, Default)]
pub struct TokenSummary {
    pub complete: usize,
    pub input: ReportedCounter,
    pub output: ReportedCounter,
    pub cached: ReportedCounter,
    pub reasoning: ReportedCounter,
    paired_cached: u128,
    paired_input: u128,
    pub cache_samples: usize,
}

impl TokenSummary {
    fn add(&mut self, usage: &Usage) {
        self.complete += usize::from(usage.total().is_some());
        self.input.add(usage.input_tokens);
        self.output.add(usage.output_tokens);
        self.cached.add(usage.cached_input_tokens);
        self.reasoning.add(usage.reasoning_output_tokens);
        if let (Some(cached), Some(input)) = (usage.cached_input_tokens, usage.input_tokens) {
            self.paired_cached += u128::from(cached);
            self.paired_input += u128::from(input);
            self.cache_samples += 1;
        }
    }

    /// All reported input/output counters, including partial reports. Subset
    /// counters never enter the total. Coverage states whether this is complete.
    pub fn subtotal(&self) -> Option<u128> {
        (self.input.samples + self.output.samples > 0).then_some(self.input.sum + self.output.sum)
    }

    pub fn cached_share(&self) -> Option<f64> {
        (self.paired_input > 0).then(|| self.paired_cached as f64 / self.paired_input as f64)
    }
}

#[derive(Clone, Debug, Default)]
pub struct LatencySummary {
    pub samples: usize,
    pub median_ms: Option<f64>,
    pub p95_ms: Option<u64>,
}

impl LatencySummary {
    fn from_values(mut values: Vec<u64>) -> Self {
        values.sort_unstable();
        let n = values.len();
        if n == 0 {
            return Self::default();
        }
        let median = if n.is_multiple_of(2) {
            values[n / 2 - 1] as f64 / 2. + values[n / 2] as f64 / 2.
        } else {
            values[n / 2] as f64
        };
        Self {
            samples: n,
            median_ms: Some(median),
            p95_ms: Some(values[(n * 95).div_ceil(100) - 1]),
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct Metrics {
    pub attempts: usize,
    /// Only the first attempt is a new lookup. A retry inside the range must
    /// not turn a lookup created outside it into a new submission.
    pub new_lookups: usize,
    pub tokens: TokenSummary,
    pub latency: LatencySummary,
    pub first_answer: LatencySummary,
    pub queue: LatencySummary,
    pub cli: LatencySummary,
}

#[derive(Clone, Debug)]
pub struct DailyBucket {
    pub date: NaiveDate,
    pub complete: bool,
    pub metrics: Metrics,
}

#[derive(Clone, Debug)]
pub struct Outcome {
    pub status: AttemptStatus,
    pub metrics: Metrics,
}

#[derive(Clone, Debug)]
pub struct SubmittedInput {
    pub input: String,
    pub direction: Direction,
    pub metrics: Metrics,
}

pub const LATENCY_BUCKETS: [(&str, u64, Option<u64>); 6] = [
    ("< 5 s", 0, Some(5_000)),
    ("5–15 s", 5_000, Some(15_000)),
    ("15–30 s", 15_000, Some(30_000)),
    ("30–60 s", 30_000, Some(60_000)),
    ("1–2 min", 60_000, Some(120_000)),
    ("≥ 2 min", 120_000, None),
];
#[derive(Clone, Debug)]
pub struct AnalysisReport {
    pub query: AnalysisQuery,
    pub summary: Metrics,
    pub days: Vec<DailyBucket>,
    pub outcomes: Vec<Outcome>,
    pub histogram: Vec<usize>,
    pub repeated_submissions: usize,
    pub top_inputs: Vec<SubmittedInput>,
    pub models: Vec<String>,
    pub efforts: Vec<String>,
    /// Saved usage for active attempts, used to refresh charts when usage changes.
    pub active_usage: BTreeMap<String, Usage>,
}

fn metrics<'a>(
    records: impl Iterator<Item = &'a AnalysisRecord>,
    population: &AttemptStatus,
) -> Metrics {
    let mut result = Metrics::default();
    let (mut latency, mut first, mut queue, mut cli) = (vec![], vec![], vec![], vec![]);
    for row in records {
        result.attempts += 1;
        result.new_lookups += usize::from(row.version == 1);
        result.tokens.add(&row.usage);
        if &row.status == population && !row.status.is_active() {
            latency.extend(row.duration_ms);
            first.extend(row.first_answer_ms);
            queue.extend(row.queue_duration_ms);
            cli.extend(row.cli_duration_ms);
        }
    }
    result.latency = LatencySummary::from_values(latency);
    result.first_answer = LatencySummary::from_values(first);
    result.queue = LatencySummary::from_values(queue);
    result.cli = LatencySummary::from_values(cli);
    result
}

impl AnalysisReport {
    pub(crate) fn build(
        query: AnalysisQuery,
        records: Vec<AnalysisRecord>,
        models: Vec<String>,
        efforts: Vec<String>,
    ) -> Result<Self> {
        query.range.bounds(query.timezone)?;
        let population = query.latency_status();
        let summary = metrics(records.iter(), &population);
        let mut dates: BTreeMap<NaiveDate, Vec<&AnalysisRecord>> = BTreeMap::new();
        let mut inputs: BTreeMap<(&str, String), Vec<&AnalysisRecord>> = BTreeMap::new();
        for row in &records {
            dates
                .entry(row.created_at.with_timezone(&query.timezone).date_naive())
                .or_default()
                .push(row);
            inputs
                .entry((row.direction.key(), crate::model::lookup_key(&row.input)))
                .or_default()
                .push(row);
        }
        let mut days = Vec::new();
        for date in query
            .range
            .start
            .iter_days()
            .take_while(|d| *d <= query.range.end)
        {
            days.push(DailyBucket {
                date,
                complete: day_start(
                    date.succ_opt().context("Date is out of range")?,
                    query.timezone,
                )? <= query.as_of,
                metrics: metrics(dates.get(&date).into_iter().flatten().copied(), &population),
            });
        }
        let outcomes = [
            AttemptStatus::Succeeded,
            AttemptStatus::Failed,
            AttemptStatus::TimedOut,
            AttemptStatus::Cancelled,
            AttemptStatus::Interrupted,
            AttemptStatus::Running,
            AttemptStatus::Pending,
        ]
        .into_iter()
        .map(|status| Outcome {
            metrics: metrics(records.iter().filter(|r| r.status == status), &status),
            status,
        })
        .collect();
        let histogram = LATENCY_BUCKETS
            .iter()
            .map(|(_, start, end)| {
                records
                    .iter()
                    .filter(|row| {
                        row.status == population
                            && !row.status.is_active()
                            && row
                                .duration_ms
                                .is_some_and(|ms| ms >= *start && end.is_none_or(|end| ms < end))
                    })
                    .count()
            })
            .collect();
        let mut top_inputs: Vec<_> = inputs
            .into_iter()
            .map(|((_, input), rows)| SubmittedInput {
                direction: rows[0].direction,
                input,
                metrics: metrics(rows.into_iter(), &population),
            })
            .filter(|input| input.metrics.new_lookups > 0)
            .collect();
        let repeated_submissions = summary.new_lookups - top_inputs.len();
        top_inputs.sort_by(|a, b| {
            b.metrics
                .new_lookups
                .cmp(&a.metrics.new_lookups)
                .then_with(|| a.input.cmp(&b.input))
                .then_with(|| a.direction.key().cmp(b.direction.key()))
        });
        top_inputs.truncate(5);
        let active_usage = records
            .iter()
            .filter(|row| row.status.is_active())
            .map(|row| (row.id.clone(), row.usage.clone()))
            .collect();
        Ok(Self {
            query,
            summary,
            days,
            outcomes,
            histogram,
            repeated_submissions,
            top_inputs,
            models,
            efforts,
            active_usage,
        })
    }
}

/// Coalesce background invalidations and reject old filters' responses.
pub struct AnalysisState {
    pub query: AnalysisQuery,
    pub report: Option<Arc<AnalysisReport>>,
    pub loading: bool,
    pub error: Option<String>,
    dirty: bool,
    request_id: u64,
}

impl AnalysisState {
    pub fn new(query: AnalysisQuery) -> Self {
        Self {
            query,
            report: None,
            loading: false,
            error: None,
            dirty: true,
            request_id: 0,
        }
    }
    pub fn invalidate(&mut self) {
        self.dirty = true;
    }
    pub fn needs_refresh(&self) -> bool {
        self.dirty && !self.loading
    }
    pub fn request(&mut self, as_of: DateTime<Utc>) -> (u64, AnalysisQuery) {
        self.request_id += 1;
        self.query.as_of = as_of;
        self.loading = true;
        self.error = None;
        self.dirty = false;
        (self.request_id, self.query.clone())
    }
    pub fn change(&mut self, range: DateRange, filter: AnalysisFilter) {
        self.query.range = range;
        self.query.filter = filter;
        self.report = None;
        self.invalidate();
    }
    pub fn receive(&mut self, request_id: u64, report: Arc<AnalysisReport>) -> bool {
        if request_id != self.request_id {
            return false;
        }
        self.report = Some(report);
        self.loading = false;
        self.error = None;
        true
    }
    pub fn fail(&mut self, request_id: u64, error: String) {
        if request_id == self.request_id {
            self.loading = false;
            self.error = Some(error);
            self.dirty = true;
        }
    }
}
