use chrono::{NaiveDate, Utc};
use dictionary::{
    analysis::{
        AnalysisFilter, AnalysisQuery, AnalysisReport, AnalysisState, DateRange, LATENCY_BUCKETS,
    },
    direction::Direction,
    model::AttemptStatus,
    service::Event,
};
use gpui_kit::{
    assets::IconName,
    component::{
        ActiveTheme, Disableable, Icon, Selectable, Sizable,
        button::{Button, ButtonVariants},
        chart::BarChart,
        date_picker::{DatePicker, DatePickerEvent, DatePickerState, DateRangePreset},
        select::{Select, SelectEvent, SelectState},
        spinner::Spinner,
    },
    prelude::FluentBuilder,
    *,
};

use super::{formatting, settings::PickerItem};

type Picker<T> = Entity<SelectState<Vec<PickerItem<T>>>>;

pub enum AnalysisEvent {
    Query {
        request_id: u64,
        query: AnalysisQuery,
    },
    Back,
}

pub struct AnalysisView {
    state: AnalysisState,
    timezone_error: Option<String>,
    dates: Entity<DatePickerState>,
    model: Picker<Option<String>>,
    effort: Picker<Option<String>>,
    direction: Picker<Option<Direction>>,
    outcome: Picker<Option<AttemptStatus>>,
    frequency_attempts: bool,
    scroll: ScrollHandle,
    visible: bool,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<AnalysisEvent> for AnalysisView {}

impl AnalysisView {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let timezone = iana_time_zone::get_timezone()
            .map_err(|e| e.to_string())
            .and_then(|name| {
                name.parse::<chrono_tz::Tz>()
                    .map_err(|_| format!("Unknown system timezone: {name}"))
            });
        let timezone_error = timezone.as_ref().err().cloned();
        // A resolution error blocks queries and is displayed, never silently
        // presents a UTC fallback as local measurements.
        let timezone = timezone.unwrap_or(chrono_tz::UTC);
        let now = Utc::now();
        let range = DateRange::recent(30, now.with_timezone(&timezone).date_naive());
        let dates = cx.new(|cx| {
            let mut state = DatePickerState::range(window, cx).date_format("%Y-%m-%d");
            state.set_date((range.start, range.end), window, cx);
            state
        });
        let model = picker(vec![(None, "All models".into())], window, cx);
        let effort = picker(vec![(None, "All efforts".into())], window, cx);
        let direction = picker(
            vec![
                (None, "All directions".into()),
                (Some(Direction::EnZh), Direction::EnZh.label().into()),
                (Some(Direction::ZhEn), Direction::ZhEn.label().into()),
            ],
            window,
            cx,
        );
        let outcome = picker(
            std::iter::once((None, "All outcomes".into()))
                .chain(
                    [
                        AttemptStatus::Succeeded,
                        AttemptStatus::Failed,
                        AttemptStatus::TimedOut,
                        AttemptStatus::Cancelled,
                        AttemptStatus::Interrupted,
                        AttemptStatus::Pending,
                        AttemptStatus::Running,
                    ]
                    .into_iter()
                    .map(|status| (Some(status.clone()), status_label(&status).into())),
                )
                .collect(),
            window,
            cx,
        );
        let subscriptions = vec![
            cx.subscribe_in(&dates, window, |this: &mut Self, _, event, _, cx| {
                let DatePickerEvent::Change(value) = event;
                if let (Some(start), Some(end)) = (value.start(), value.end()) {
                    this.state.change(
                        DateRange {
                            start: start.date(),
                            end: end.date(),
                        },
                        this.state.query.filter.clone(),
                    );
                    this.scroll.set_offset(point(px(0.), px(0.)));
                    this.refresh(cx);
                }
            }),
            cx.subscribe_in(&model, window, |this: &mut Self, _, event, _, cx| {
                if let SelectEvent::Confirm(Some(value)) = event {
                    let mut filter = this.state.query.filter.clone();
                    filter.model = value.clone();
                    this.filter(filter, cx);
                }
            }),
            cx.subscribe_in(&effort, window, |this: &mut Self, _, event, _, cx| {
                if let SelectEvent::Confirm(Some(value)) = event {
                    let mut filter = this.state.query.filter.clone();
                    filter.effort = value.clone();
                    this.filter(filter, cx);
                }
            }),
            cx.subscribe_in(&direction, window, |this: &mut Self, _, event, _, cx| {
                if let SelectEvent::Confirm(Some(value)) = event {
                    let mut filter = this.state.query.filter.clone();
                    filter.direction = *value;
                    this.filter(filter, cx);
                }
            }),
            cx.subscribe_in(&outcome, window, |this: &mut Self, _, event, _, cx| {
                if let SelectEvent::Confirm(Some(value)) = event {
                    let mut filter = this.state.query.filter.clone();
                    filter.status = value.clone();
                    this.filter(filter, cx);
                }
            }),
        ];
        Self {
            state: AnalysisState::new(AnalysisQuery {
                range,
                timezone,
                filter: AnalysisFilter::default(),
                as_of: now,
            }),
            timezone_error,
            dates,
            model,
            effort,
            direction,
            outcome,
            frequency_attempts: true,
            scroll: ScrollHandle::new(),
            visible: false,
            _subscriptions: subscriptions,
        }
    }

