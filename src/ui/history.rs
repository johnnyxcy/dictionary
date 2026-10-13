use std::collections::HashSet;

use dictionary::{
    direction::Direction,
    history::{HistoryFilter, HistoryQuery, HistoryRow, HistoryState},
    model::{AttemptStatus, ServiceTier},
    service::Event,
};
use gpui_kit::{
    assets::IconName,
    component::{
        ActiveTheme, Disableable, Icon, Sizable,
        button::{Button, ButtonVariants},
        input::{Input, InputEvent, InputState},
        select::{Select, SelectEvent, SelectState},
        spinner::Spinner,
    },
    prelude::FluentBuilder,
    *,
};

use super::settings::PickerItem;

pub enum HistoryEvent {
    Query {
        request_id: u64,
        query: HistoryQuery,
    },
    Open(String),
    Delete(String),
    Back,
}

pub struct HistoryView {
    state: HistoryState,
    search: Entity<InputState>,
    direction: Entity<SelectState<Vec<PickerItem<Option<Direction>>>>>,
    status: Entity<SelectState<Vec<PickerItem<Option<AttemptStatus>>>>>,
    scroll: ScrollHandle,
    visible: bool,
    deleting: bool,
    active: HashSet<String>,
    unsaved: HashSet<String>,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<HistoryEvent> for HistoryView {}

impl HistoryView {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let search = cx.new(|cx| InputState::new(window, cx).placeholder("Search history…"));
        let direction = cx.new(|cx| {
            SelectState::new(
                vec![
                    PickerItem {
                        value: None,
                        label: "All directions".into(),
                    },
                    PickerItem {
                        value: Some(Direction::EnZh),
                        label: Direction::EnZh.label().into(),
                    },
                    PickerItem {
                        value: Some(Direction::ZhEn),
                        label: Direction::ZhEn.label().into(),
                    },
                ],
                Some(gpui_kit::component::IndexPath::new(0)),
                window,
                cx,
            )
        });
        let status: Entity<SelectState<Vec<PickerItem<Option<AttemptStatus>>>>> = cx.new(|cx| {
            SelectState::new(
                std::iter::once(PickerItem {
                    value: None,
                    label: "All outcomes".into(),
                })
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
                    .map(|status| PickerItem {
                        label: if status == AttemptStatus::Running {
                            "Running".into()
                        } else {
                            status.label().into()
                        },
                        value: Some(status),
                    }),
                )
                .collect(),
                Some(gpui_kit::component::IndexPath::new(0)),
                window,
                cx,
            )
        });
        let subscriptions = vec![
            cx.subscribe_in(&search, window, |this: &mut Self, input, event, _, cx| {
                if matches!(event, InputEvent::Change) {
                    let mut filter = this.state.query.filter.clone();
                    filter.search = input.read(cx).value().to_string();
                    this.filter(filter, cx);
                }
            }),
            cx.subscribe_in(&direction, window, |this: &mut Self, _, event, _, cx| {
                if let SelectEvent::Confirm(Some(direction)) = event {
                    let mut filter = this.state.query.filter.clone();
                    filter.direction = *direction;
                    this.filter(filter, cx);
                }
            }),
            cx.subscribe_in(&status, window, |this: &mut Self, _, event, _, cx| {
                if let SelectEvent::Confirm(Some(status)) = event {
                    let mut filter = this.state.query.filter.clone();
                    filter.status = status.clone();
                    this.filter(filter, cx);
                }
            }),
        ];
        Self {
            state: HistoryState::default(),
            search,
            direction,
            status,
            scroll: ScrollHandle::new(),
            visible: false,
            deleting: false,
            active: HashSet::new(),
            unsaved: HashSet::new(),
            _subscriptions: subscriptions,
        }
    }

    pub fn show(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.visible = true;
        if self.state.needs_refresh() {
            self.refresh(cx);
        }
        self.search.update(cx, |input, cx| input.focus(window, cx));
        cx.notify();
    }

    pub fn hide(&mut self) {
        self.visible = false;
    }

    pub fn set_deleting(&mut self, deleting: bool, cx: &mut Context<Self>) {
        if self.deleting != deleting {
            self.deleting = deleting;
            cx.notify();
        }
    }

    fn refresh(&mut self, cx: &mut Context<Self>) {
        let (request_id, query) = self.state.request();
        cx.emit(HistoryEvent::Query { request_id, query });
        cx.notify();
    }

    pub fn fail(&mut self, request_id: u64, error: String, cx: &mut Context<Self>) {
        self.state.fail(request_id, error);
        cx.notify();
    }

    fn filter(&mut self, filter: HistoryFilter, cx: &mut Context<Self>) {
        if !self.state.filter(filter) {
            return;
        }
        self.scroll.set_offset(point(px(0.), px(0.)));
        self.refresh(cx);
    }

    fn reset(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.search
            .update(cx, |input, cx| input.set_value("", window, cx));
        self.direction
            .update(cx, |state, cx| state.set_selected_value(&None, window, cx));
        self.status
            .update(cx, |state, cx| state.set_selected_value(&None, window, cx));
        self.filter(HistoryFilter::default(), cx);
    }

    pub fn receive(&mut self, event: &Event, cx: &mut Context<Self>) {
        match event {
            Event::HistoryPage { request_id, page } => {
                if self.state.receive(*request_id, page.clone())
                    && self.visible
                    && self.state.needs_refresh()
                {
                    self.refresh(cx);
                }
            }
            Event::HistoryFailed {
                request_id,
                message,
            } => self.state.fail(*request_id, message.clone()),
            Event::HistoryChanged => {
                self.state.invalidate();
                if self.visible && self.state.needs_refresh() {
                    self.refresh(cx);
                }
            }
            Event::Opened { attempt, .. } | Event::Started(attempt) => {
                if attempt.status.is_active() {
                    self.active.insert(attempt.lookup_id.clone());
                }
            }
            Event::Finished {
                attempt,
                save_error,
            } => {
                self.active.remove(&attempt.lookup_id);
                if save_error.is_some() {
                    self.unsaved.insert(attempt.id.clone());
                } else {
                    self.unsaved.remove(&attempt.id);
                }
            }
            Event::Progress(progress) => {
                if let Some(row) = self
                    .state
                    .page
                    .rows
                    .iter_mut()
                    .find(|row| row.entry.id == progress.attempt_id)
                {
                    row.entry.translation = dictionary::model::translation_preview_for(
                        &progress.answer,
                        row.entry.direction,
                    );
                    row.entry.generating = !progress.answer.is_empty();
                    row.usage = progress.metadata.usage.clone();
                }
            }
            Event::DeletedLookup(id) => {
                self.state
                    .page
                    .rows
                    .retain(|row| &row.entry.lookup_id != id);
            }
            _ => {}
        }
        cx.notify();
    }

    fn row(&self, row: &HistoryRow, cx: &Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let entry = &row.entry;
        let id = entry.id.clone();
        let keyboard_id = id.clone();
        let lookup_id = entry.lookup_id.clone();
        let disabled = self.deleting || row.lookup_active || self.active.contains(&lookup_id);
        let unsaved = self.unsaved.contains(&entry.id);
        let failed = matches!(
            entry.status,
            AttemptStatus::Failed | AttemptStatus::TimedOut | AttemptStatus::Interrupted
        );
        let status = if unsaved {
            Some("Not saved")
        } else if entry.status != AttemptStatus::Succeeded {
            Some(entry.status.label())
        } else {
            None
        };
        let preview = status
            .map(|status| {
                if entry.translation.is_empty() {
                    status.into()
                } else {
                    format!("{status} · {}", entry.translation)
                }
            })
            .unwrap_or_else(|| entry.translation.clone());
        div()
            .id(SharedString::from(format!("saved-attempt-{}", entry.id)))
            .group("saved-attempt-row")
            .role(Role::Button)
            .tab_index(0)
            .focus_visible(|style| style.bg(theme.accent).border_2().border_color(theme.ring))
            .aria_label(format!(
                "Open {} · version {} · {}",
                entry.input,
                row.version,
                entry.status.label()
            ))
            .flex()
            .items_center()
            .gap_3()
            .px_4()
            .py_3()
            .w_full()
            .min_h(px(64.))
            .border_b_1()
            .border_color(theme.border)
            .cursor_pointer()
            .hover(|this| this.bg(theme.accent))
            .on_click(cx.listener(move |_, _, _, cx| cx.emit(HistoryEvent::Open(id.clone()))))
            .on_key_down(cx.listener(move |_, event: &KeyDownEvent, _, cx| {
                if matches!(event.keystroke.key.as_str(), "enter" | "space") {
                    cx.stop_propagation();
                    cx.emit(HistoryEvent::Open(keyboard_id.clone()));
                }
            }))
            .child(
                div()
                    .flex_1()
                    .min_w(px(180.))
                    .overflow_hidden()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(
                                div()
                                    .min_w_0()
                                    .truncate()
                                    .font_weight(FontWeight::MEDIUM)
                                    .text_sm()
                                    .child(
                                        entry
                                            .input
                                            .split_whitespace()
                                            .collect::<Vec<_>>()
                                            .join(" "),
                                    ),
                            )
                            .when(row.version > 1, |this| {
                                this.child(
                                    div()
                                        .text_xs()
                                        .text_color(theme.muted_foreground)
                                        .flex_shrink_0()
                                        .child(format!("v{}", row.version)),
                                )
                            }),
                    )
                    .child(
                        div()
                            .mt_1()
                            .text_xs()
                            .truncate()
                            .text_color(if failed || unsaved {
                                theme.danger
                            } else if entry.status.is_active() {
                                theme.info
                            } else {
                                theme.muted_foreground
                            })
                            .child(preview),
                    ),
            )
            .child(cell(entry.direction.label().into(), 90., cx))
            .child(
                div()
                    .w(px(122.))
                    .flex_shrink_0()
                    .overflow_hidden()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(div().truncate().child(row.model.clone()))
                    .child(div().mt_1().child(format!(
                        "{}{}",
                        row.effort,
                        if row.tier == Some(ServiceTier::Fast) {
                            " · fast"
                        } else {
                            ""
                        }
                    ))),
            )
            .child(
                super::token_count(
                    SharedString::from(format!("history-tokens-{}", entry.id)),
                    row.usage.total().map(u128::from),
                )
                .w(px(72.))
                .flex_shrink_0()
                .text_xs()
                .text_color(theme.muted_foreground),
            )
            .child(cell(super::duration(row.duration_ms), 64., cx))
            .child(cell(history_date(&entry.created_at), 76., cx))
            .child(
                div()
                    .size(px(26.))
                    .flex_shrink_0()
                    .invisible()
                    .when(!disabled, |this| {
                        this.group_hover("saved-attempt-row", |this| this.visible())
                            .child(
                                super::icon_button(
                                    "delete-saved-lookup",
                                    IconName::Trash,
                                    "Delete lookup and all versions",
                                )
                                .xsmall()
                                .on_click(cx.listener(
                                    move |_, _, _, cx| {
                                        cx.stop_propagation();
                                        cx.emit(HistoryEvent::Delete(lookup_id.clone()));
                                    },
                                )),
                            )
                    }),
            )
            .into_any_element()
    }
}