    pub fn show(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.visible = true;
        // Re-resolve on entry so changing the system timezone is reflected.
        match iana_time_zone::get_timezone()
            .ok()
            .and_then(|name| name.parse().ok())
        {
            Some(timezone) => {
                if self.state.query.timezone != timezone {
                    self.state.report = None;
                }
                self.state.query.timezone = timezone;
                self.timezone_error = None;
            }
            None => {
                self.timezone_error = Some(
                    "Cannot read the system timezone. Check macOS Date & Time settings.".into(),
                )
            }
        }
        self.refresh(cx);
        self.dates.focus_handle(cx).focus(window, cx);
    }

    pub fn hide(&mut self) {
        self.visible = false;
    }

    fn refresh(&mut self, cx: &mut Context<Self>) {
        if self.timezone_error.is_some() {
            cx.notify();
            return;
        }
        let (request_id, query) = self.state.request(Utc::now());
        cx.emit(AnalysisEvent::Query { request_id, query });
        cx.notify();
    }

    fn filter(&mut self, filter: AnalysisFilter, cx: &mut Context<Self>) {
        self.state.change(self.state.query.range.clone(), filter);
        self.scroll.set_offset(point(px(0.), px(0.)));
        self.refresh(cx);
    }

    pub fn fail(&mut self, request_id: u64, message: String, cx: &mut Context<Self>) {
        self.state.fail(request_id, message);
        cx.notify();
    }

    pub fn receive(&mut self, event: &Event, window: &mut Window, cx: &mut Context<Self>) {
        match event {
            Event::Analysis { request_id, report } => {
                if self.state.receive(*request_id, report.clone()) {
                    self.model.update(cx, |state, cx| {
                        state.set_items(
                            string_items(
                                "All models",
                                &report.models,
                                &self.state.query.filter.model,
                            ),
                            window,
                            cx,
                        );
                        state.set_selected_value(&self.state.query.filter.model, window, cx);
                    });
                    self.effort.update(cx, |state, cx| {
                        state.set_items(
                            string_items(
                                "All efforts",
                                &report.efforts,
                                &self.state.query.filter.effort,
                            ),
                            window,
                            cx,
                        );
                        state.set_selected_value(&self.state.query.filter.effort, window, cx);
                    });
                    if self.visible && self.state.needs_refresh() {
                        self.refresh(cx);
                    }
                }
            }
            Event::AnalysisFailed {
                request_id,
                message,
            } => self.state.fail(*request_id, message.clone()),
            Event::HistoryChanged => {
                self.state.invalidate();
                if self.visible && self.state.needs_refresh() {
                    self.refresh(cx);
                }
            }
            Event::Progress(progress)
                if self.state.report.as_ref().is_some_and(|report| {
                    report
                        .active_usage
                        .get(&progress.attempt_id)
                        .is_some_and(|usage| *usage != progress.metadata.usage)
                }) =>
            {
                self.state.invalidate();
                if self.visible && self.state.needs_refresh() {
                    self.refresh(cx);
                }
            }
            _ => return,
        }
        cx.notify();
    }

    fn reset(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.model
            .update(cx, |s, cx| s.set_selected_value(&None, window, cx));
        self.effort
            .update(cx, |s, cx| s.set_selected_value(&None, window, cx));
        self.direction
            .update(cx, |s, cx| s.set_selected_value(&None, window, cx));
        self.outcome
            .update(cx, |s, cx| s.set_selected_value(&None, window, cx));
        self.filter(AnalysisFilter::default(), cx);
    }

    fn recent(&mut self, days: u64, window: &mut Window, cx: &mut Context<Self>) {
        let range = DateRange::recent(
            days,
            Utc::now()
                .with_timezone(&self.state.query.timezone)
                .date_naive(),
        );
        self.dates
            .update(cx, |s, cx| s.set_date((range.start, range.end), window, cx));
        self.state.change(range, self.state.query.filter.clone());
        self.scroll.set_offset(point(px(0.), px(0.)));
        self.refresh(cx);
    }

    fn dashboard(&self, report: &AnalysisReport, cx: &Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let summary = &report.summary;
        let population = status_label(&report.query.latency_status());
        let coverage = format!(
            "{} / {} attempts with complete totals",
            summary.tokens.complete, summary.attempts
        );
        div().flex().flex_col().gap_4()
            .child(div().flex().flex_wrap().gap_3()
                .child(stat("Reported tokens", super::token_count("reported-tokens", summary.tokens.subtotal()), coverage.clone(), cx))
                .child(stat("Attempts", summary.attempts.to_string(), format!("{} new lookups · {} regenerations", summary.new_lookups, summary.attempts - summary.new_lookups), cx))
                .child(stat("Median response", millis(summary.latency.median_ms), format!("{population} · n = {}", summary.latency.samples), cx))
                .child(stat("p95 response", super::duration(summary.latency.p95_ms), "Nearest rank · same timing samples".into(), cx)))
            .child(panel("Token usage", "Reported input + output. Cached input and reasoning are already included; incomplete coverage means a subtotal.", cx)
                .child(div().flex().flex_wrap().gap_4().mt_3()
                    .child(token_metric("Input", summary.tokens.input.value(), cx))
                    .child(token_metric("Output", summary.tokens.output.value(), cx))
                    .child(token_metric("Cached", summary.tokens.cached.value(), cx))
                    .child(token_metric("Reasoning", summary.tokens.reasoning.value(), cx)))
                .child(chart("daily-tokens", report.days.iter().map(|day| ChartPoint {
                    label: day_label(day.date, day.complete),
                    value: day.metrics.tokens.subtotal().map(|n| n as f64),
                    detail: format!("{} tokens · totals reported for {}/{} attempts", formatting::exact_count(day.metrics.tokens.subtotal()), day.metrics.tokens.complete, day.metrics.attempts),
                    color: if day.complete { theme.primary } else { theme.primary.opacity(0.5) },
                }).collect()))
                .child(note("Blank bars have no reported tokens. * marks an unfinished day.", cx)))
            .child(div().flex().flex_wrap().gap_4()
                .child(panel("Request frequency", "New lookups count first attempts only. Reading cached entries adds no requests.", cx)
                    .child(div().flex().gap_1().mt_2()
                        .child(Button::new("frequency-attempts").small().ghost().label("Attempts").selected(self.frequency_attempts)
                            .on_click(cx.listener(|this, _, _, cx| { this.frequency_attempts = true; cx.notify(); })))
                        .child(Button::new("frequency-lookups").small().ghost().label("New lookups").selected(!self.frequency_attempts)
                            .on_click(cx.listener(|this, _, _, cx| { this.frequency_attempts = false; cx.notify(); }))))
                    .child(chart("daily-frequency", report.days.iter().map(|day| {
                        let count = if self.frequency_attempts { day.metrics.attempts } else { day.metrics.new_lookups };
                        ChartPoint { label: day_label(day.date, day.complete), value: Some(count as f64), detail: count.to_string(), color: theme.info }
                    }).collect())))
                .child(panel("Response time", &format!("Daily median in seconds · {population}. Includes queue wait. Blank bars have no measured sample."), cx)
                    .child(chart("daily-latency", report.days.iter().map(|day| ChartPoint {
                        label: day_label(day.date, day.complete), value: day.metrics.latency.median_ms.map(|ms| ms / 1000.),
                        detail: format!("{} · n = {} · p95 {}", millis(day.metrics.latency.median_ms), day.metrics.latency.samples, super::duration(day.metrics.latency.p95_ms)), color: theme.success,
                    }).collect()))
                    .child(note(format!("First answer {} (n={}) · Queue {} (n={}) · CLI {} (n={})",
                        millis(summary.first_answer.median_ms), summary.first_answer.samples,
                        millis(summary.queue.median_ms), summary.queue.samples,
                        millis(summary.cli.median_ms), summary.cli.samples), cx))))
            .child(div().flex().flex_wrap().gap_4()
                .child(panel("Response distribution", &format!("{population} · {} measured attempts.", summary.latency.samples), cx)
                    .child(chart("latency-distribution", LATENCY_BUCKETS.iter().enumerate().map(|(i, (label, _, _))| ChartPoint {
                        label: (*label).into(), value: Some(report.histogram[i] as f64), detail: format!("{} attempts", report.histogram[i]), color: theme.success,
                    }).collect())))
                .child(panel("Outcomes", &format!("{} attempts in this filter, including queued and running work.", summary.attempts), cx)
                    .child(chart("outcome-counts", report.outcomes.iter().map(|outcome| ChartPoint {
                        label: status_label(&outcome.status).into(), value: Some(outcome.metrics.attempts as f64),
                        detail: format!("{} / {} attempts · median {} (n={})", outcome.metrics.attempts, summary.attempts, millis(outcome.metrics.latency.median_ms), outcome.metrics.latency.samples),
                        color: if outcome.status == AttemptStatus::Succeeded { theme.success } else if outcome.status.is_active() { theme.info } else { theme.danger },
                    }).collect()))))
            .child(self.submitted_inputs(report, cx))
            .child(panel("Usage details", "Cache share is weighted by input tokens, using only attempts reporting both input and cached counters.", cx)
                .child(note(format!("Cached input share: {} · {} paired samples", summary.tokens.cached_share().map(|share| format!("{:.1}%", share * 100.)).unwrap_or_else(|| "Unavailable".into()), summary.tokens.cache_samples), cx))
                .child(note(format!("Input reported: {}/{} · Output: {}/{} · Cached: {}/{} · Reasoning: {}/{}",
                    summary.tokens.input.samples, summary.attempts, summary.tokens.output.samples, summary.attempts,
                    summary.tokens.cached.samples, summary.attempts, summary.tokens.reasoning.samples, summary.attempts), cx)))
            .child(note(format!("Saved measurements as of {} · {}. Unavailable measurements are shown as —.",
                report.query.as_of.with_timezone(&report.query.timezone).format("%Y-%m-%d %H:%M:%S"), report.query.timezone), cx))
            .into_any_element()
    }