impl Render for HistoryView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        div()
            .flex()
            .flex_col()
            .size_full()
            .min_h_0()
            .px_6()
            .pt_6()
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(
                        super::icon_button(
                            "back-from-history",
                            IconName::ChevronLeft,
                            "Back to dictionary (⌘L)",
                        )
                        .on_click(cx.listener(|_, _, _, cx| cx.emit(HistoryEvent::Back))),
                    )
                    .child(
                        div()
                            .text_2xl()
                            .font_weight(FontWeight::SEMIBOLD)
                            .child("History"),
                    ),
            )
            .child(
                div()
                    .mt_2()
                    .text_sm()
                    .text_color(theme.muted_foreground)
                    .child("Every word worth coming back to."),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .mt_5()
                    .mb_4()
                    .child(
                        div().flex_1().min_w_0().child(
                            Input::new(&self.search)
                                .small()
                                .w_full()
                                .cleanable(true)
                                .prefix(Icon::new(IconName::Search).small())
                                .aria_label("Search history"),
                        ),
                    )
                    .child(
                        div().w(px(138.)).h(px(28.)).flex_shrink_0().child(
                            Select::new(&self.direction)
                                .small()
                                .w_full()
                                .accessibility_label("History direction"),
                        ),
                    )
                    .child(
                        div().w(px(138.)).h(px(28.)).flex_shrink_0().child(
                            Select::new(&self.status)
                                .small()
                                .w_full()
                                .accessibility_label("History outcome"),
                        ),
                    )
                    .child(
                        super::icon_button(
                            "refresh-history",
                            IconName::RefreshCw,
                            "Refresh history",
                        )
                        .disabled(self.state.loading)
                        .on_click(cx.listener(|this, _, _, cx| this.refresh(cx))),
                    )
                    .when(!self.state.query.filter.is_empty(), |this| {
                        this.child(
                            super::icon_button(
                                "reset-history",
                                IconName::X,
                                "Clear history filters",
                            )
                            .on_click(cx.listener(|this, _, window, cx| this.reset(window, cx))),
                        )
                    }),
            )
            .when_some(self.state.error.clone(), |this, error| {
                this.child(super::message_box(error, theme.danger, theme.border))
            })
            .child(
                div()
                    .id("history-table")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .overflow_x_scroll()
                    .track_scroll(&self.scroll)
                    .border_1()
                    .border_color(theme.border)
                    .rounded_lg()
                    .child(
                        div()
                            .min_w(px(810.))
                            .w_full()
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap_3()
                                    .px_4()
                                    .py_3()
                                    .border_b_1()
                                    .border_color(theme.border)
                                    .bg(theme.sidebar)
                                    .child(
                                        div()
                                            .flex_1()
                                            .min_w(px(180.))
                                            .text_xs()
                                            .text_color(theme.muted_foreground)
                                            .child("Input & translation"),
                                    )
                                    .child(cell("Direction".into(), 90., cx))
                                    .child(cell("Model & effort".into(), 122., cx))
                                    .child(cell("Tokens".into(), 72., cx))
                                    .child(cell("Time".into(), 64., cx))
                                    .child(cell("Date".into(), 76., cx))
                                    .child(div().w(px(26.)).flex_shrink_0()),
                            )
                            .children(self.state.page.rows.iter().map(|row| self.row(row, cx))),
                    )
                    .when(self.state.page.rows.is_empty(), |this| {
                        this.child(
                            div()
                                .flex()
                                .flex_col()
                                .items_center()
                                .justify_center()
                                .gap_3()
                                .py_12()
                                .text_color(theme.muted_foreground)
                                .when(self.state.loading, |this| this.child(Spinner::new()))
                                .when(!self.state.loading, |this| {
                                    this.child(Icon::new(IconName::Search).size_6())
                                })
                                .child(if self.state.loading {
                                    "Searching saved entries…"
                                } else if self.state.error.is_some() {
                                    "History is unavailable. Try Refresh."
                                } else if self.state.query.filter.is_empty() {
                                    "Your saved entries will appear here."
                                } else {
                                    "No saved entries match your search."
                                })
                                .when(!self.state.query.filter.is_empty(), |this| {
                                    this.child(
                                        Button::new("clear-empty-history")
                                            .small()
                                            .ghost()
                                            .label("Clear filters")
                                            .on_click(cx.listener(|this, _, window, cx| {
                                                this.reset(window, cx)
                                            })),
                                    )
                                }),
                        )
                    }),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_3()
                    .h(px(56.))
                    .flex_shrink_0()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .when(self.state.loading, |this| {
                                this.child(Spinner::new().xsmall())
                            })
                            .child(format!(
                                "{} attempts · {} lookups",
                                self.state.page.attempts, self.state.page.lookups
                            )),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_3()
                            .child(
                                Button::new("previous-history-page")
                                    .small()
                                    .icon(IconName::ChevronLeft)
                                    .label("Prev")
                                    .accessibility_label("Previous history page")
                                    .disabled(!self.state.can_previous())
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        if this.state.previous_page() {
                                            this.scroll.set_offset(point(px(0.), px(0.)));
                                            this.refresh(cx);
                                        }
                                    })),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(theme.muted_foreground)
                                    .child(format!("Page {}", self.state.page_number())),
                            )
                            .child(
                                Button::new("next-history-page")
                                    .small()
                                    .label("Next")
                                    .icon(IconName::ChevronRight)
                                    .accessibility_label("Next history page")
                                    .disabled(!self.state.can_next())
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        if this.state.next_page() {
                                            this.scroll.set_offset(point(px(0.), px(0.)));
                                            this.refresh(cx);
                                        }
                                    })),
                            ),
                    ),
            )
    }
}

fn cell(text: String, width: f32, cx: &App) -> impl IntoElement {
    div()
        .w(px(width))
        .flex_shrink_0()
        .text_xs()
        .text_color(cx.theme().muted_foreground)
        .truncate()
        .child(text)
}

fn history_date(value: &str) -> String {
    chrono::DateTime::parse_from_rfc3339(value)
        .map(|date| {
            date.with_timezone(&chrono::Local)
                .format("%b %d")
                .to_string()
        })
        .unwrap_or_else(|_| value.into())
}