    fn submitted_inputs(&self, report: &AnalysisReport, cx: &Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let repeat_rate = if report.summary.new_lookups > 0 {
            format!(
                "{:.1}%",
                report.repeated_submissions as f64 / report.summary.new_lookups as f64 * 100.
            )
        } else {
            "—".into()
        };
        panel(
            "Most submitted inputs",
            "Top 5 by new lookup submissions in this date range and filter.",
            cx,
        )
        .child(
            div()
                .id("most-submitted-inputs")
                .role(Role::List)
                .aria_label("Most submitted inputs")
                .flex()
                .flex_col()
                .mt_3()
                .children(report.top_inputs.iter().enumerate().map(|(i, input)| {
                    let count = input.metrics.new_lookups;
                    let count_label = if count == 1 { "lookup" } else { "lookups" };
                    div()
                        .id(("submitted-input", i))
                        .role(Role::ListItem)
                        .aria_label(format!(
                            "{}. {} · {} · {count} new {count_label}",
                            i + 1,
                            input.input,
                            input.direction.label(),
                        ))
                        .flex()
                        .items_center()
                        .gap_3()
                        .w_full()
                        .min_w_0()
                        .h(px(64.))
                        .px_3()
                        .text_left()
                        .when(i > 0, |s| s.border_t_1().border_color(theme.border))
                        .child(
                            div()
                                .w_6()
                                .flex_shrink_0()
                                .text_sm()
                                .text_color(theme.muted_foreground)
                                .child((i + 1).to_string()),
                        )
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .child(
                                    div()
                                        .text_sm()
                                        .font_weight(FontWeight::MEDIUM)
                                        .truncate()
                                        .child(
                                            input
                                                .input
                                                .split_whitespace()
                                                .collect::<Vec<_>>()
                                                .join(" "),
                                        ),
                                )
                                .child(note(input.direction.label(), cx).mt_1()),
                        )
                        .child(
                            div()
                                .flex_shrink_0()
                                .text_right()
                                .child(
                                    div()
                                        .text_base()
                                        .font_weight(FontWeight::SEMIBOLD)
                                        .child(count.to_string()),
                                )
                                .child(note(count_label, cx)),
                        )
                })),
        )
        .child(
            div()
                .flex()
                .items_center()
                .flex_wrap()
                .gap_2()
                .mt_3()
                .pt_3()
                .border_t_1()
                .border_color(theme.border)
                .child(note("Repeat submissions", cx))
                .child(
                    div()
                        .text_sm()
                        .font_weight(FontWeight::SEMIBOLD)
                        .child(repeat_rate),
                )
                .child(note(
                    format!(
                        "· {} of {} lookups",
                        report.repeated_submissions, report.summary.new_lookups
                    ),
                    cx,
                )),
        )
        .child(note("Cached opens and regenerated versions are excluded.", cx).mt_1())
        .into_any_element()
    }
}

impl Render for AnalysisView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let today = Utc::now()
            .with_timezone(&self.state.query.timezone)
            .date_naive();
        div().flex().flex_col().size_full().min_h_0().px_6().pt_5()
            .child(div().flex().items_center().gap_2().flex_shrink_0()
                .child(super::icon_button("back-from-analysis", IconName::ChevronLeft, "Back to dictionary (⌘L)")
                    .on_click(cx.listener(|_, _, _, cx| cx.emit(AnalysisEvent::Back))))
                .child(div().flex_1().text_2xl().font_weight(FontWeight::SEMIBOLD).child("Analysis"))
                .when(self.state.loading, |s| s.child(Spinner::new().small()))
                .child(super::icon_button("refresh-analysis", IconName::RefreshCw, "Refresh analysis").disabled(self.state.loading)
                    .on_click(cx.listener(|this, _, _, cx| this.refresh(cx)))))
            .child(note("Your dictionary activity, measured from saved requests.", cx))
            .child(div().flex().items_center().flex_wrap().gap_2().mt_4().flex_shrink_0()
                .child(Button::new("analysis-seven-days").small().ghost().label("7 days")
                    .on_click(cx.listener(|this, _, window, cx| this.recent(7, window, cx))))
                .child(Button::new("analysis-thirty-days").small().ghost().label("30 days")
                    .on_click(cx.listener(|this, _, window, cx| this.recent(30, window, cx))))
                .child(DatePicker::new(&self.dates).small().w(px(260.)).cleanable(false)
                    .presets(vec![DateRangePreset::range("Last 7 days", today - chrono::Days::new(6), today), DateRangePreset::range("Last 30 days", today - chrono::Days::new(29), today)]))
                .child(note(self.state.query.timezone.name(), cx)))
            .child(div().flex().items_center().flex_wrap().gap_2().mt_2().mb_4().flex_shrink_0()
                .child(select_control(&self.model, "Analysis model filter", 166.))
                .child(select_control(&self.effort, "Analysis effort filter", 120.))
                .child(select_control(&self.direction, "Analysis direction filter", 156.))
                .child(select_control(&self.outcome, "Analysis outcome filter", 134.))
                .when(self.state.query.filter != AnalysisFilter::default(), |s| s.child(Button::new("reset-analysis-filters").small().ghost().label("Reset")
                    .on_click(cx.listener(|this, _, window, cx| this.reset(window, cx))))))
            .child(div().id("analysis-scroll").flex_1().min_h_0().overflow_y_scroll().track_scroll(&self.scroll).pb_6()
                .when_some(self.timezone_error.clone().or_else(|| self.state.error.clone()), |s, error|
                    s.child(super::message_box(error, theme.danger, theme.border)))
                .when_some(self.state.report.clone(), |s, report| {
                    if report.summary.attempts == 0 {
                        s.child(div().flex().flex_col().items_center().justify_center().h(px(300.)).gap_3()
                            .child(Icon::new(IconName::ChartColumn).text_color(theme.muted_foreground))
                            .child(div().text_lg().child("No requests in this range"))
                            .child(note("Try another date range or filter. Your next lookup will appear here.", cx)))
                    } else { s.child(self.dashboard(&report, cx)) }
                })
                .when(self.state.report.is_none() && self.state.loading, |s| s.child(note("Reading saved measurements…", cx))))
    }
}

fn picker<T: Clone + PartialEq + 'static>(
    items: Vec<(T, String)>,
    window: &mut Window,
    cx: &mut App,
) -> Picker<T> {
    cx.new(|cx| {
        SelectState::new(
            items
                .into_iter()
                .map(|(value, label)| PickerItem {
                    value,
                    label: label.into(),
                })
                .collect(),
            Some(gpui_kit::component::IndexPath::new(0)),
            window,
            cx,
        )
    })
}

fn select_control<T: Clone + PartialEq + 'static>(
    state: &Picker<T>,
    label: &'static str,
    width: f32,
) -> Div {
    div().w(px(width)).h(px(28.)).flex_shrink_0().child(
        Select::new(state)
            .small()
            .w_full()
            .accessibility_label(label),
    )
}

fn string_items(
    label: &str,
    values: &[String],
    selected: &Option<String>,
) -> Vec<PickerItem<Option<String>>> {
    std::iter::once(PickerItem {
        value: None,
        label: label.to_owned().into(),
    })
    .chain(values.iter().map(|value| PickerItem {
        value: Some(value.clone()),
        label: known(value).into(),
    }))
    .chain(
        selected
            .iter()
            .filter(|value| !values.contains(value))
            .map(|value| PickerItem {
                value: Some(value.clone()),
                label: known(value).into(),
            }),
    )
    .collect()
}

fn known(value: &str) -> String {
    if value.is_empty() {
        "Unknown".into()
    } else {
        value.into()
    }
}

fn status_label(status: &AttemptStatus) -> &'static str {
    if *status == AttemptStatus::Running {
        "Running"
    } else {
        status.label()
    }
}

fn day_label(date: NaiveDate, complete: bool) -> String {
    format!(
        "{}{}",
        date.format("%y-%m-%d"),
        if complete { "" } else { "*" }
    )
}

fn token_metric(label: &'static str, value: Option<u128>, cx: &App) -> Div {
    div()
        .flex()
        .items_center()
        .gap_1()
        .child(note(label, cx))
        .child(
            super::token_count(
                SharedString::from(format!("analysis-tokens-{label}")),
                value,
            )
            .text_xs()
            .font_weight(FontWeight::MEDIUM),
        )
}
fn millis(value: Option<f64>) -> String {
    value
        .map(|ms| format!("{:.1} s", ms / 1000.))
        .unwrap_or_else(|| "—".into())
}

fn note(text: impl Into<SharedString>, cx: &App) -> Div {
    div()
        .text_xs()
        .text_color(cx.theme().muted_foreground)
        .child(text.into())
}

fn stat(title: &str, value: impl IntoElement, detail: String, cx: &App) -> Div {
    div()
        .flex_1()
        .min_w(px(130.))
        .p_4()
        .rounded_lg()
        .border_1()
        .border_color(cx.theme().border)
        .child(note(title.to_owned(), cx))
        .child(
            div()
                .text_2xl()
                .font_weight(FontWeight::SEMIBOLD)
                .my_2()
                .child(value),
        )
        .child(note(detail, cx))
}

fn panel(title: &str, description: &str, cx: &App) -> Div {
    div()
        .flex_1()
        .min_w(px(290.))
        .p_4()
        .rounded_lg()
        .border_1()
        .border_color(cx.theme().border)
        .child(
            div()
                .font_weight(FontWeight::SEMIBOLD)
                .mb_1()
                .child(title.to_owned()),
        )
        .child(note(description.to_owned(), cx))
}

#[derive(Clone)]
struct ChartPoint {
    label: String,
    value: Option<f64>,
    detail: String,
    color: Hsla,
}

fn chart(id: &'static str, data: Vec<ChartPoint>) -> Div {
    if data.iter().all(|point| point.value.is_none()) {
        return div()
            .h(px(185.))
            .flex()
            .items_center()
            .justify_center()
            .text_sm()
            .opacity(0.6)
            .child("No reported measurements in this range");
    }
    div().h(px(185.)).px_3().mt_3().mb_2().child(
        BarChart::new(data)
            .id(id)
            .band(|point| point.label.clone())
            // An absent value has no bar; its tooltip and accompanying sample
            // counts say unavailable. Never interpolate across a missing day.
            .value(|point| point.value.unwrap_or(0.))
            .fill(|point, _, _, _| point.color)
            .tooltip_value(|point, _| point.detail.clone().into())
            .value_axis(true)
            .value_tick_count(3)
            .band_tick_count(3)
            .value_tick_format(|value| formatting::compact_count(Some(value.round() as u128))),
    )
}
