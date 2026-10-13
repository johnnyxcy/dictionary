mod analysis;
mod formatting;
mod history;
mod settings;
use analysis::{AnalysisEvent, AnalysisView};
use history::{HistoryEvent, HistoryView};
use settings::{PickerItem, SettingsEvent, SettingsView};

use dictionary::{
    direction::DirectionChoice,
    model::{Activity, Attempt, AttemptStatus, ServiceTier, Setup},
    service::{Command, Event, Service},
    settings::SettingsState,
    view_state::LookupState,
};
use gpui_kit::assets::IconName;
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::{
    component::{
        ActiveTheme, Disableable, Icon, Selectable, Sizable, WindowExt,
        button::{Button, ButtonVariants},
        input::{Textarea, TextareaState},
        notification::{Notification, NotificationList},
        popover::Popover,
        select::{Select, SelectEvent, SelectState},
        skeleton::Skeleton,
        spinner::Spinner,
        text::TextView,
        tooltip::Tooltip,
    },
    *,
};

actions!(
    dictionary,
    [
        FocusLookup,
        FocusHistory,
        FocusAnalysis,
        ToggleSidebar,
        OpenSettings,
        Quit
    ]
);

struct SettingsNotification;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Page {
    Dictionary,
    History,
    Analysis,
}

pub struct DictionaryView {
    input: Entity<TextareaState>,
    direction: DirectionChoice,
    direction_picker: Entity<SelectState<Vec<PickerItem<DirectionChoice>>>>,
    settings_state: SettingsState,
    settings_view: Option<Entity<SettingsView>>,
    settings_subscription: Option<Subscription>,
    notifications: Entity<NotificationList>,
    _direction_subscription: Subscription,
    service: Option<Service>,
    setup: Option<Setup>,
    connecting: bool,
    setup_error: Option<String>,
    notice: Option<String>,
    lookups: LookupState,
    history_view: Entity<HistoryView>,
    analysis_view: Entity<AnalysisView>,
    page: Page,
    return_to_history: bool,
    _history_subscription: Subscription,
    _analysis_subscription: Subscription,
    deleting: Option<String>,
    activity_scroll: ScrollHandle,
    history_scroll: ScrollHandle,
    sidebar_visible: bool,
    _subscription: Subscription,
    _events: Task<()>,
    _quit: Subscription,
}

impl DictionaryView {
    pub fn new(
        service: Service,
        events: async_channel::Receiver<Event>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let input = cx.new(|cx| {
            TextareaState::new(window, cx)
                .auto_grow(1, 4)
                .submit_on_enter(true)
                .placeholder("Look up a word, phrase, or sentence…")
        });
        let direction_picker = cx.new(|cx| {
            SelectState::new(
                direction_items(""),
                Some(gpui_kit::component::IndexPath::new(0)),
                window,
                cx,
            )
        });
        let direction_subscription = cx.subscribe_in(
            &direction_picker,
            window,
            |this: &mut Self, _, event, _, cx| {
                if let SelectEvent::Confirm(Some(direction)) = event {
                    this.direction = *direction;
                    this.notice = None;
                    cx.notify();
                }
            },
        );
        let subscription = cx.subscribe_in(&input, window, |this: &mut Self, _, _, window, cx| {
            this.sync_direction(window, cx);
            cx.notify();
        });
        let history_view = cx.new(|cx| HistoryView::new(window, cx));
        let history_subscription = cx.subscribe_in(
            &history_view,
            window,
            |this: &mut Self, view, event, window, cx| match event {
                HistoryEvent::Query { request_id, query } => {
                    let result = this.dispatch(Command::QueryHistory {
                        request_id: *request_id,
                        query: query.clone(),
                    });
                    if let Err(error) = result {
                        view.update(cx, |view, cx| view.fail(*request_id, error, cx));
                    }
                }
                HistoryEvent::Open(id) => this.open_attempt(id.clone(), true, cx),
                HistoryEvent::Delete(id) => this.delete_lookup(id.clone(), cx),
                HistoryEvent::Back => this.show_dictionary(window, cx),
            },
        );
        let analysis_view = cx.new(|cx| AnalysisView::new(window, cx));
        let analysis_subscription = cx.subscribe_in(
            &analysis_view,
            window,
            |this: &mut Self, view, event, window, cx| match event {
                AnalysisEvent::Query { request_id, query } => {
                    if let Err(error) = this.dispatch(Command::QueryAnalysis {
                        request_id: *request_id,
                        query: query.clone(),
                    }) {
                        view.update(cx, |view, cx| view.fail(*request_id, error, cx));
                    }
                }
                AnalysisEvent::Back => this.show_dictionary(window, cx),
            },
        );
        let task = cx.spawn_in(window, async move |view, cx| {
            while let Ok(event) = events.recv().await {
                if cx
                    .update(|window, cx| {
                        view.update(cx, |view, cx| {
                            view.receive(event, window, cx);
                            cx.notify();
                        })
                    })
                    .and_then(|result| result)
                    .is_err()
                {
                    break;
                }
            }
        });
        input.update(cx, |input, cx| input.focus(window, cx));
        let quit = cx.on_app_quit(|this, _| {
            this.service.take();
            async {}
        });
        Self {
            input,
            direction: DirectionChoice::Auto,
            direction_picker,
            settings_state: SettingsState::default(),
            settings_view: None,
            settings_subscription: None,
            notifications: cx.new(|cx| NotificationList::new(window, cx)),
            _direction_subscription: direction_subscription,
            service: Some(service),
            setup: None,
            connecting: true,
            setup_error: None,
            notice: None,
            lookups: LookupState::default(),
            history_view,
            analysis_view,
            page: Page::Dictionary,
            return_to_history: false,
            _history_subscription: history_subscription,
            _analysis_subscription: analysis_subscription,
            deleting: None,
            activity_scroll: ScrollHandle::new(),
            history_scroll: ScrollHandle::new(),
            sidebar_visible: true,
            _subscription: subscription,
            _events: task,
            _quit: quit,
        }
    }

    fn dispatch(&self, command: Command) -> Result<(), String> {
        self.service
            .as_ref()
            .ok_or_else(|| "Background worker unavailable".to_owned())?
            .send(command)
            .map_err(|error| error.to_string())
    }

    fn send(&mut self, command: Command) -> bool {
        match self.dispatch(command) {
            Ok(()) => true,
            Err(error) => {
                self.notice = Some(error);
                false
            }
        }
    }

    fn submit(&mut self, cx: &mut Context<Self>) {
        let input = self.input.read(cx).value().to_string();
        if input.trim().is_empty() {
            return;
        }
        self.notice = None;
        self.return_to_history = false;
        let request_id = self.lookups.begin_navigation();
        self.send(Command::Lookup {
            request_id,
            input,
            direction: self.direction,
        });
        cx.notify();
    }

    fn regenerate(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(attempt) = &self.lookups.selected else {
            return;
        };
        if attempt.status.is_active()
            || self.setup.is_none()
            || self.deleting.as_ref() == Some(&attempt.lookup_id)
        {
            return;
        }
        let attempt_id = attempt.id.clone();
        let input = attempt.input.clone();
        self.direction = attempt.requested_direction;
        self.sync_direction(window, cx);
        self.input
            .update(cx, |state, cx| state.set_value(input, window, cx));
        self.notice = None;
        let request_id = self.lookups.begin_navigation();
        self.send(Command::Regenerate {
            request_id,
            attempt_id,
        });
        cx.notify();
    }

    fn delete_lookup(&mut self, lookup_id: String, cx: &mut Context<Self>) {
        if self.deleting.is_some() || self.lookups.lookup_active(&lookup_id) {
            return;
        }
        self.notice = None;
        if self.send(Command::DeleteLookup(lookup_id.clone())) {
            self.deleting = Some(lookup_id);
            self.history_view
                .update(cx, |view, cx| view.set_deleting(true, cx));
        }
        cx.notify();
    }

    fn receive(&mut self, event: Event, window: &mut Window, cx: &mut Context<Self>) {
        let selected_progress = matches!(&event, Event::Progress(progress)
            if self.lookups.selected.as_ref().is_some_and(|a| a.id == progress.attempt_id));
        let navigation = matches!(&event, Event::Opened { request_id, .. }
            if self.lookups.accepts_response(*request_id));
        self.lookups.receive(&event);
        self.history_view
            .update(cx, |view, cx| view.receive(&event, cx));
        self.analysis_view
            .update(cx, |view, cx| view.receive(&event, window, cx));
        if navigation {
            self.page = Page::Dictionary;
            self.history_view.update(cx, |view, _| view.hide());
            self.analysis_view.update(cx, |view, _| view.hide());
            if let Some(index) = self.lookups.history.iter().position(|entry| {
                self.lookups
                    .selected
                    .as_ref()
                    .is_some_and(|a| a.id == entry.id)
            }) {
                self.history_scroll.scroll_to_item(index);
            }
            self.activity_scroll.set_offset(point(px(0.), px(0.)));
        } else if selected_progress && self.lookups.activity_expanded {
            self.activity_scroll.scroll_to_bottom();
        }
        match event {
            Event::Connecting => {
                self.connecting = true;
                self.setup_error = None;
                if let Some(view) = &self.settings_view {
                    view.update(cx, |view, cx| view.set_refreshing(cx));
                }
            }
            Event::Settings(state) => {
                self.settings_state = *state;
                if let Some(view) = &self.settings_view {
                    view.update(cx, |view, cx| {
                        view.update_state(self.settings_state.clone(), window, cx)
                    });
                }
            }
            Event::SettingsSaved => {
                if let Some(view) = &self.settings_view {
                    view.update(cx, |view, cx| view.finish_save(cx));
                }
                self.push_notification(
                    Notification::success("New lookups will use these settings.")
                        .title("Settings saved")
                        .id::<SettingsNotification>()
                        .placement(Anchor::BottomRight),
                    window,
                    cx,
                );
            }
            Event::SettingsFailed(error) => {
                if let Some(view) = &self.settings_view {
                    view.update(cx, |view, cx| view.finish_save(cx));
                }
                self.push_notification(
                    Notification::error(error)
                        .title("Could not save settings")
                        .id::<SettingsNotification>()
                        .placement(Anchor::BottomRight)
                        .autohide(false),
                    window,
                    cx,
                );
            }
            Event::Ready(setup) => {
                self.setup = Some(setup);
                self.connecting = false;
                self.setup_error = None;
            }
            Event::SetupFailed(message) => {
                self.setup = None;
                self.connecting = false;
                self.setup_error = Some(message);
            }
            Event::RequestFailed {
                request_id,
                message,
            } => {
                if self.lookups.accepts_response(request_id) {
                    self.notice = Some(message);
                }
            }
            Event::DeletedLookup(lookup_id) => {
                if self.deleting.as_ref() == Some(&lookup_id) {
                    self.deleting = None;
                }
            }
            Event::DeleteFailed { lookup_id, message } => {
                if self.deleting.as_ref() == Some(&lookup_id) {
                    self.deleting = None;
                }
                self.notice = Some(message);
            }
            Event::Finished {
                attempt,
                save_error: Some(_),
                ..
            } => {
                if !self
                    .lookups
                    .selected
                    .as_ref()
                    .is_some_and(|a| a.id == attempt.id)
                {
                    self.notice = Some(format!(
                        "Could not save “{}”. Open its history entry to copy the result.",
                        attempt.input
                    ));
                }
            }
            Event::Notice(message) => self.notice = Some(message),
            Event::Fatal(message) => {
                self.notice = Some(message);
                self.connecting = false;
                self.setup = None;
                self.deleting = None;
            }
            _ => {}
        }
        self.history_view.update(cx, |view, cx| {
            view.set_deleting(self.deleting.is_some(), cx)
        });
    }

    fn new_lookup(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.page = Page::Dictionary;
        self.return_to_history = false;
        self.history_view.update(cx, |view, _| view.hide());
        self.analysis_view.update(cx, |view, _| view.hide());
        self.lookups.new_lookup();
        self.notice = None;
        self.input.update(cx, |input, cx| {
            input.set_value("", window, cx);
            input.focus(window, cx);
        });
        cx.notify();
    }

    fn new_lookup_button(&self, cx: &Context<Self>) -> Button {
        icon_button("new-lookup", IconName::Plus, "New lookup")
            .on_click(cx.listener(|this, _, window, cx| this.new_lookup(window, cx)))
    }

    fn open_attempt(&mut self, attempt_id: String, from_history: bool, cx: &mut Context<Self>) {
        self.return_to_history = from_history;
        let request_id = self.lookups.begin_navigation();
        self.notice = None;
        self.send(Command::Open {
            request_id,
            attempt_id,
        });
        cx.notify();
    }

    fn open_history(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.lookups.begin_navigation();
        self.page = Page::History;
        self.analysis_view.update(cx, |view, _| view.hide());
        self.notice = None;
        self.history_view
            .update(cx, |view, cx| view.show(window, cx));
        cx.notify();
    }

    fn open_analysis(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.lookups.begin_navigation();
        self.page = Page::Analysis;
        self.notice = None;
        self.history_view.update(cx, |view, _| view.hide());
        self.analysis_view
            .update(cx, |view, cx| view.show(window, cx));
        cx.notify();
    }

    fn analysis_button(&self, cx: &Context<Self>) -> Button {
        icon_button("open-analysis", IconName::ChartColumn, "Analysis (⌘⇧A)")
            .selected(self.page == Page::Analysis)
            .on_click(cx.listener(|this, _, window, cx| this.open_analysis(window, cx)))
    }

    fn show_dictionary(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.lookups.begin_navigation();
        self.page = Page::Dictionary;
        self.history_view.update(cx, |view, _| view.hide());
        self.analysis_view.update(cx, |view, _| view.hide());
        self.input.update(cx, |input, cx| input.focus(window, cx));
        cx.notify();
    }

    fn configuration(&self) -> String {
        self.setup
            .as_ref()
            .map(|setup| {
                let fast = if setup.service_tier == Some(ServiceTier::Fast) {
                    " · fast"
                } else {
                    ""
                };
                format!("{} · {}{fast}", setup.model, setup.effort)
            })
            .unwrap_or_else(|| {
                if self.connecting {
                    "Connecting…".into()
                } else {
                    "Connection unavailable".into()
                }
            })
    }

    fn settings_button(&self, cx: &Context<Self>) -> Button {
        icon_button("settings", IconName::Settings, "Settings (⌘,)")
            .on_click(cx.listener(|this, _, window, cx| this.open_settings(window, cx)))
    }

    fn sync_direction(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let items = direction_items(&self.input.read(cx).value());
        self.direction_picker.update(cx, |state, cx| {
            state.set_items(items, window, cx);
            state.set_selected_value(&self.direction, window, cx);
        });
    }

    fn open_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if window.has_active_dialog(cx) {
            return;
        }
        let view = cx
            .new(|cx| SettingsView::new(self.settings_state.clone(), self.connecting, window, cx));
        self.settings_subscription = Some(cx.subscribe_in(
            &view,
            window,
            |this, view, event, window, cx| {
                let command = match event {
                    SettingsEvent::Save(preferences) => Command::SaveSettings(preferences.clone()),
                    SettingsEvent::Refresh => Command::Reconnect,
                };
                let result = this.dispatch(command);
                if let Err(error) = result {
                    let title = match event {
                        SettingsEvent::Save(_) => {
                            view.update(cx, |view, cx| view.finish_save(cx));
                            "Could not save settings"
                        }
                        SettingsEvent::Refresh => {
                            view.update(cx, |view, cx| view.refresh_failed(cx));
                            "Could not refresh settings"
                        }
                    };
                    this.push_notification(
                        Notification::error(error)
                            .title(title)
                            .id::<SettingsNotification>()
                            .placement(Anchor::BottomRight)
                            .autohide(false),
                        window,
                        cx,
                    );
                }
            },
        ));
        self.settings_view = Some(view.clone());
        window.open_dialog(cx, move |dialog, _, _| {
            let cancel_view = view.clone();
            let save_view = view.clone();
            dialog
                .title("Settings")
                .w(px(560.))
                .overlay_closable(false)
                .on_cancel(move |_, window, cx| {
                    cancel_view.update(cx, |view, cx| view.request_cancel(window, cx))
                })
                .on_ok(move |_, _, cx| {
                    save_view.update(cx, |view, cx| view.save(cx));
                    false
                })
                .child(view.clone())
        });
    }

    fn push_notification(
        &self,
        notification: Notification,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.notifications.update(cx, |list, cx| {
            list.push(notification, window, cx);
        });
    }

    fn notification_layer(&self, window: &Window) -> impl IntoElement {
        // GPUI Kit 0.7 dialogs paint at deferred priority 10 + layer, and
        // menus at POPUP_PRIORITY. Keep toasts above both, below tooltips.
        // Mount the list once; using WindowExt's default host would leave it
        // behind deferred dialogs, regardless of normal child ordering.
        deferred(
            anchored().position(point(px(0.), px(0.))).child(
                div()
                    .w(window.viewport_size().width)
                    .h(window.viewport_size().height)
                    .child(self.notifications.clone()),
            ),
        )
        .with_priority(gpui_kit::base::POPUP_PRIORITY + 1)
    }

    fn sidebar(&self, cx: &Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let selected = self.lookups.selected.as_ref().map(|attempt| &attempt.id);
        div()
            .flex()
            .flex_col()
            .w(px(232.))
            .h_full()
            .flex_shrink_0()
            .bg(theme.sidebar)
            .border_r_1()
            .border_color(theme.border)
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .h(px(60.))
                    .px_4()
                    .flex_shrink_0()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(
                                Icon::new(IconName::BookOpen)
                                    .small()
                                    .text_color(theme.muted_foreground),
                            )
                            .child(div().font_weight(FontWeight::SEMIBOLD).child("Dictionary")),
                    )
                    .child(self.new_lookup_button(cx)),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .px_4()
                    .h(px(36.))
                    .flex_shrink_0()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(Icon::new(IconName::Clock).xsmall())
                            .child("History"),
                    )
                    .child(
                        icon_button(
                            "open-full-history",
                            IconName::Ellipsis,
                            "Show all history (⌘F)",
                        )
                        .xsmall()
                        .size(px(24.))
                        .text_color(theme.muted_foreground)
                        .on_click(cx.listener(|this, _, window, cx| this.open_history(window, cx))),
                    ),
            )
            .child(
                div()
                    .id("history-list")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .track_scroll(&self.history_scroll)
                    .px_2()
                    .pb_2()
                    .when(self.lookups.history.is_empty(), |this| {
                        this.child(
                            div()
                                .px_2()
                                .py_3()
                                .text_sm()
                                .text_color(theme.muted_foreground)
                                .child("Your saved words will appear here."),
                        )
                    })
                    .children(self.lookups.history.iter().map(|entry| {
                        let id = entry.id.clone();
                        let lookup_id = entry.lookup_id.clone();
                        let delete_id = entry.lookup_id.clone();
                        let delete_disabled =
                            self.deleting.is_some() || self.lookups.lookup_active(&entry.lookup_id);
                        let active = entry.status.is_active();
                        let failed = self.lookups.save_error(&entry.id).is_some()
                            || matches!(
                                entry.status,
                                AttemptStatus::Failed
                                    | AttemptStatus::TimedOut
                                    | AttemptStatus::Interrupted
                            );
                        let status = if self.lookups.save_error(&entry.id).is_some() {
                            Some("Not saved")
                        } else if active {
                            Some(if entry.status == AttemptStatus::Pending {
                                "Queued"
                            } else if entry.generating {
                                "Generating…"
                            } else {
                                "Running…"
                            })
                        } else if failed {
                            Some(entry.status.label())
                        } else {
                            None
                        };
                        let translation = if let Some(status) = status {
                            if entry.translation.is_empty() {
                                status.to_owned()
                            } else {
                                format!("{status} · {}", entry.translation)
                            }
                        } else if entry.translation.is_empty() {
                            "No translation available".into()
                        } else {
                            entry.translation.clone()
                        };
                        let color = if failed {
                            theme.danger
                        } else if active {
                            theme.info
                        } else {
                            theme.muted_foreground
                        };
                        div()
                            .id(SharedString::from(format!("history-{}", entry.id)))
                            .group("history-row")
                            .flex()
                            .items_start()
                            .gap_2()
                            .px_3()
                            .py_2()
                            .mb_1()
                            .rounded_md()
                            .when(
                                self.page == Page::Dictionary && selected == Some(&entry.id),
                                |this| this.bg(theme.accent),
                            )
                            .hover(|this| this.bg(theme.accent))
                            .child(
                                div()
                                    .id("open-history-entry")
                                    .role(Role::Button)
                                    .tab_index(0)
                                    .aria_label(format!("{} · {}", entry.input, translation))
                                    .min_w_0()
                                    .flex_1()
                                    .cursor_pointer()
                                    .child(
                                        div()
                                            .text_sm()
                                            .line_height(rems(1.25))
                                            .font_weight(FontWeight::MEDIUM)
                                            .line_clamp(2)
                                            .child(
                                                entry
                                                    .input
                                                    .split_whitespace()
                                                    .collect::<Vec<_>>()
                                                    .join(" "),
                                            ),
                                    )
                                    .child(
                                        div()
                                            .flex()
                                            .items_start()
                                            .gap_1()
                                            .mt_1()
                                            .text_xs()
                                            .line_height(rems(1.25))
                                            .text_color(color)
                                            .when(active || failed, |this| {
                                                // Keep the status icon centered on the first line.
                                                this.child(
                                                    div()
                                                        .h(rems(1.25))
                                                        .flex()
                                                        .items_center()
                                                        .flex_shrink_0()
                                                        .child(if active {
                                                            Spinner::new()
                                                                .xsmall()
                                                                .into_any_element()
                                                        } else {
                                                            Icon::new(IconName::CircleAlert)
                                                                .xsmall()
                                                                .into_any_element()
                                                        }),
                                                )
                                            })
                                            .child(
                                                div()
                                                    .min_w_0()
                                                    .flex_1()
                                                    .line_clamp(2)
                                                    .child(translation),
                                            ),
                                    )
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        if this.deleting.as_ref() != Some(&lookup_id) {
                                            this.open_attempt(id.clone(), false, cx);
                                        }
                                    })),
                            )
                            .child(
                                // Reserve the action's space so hovering never shifts the text.
                                div().size(rems(1.25)).flex_shrink_0().invisible().when(
                                    !delete_disabled,
                                    |this| {
                                        this.group_hover("history-row", |this| this.visible())
                                            .child(
                                                icon_button(
                                                    "delete-lookup",
                                                    IconName::Trash,
                                                    "Delete lookup and all versions",
                                                )
                                                .xsmall()
                                                .size(rems(1.25))
                                                .text_color(theme.muted_foreground)
                                                .on_click(cx.listener(move |this, _, _, cx| {
                                                    cx.stop_propagation();
                                                    this.delete_lookup(delete_id.clone(), cx);
                                                })),
                                            )
                                    },
                                ),
                            )
                    })),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .h(px(40.))
                    .px_3()
                    .flex_shrink_0()
                    .border_t_1()
                    .border_color(theme.border)
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(Icon::new(IconName::Cpu).xsmall())
                    .child(
                        div()
                            .min_w_0()
                            .flex_1()
                            .truncate()
                            .child(self.configuration()),
                    )
                    .child(self.analysis_button(cx))
                    .child(self.settings_button(cx)),
            )
            .into_any_element()
    }

    fn search_bar(&self, cx: &Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let empty = self.input.read(cx).value().trim().is_empty();
        div()
            .flex()
            .items_center()
            .gap_2()
            .px_4()
            .py_3()
            .flex_shrink_0()
            .border_b_1()
            .border_color(theme.border)
            .child(
                icon_button(
                    "toggle-sidebar",
                    IconName::PanelLeft,
                    if self.sidebar_visible {
                        "Hide history (⌘B)"
                    } else {
                        "Show history (⌘B)"
                    },
                )
                .on_click(cx.listener(|this, _, _, cx| {
                    this.sidebar_visible = !this.sidebar_visible;
                    cx.notify();
                })),
            )
            .when(!self.sidebar_visible, |this| {
                this.child(self.new_lookup_button(cx))
            })
            .child(
                div().w(px(168.)).h(px(28.)).flex_shrink_0().child(
                    Select::new(&self.direction_picker)
                        .small()
                        .w_full()
                        .accessibility_label("Translation direction"),
                ),
            )
            .child(
                div().flex_1().min_w_0().child(
                    Textarea::new(&self.input)
                        .small()
                        .w_full()
                        .aria_label("English or Chinese word, phrase, or sentence")
                        .accessibility_id("lookup-input"),
                ),
            )
            .child(
                Button::new("lookup")
                    .small()
                    .primary()
                    .icon(IconName::Search)
                    .label("Look up")
                    .tooltip("Look up · Enter")
                    .disabled(empty)
                    .on_click(cx.listener(|this, _, _, cx| this.submit(cx))),
            )
            .when(!self.sidebar_visible, |this| {
                this.child(
                    icon_button("open-history", IconName::ClockArrowLeft, "History (⌘F)")
                        .on_click(cx.listener(|this, _, window, cx| this.open_history(window, cx))),
                )
                .child(self.analysis_button(cx))
                .child(self.settings_button(cx))
            })
            .into_any_element()
    }

    fn details_popover(&self, attempt: &Attempt) -> Popover {
        let details = vec![
            (
                "Status",
                if self.lookups.save_error(&attempt.id).is_some() {
                    "Saving incomplete".into()
                } else {
                    attempt.status.label().into()
                },
            ),
            (
                "Total tokens",
                formatting::exact_count(attempt.usage.total().map(u128::from)),
            ),
            (
                "Input",
                formatting::exact_count(attempt.usage.input_tokens.map(u128::from)),
            ),
            (
                "Cached input",
                formatting::exact_count(attempt.usage.cached_input_tokens.map(u128::from)),
            ),
            (
                "Output",
                formatting::exact_count(attempt.usage.output_tokens.map(u128::from)),
            ),
            (
                "Reasoning",
                formatting::exact_count(attempt.usage.reasoning_output_tokens.map(u128::from)),
            ),
            ("Response time", duration(attempt.duration_ms)),
            ("Queue wait", duration(attempt.queue_duration_ms)),
            ("CLI runtime", duration(attempt.cli_duration_ms)),
            ("First answer", duration(attempt.first_answer_ms)),
            ("Direction", attempt.direction.label().into()),
            ("Model", attempt.setup.model.clone()),
            ("Effort", attempt.setup.effort.clone()),
            (
                "Service tier",
                attempt
                    .setup
                    .service_tier
                    .as_ref()
                    .map_or("Not recorded", |tier| tier.label())
                    .into(),
            ),
            ("Skill", attempt.setup.skill_name.clone()),
            (
                "Skill version",
                attempt.skill_hash.chars().take(12).collect(),
            ),
            ("Prompt", attempt.prompt_version.clone()),
            ("Created", short_date(&attempt.created_at)),
        ];
        Popover::new(SharedString::from(format!("details-{}", attempt.id)))
            .anchor(Anchor::TopRight)
            .trigger(icon_button("entry-details", IconName::Info, "Entry details and token usage"))
            .content(move |_, _, cx| {
                let theme = cx.theme();
                div().id("entry-details-scroll").w(px(320.)).max_h(px(520.)).overflow_y_scroll().flex().flex_col().gap_3()
                    .child(div().flex_shrink_0().flex().items_center().gap_2().font_weight(FontWeight::SEMIBOLD)
                        .child(Icon::new(IconName::Info).small()).child("Entry details"))
                    .child(div().flex_shrink_0().flex().flex_col().gap_2().children(details.iter().map(|(label, value)| {
                        div().flex().items_start().gap_4().text_sm()
                            .child(div().w(px(104.)).flex_shrink_0().text_color(theme.muted_foreground).child(*label))
                            .child(div().min_w_0().flex_1().child(value.clone()))
                    })))
                    .child(div().flex_shrink_0().text_xs().text_color(theme.muted_foreground)
                        .child("Cached and reasoning tokens are included in the input and output totals. — means unavailable."))
            })
    }

    fn entry_toolbar(&self, attempt: &Attempt, cx: &Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let active = attempt.status.is_active();
        let has_activity = attempt.activity.iter().any(Activity::has_details);
        let heading = if attempt.status == AttemptStatus::Pending {
            "Queued".into()
        } else if active {
            if !attempt.answer.is_empty() {
                "Generating…".into()
            } else if let Some(activity) = attempt.activity.iter().rev().find(|a| a.has_details()) {
                if activity.label == "Thinking" && !activity.completed {
                    "Thinking…"
                } else {
                    "Working…"
                }
                .into()
            } else {
                "Running…".into()
            }
        } else if attempt.status == AttemptStatus::Succeeded {
            let label = if has_activity {
                "Worked for"
            } else {
                "Completed in"
            };
            let elapsed = if has_activity {
                attempt.cli_duration_ms.or(attempt.duration_ms)
            } else {
                attempt.duration_ms
            };
            elapsed
                .map(|ms| format!("{label} {:.1} s", ms as f64 / 1000.))
                .unwrap_or_else(|| "Completed".into())
        } else {
            format!("Work stopped · {}", attempt.status.label())
        };
        let answer = attempt.answer.clone();
        let stop_id = attempt.id.clone();
        div()
            .flex()
            .items_center()
            .justify_between()
            .gap_2()
            .h(px(32.))
            .flex_shrink_0()
            .text_color(theme.muted_foreground)
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_1()
                    .min_w_0()
                    .when(self.return_to_history, |this| {
                        this.child(
                            Button::new("back-to-history")
                                .small()
                                .ghost()
                                .icon(IconName::ChevronLeft)
                                .label("Back to History")
                                .on_click(
                                    cx.listener(|this, _, window, cx| {
                                        this.open_history(window, cx)
                                    }),
                                ),
                        )
                    })
                    .when(active, |this| this.child(Spinner::new().xsmall()))
                    .child(if has_activity {
                        Button::new("toggle-activity")
                            .small()
                            .ghost()
                            .icon(if self.lookups.activity_expanded {
                                IconName::ChevronDown
                            } else {
                                IconName::ChevronRight
                            })
                            .label(heading)
                            .accessibility_label(if self.lookups.activity_expanded {
                                "Hide activity"
                            } else {
                                "Show activity"
                            })
                            .tooltip(if self.lookups.activity_expanded {
                                "Collapse thinking and work"
                            } else {
                                "Expand thinking and work"
                            })
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.lookups.activity_expanded = !this.lookups.activity_expanded;
                                if this.lookups.activity_expanded
                                    && this
                                        .lookups
                                        .selected
                                        .as_ref()
                                        .is_some_and(|a| !a.status.is_active())
                                {
                                    this.activity_scroll.set_offset(point(px(0.), px(0.)));
                                }
                                cx.notify();
                            }))
                            .into_any_element()
                    } else {
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .px_2()
                            .text_sm()
                            .when(!active, |this| {
                                this.child(Icon::new(IconName::Clock).small())
                            })
                            .child(heading)
                            .into_any_element()
                    })
                    .when(active, |this| {
                        this.child(
                            icon_button("cancel-lookup", IconName::Square, "Stop this lookup")
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.send(Command::Cancel(stop_id.clone()));
                                    cx.notify();
                                })),
                        )
                    }),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .flex_shrink_0()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_1()
                            .text_xs()
                            .child(Icon::new(IconName::Zap).xsmall())
                            .child(token_count(
                                "entry-total-tokens",
                                attempt.usage.total().map(u128::from),
                            ))
                            .child("tokens"),
                    )
                    .child(
                        icon_button("regenerate", IconName::RotateCcw, "Regenerate")
                            .disabled(
                                attempt.status.is_active()
                                    || self.deleting.as_ref() == Some(&attempt.lookup_id)
                                    || self.setup.is_none(),
                            )
                            .on_click(
                                cx.listener(|this, _, window, cx| this.regenerate(window, cx)),
                            ),
                    )
                    .child(
                        icon_button("copy-entry", IconName::Copy, "Copy")
                            .disabled(answer.is_empty())
                            .on_click(move |_, _, cx| {
                                cx.write_to_clipboard(ClipboardItem::new_string(answer.clone()))
                            }),
                    )
                    .child(self.details_popover(attempt)),
            )
            .into_any_element()
    }

    fn activity_body(&self, attempt: &Attempt, cx: &Context<Self>) -> AnyElement {
        let theme = cx.theme();
        div()
            .id(SharedString::from(format!("activity-{}", attempt.id)))
            .flex_shrink_0()
            .max_h(px(if attempt.answer.is_empty() {
                220.
            } else {
                132.
            }))
            .overflow_y_scroll()
            .track_scroll(&self.activity_scroll)
            .border_l_2()
            .border_color(theme.border)
            .ml_2()
            .px_3()
            .py_2()
            .flex()
            .flex_col()
            .gap_2()
            .children(
                attempt
                    .activity
                    .iter()
                    .filter(|a| a.has_details())
                    .map(|activity| {
                        div()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(theme.muted_foreground)
                                    .child(activity.label.clone()),
                            )
                            .child(div().text_sm().child(activity.text.clone()))
                    }),
            )
            .into_any_element()
    }

    fn loading_entry(&self, attempt: &Attempt, cx: &Context<Self>) -> AnyElement {
        div()
            .id("loading-entry")
            .role(Role::Group)
            .aria_label("Loading translation")
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .overflow_hidden()
            .pt_3()
            .gap_6()
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .child(
                        div()
                            .text_3xl()
                            .font_weight(FontWeight::SEMIBOLD)
                            .line_clamp(2)
                            .child(
                                attempt
                                    .input
                                    .split_whitespace()
                                    .collect::<Vec<_>>()
                                    .join(" "),
                            ),
                    )
                    .child(
                        div()
                            .text_sm()
                            .text_color(cx.theme().muted_foreground)
                            .child(if attempt.status == AttemptStatus::Pending {
                                "Waiting for an available slot…"
                            } else {
                                "Preparing your dictionary entry…"
                            }),
                    ),
            )
            .child(
                div()
                    .w_full()
                    .max_w(px(640.))
                    .flex()
                    .flex_col()
                    .gap_3()
                    .children(
                        [
                            (0.68, 18., 0.),
                            (0.38, 12., 0.),
                            (0.24, 16., 20.),
                            (1.0, 12., 0.),
                            (0.9, 12., 0.),
                            (0.57, 12., 0.),
                            (0.2, 16., 20.),
                            (0.85, 12., 0.),
                            (0.63, 12., 0.),
                        ]
                        .into_iter()
                        .enumerate()
                        .map(|(index, (width, height, spacing))| {
                            div()
                                .id(SharedString::from(format!("loading-line-{index}")))
                                .w(relative(width))
                                .h(px(height))
                                .mt(px(spacing))
                                .flex_shrink_0()
                                .child(Skeleton::new().size_full().rounded_sm())
                        }),
                    ),
            )
            .into_any_element()
    }

    fn response(&self, cx: &Context<Self>) -> AnyElement {
        let theme = cx.theme();
        if let Some(attempt) = &self.lookups.selected {
            return div()
                .flex()
                .flex_col()
                .flex_1()
                .min_h_0()
                .px_6()
                .pt_2()
                .pb_3()
                .gap_2()
                .child(self.entry_toolbar(attempt, cx))
                .when_some(
                    self.lookups.save_error(&attempt.id).map(str::to_owned),
                    |this, message| this.child(message_box(message, theme.danger, theme.border)),
                )
                .when_some(attempt.failure.as_ref(), |this, failure| {
                    this.child(message_box(
                        failure.message.clone(),
                        theme.danger,
                        theme.border,
                    ))
                })
                .when(
                    self.lookups.activity_expanded
                        && attempt.activity.iter().any(Activity::has_details),
                    |this| this.child(self.activity_body(attempt, cx)),
                )
                .child(if attempt.answer.is_empty() && attempt.status.is_active() {
                    self.loading_entry(attempt, cx)
                } else if attempt.answer.is_empty() {
                    div()
                        .flex()
                        .flex_1()
                        .items_center()
                        .justify_center()
                        .text_sm()
                        .text_color(theme.muted_foreground)
                        .child("No entry was returned for this attempt.")
                        .into_any_element()
                } else {
                    div()
                        .flex_1()
                        .min_h_0()
                        .overflow_hidden()
                        .child(
                            TextView::markdown(
                                SharedString::from(format!("entry-{}", attempt.id)),
                                attempt.answer.clone(),
                            )
                            .selectable(true)
                            .scrollable(true)
                            .size_full()
                            .on_link_click(|url, _, _, cx| {
                                if url.starts_with("https://") || url.starts_with("http://") {
                                    cx.open_url(url);
                                }
                            }),
                        )
                        .into_any_element()
                })
                .into_any_element();
        }
        div()
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .items_center()
            .justify_center()
            .gap_3()
            .px_6()
            .child(
                Icon::new(IconName::BookOpenText)
                    .size(px(36.))
                    .text_color(theme.muted_foreground),
            )
            .child(
                div()
                    .text_xl()
                    .font_weight(FontWeight::MEDIUM)
                    .child("Understand the words you meet"),
            )
            .child(
                div()
                    .text_sm()
                    .text_color(theme.muted_foreground)
                    .child("English ↔ Chinese · Meanings, pronunciation, and examples."),
            )
            .child(
                div()
                    .mt_2()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child("Enter to look up · Shift+Enter for a new line · ⌘L to search"),
            )
            .into_any_element()
    }
}

impl Render for DictionaryView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        div()
            .flex()
            .size_full()
            .bg(theme.background)
            .text_color(theme.foreground)
            .text_base()
            // The textarea handles Shift+Enter and propagates submit actions.
            .on_action(cx.listener(
                |this, action: &gpui_kit::component::input::Enter, window, cx| {
                    if this.page == Page::Dictionary && !action.shift {
                        let composing = this.input.update(cx, |input, cx| {
                            input.marked_text_range(window, cx).is_some()
                        });
                        if !composing {
                            this.submit(cx);
                            cx.stop_propagation();
                        }
                    }
                },
            ))
            .on_action(cx.listener(|this, _: &FocusLookup, window, cx| {
                this.show_dictionary(window, cx);
            }))
            .on_action(
                cx.listener(|this, _: &FocusHistory, window, cx| this.open_history(window, cx)),
            )
            .on_action(
                cx.listener(|this, _: &FocusAnalysis, window, cx| this.open_analysis(window, cx)),
            )
            .on_action(
                cx.listener(|this, _: &OpenSettings, window, cx| this.open_settings(window, cx)),
            )
            .on_action(cx.listener(|this, _: &ToggleSidebar, _, cx| {
                this.sidebar_visible = !this.sidebar_visible;
                cx.notify();
            }))
            .when(self.sidebar_visible, |this| this.child(self.sidebar(cx)))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .when(self.page == Page::Dictionary, |this| {
                        this.child(self.search_bar(cx))
                    })
                    .when(
                        self.page != Page::Dictionary && !self.sidebar_visible,
                        |this| {
                            this.child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap_2()
                                    .px_4()
                                    .pt_3()
                                    .child(
                                        icon_button(
                                            "show-history-sidebar",
                                            IconName::PanelLeft,
                                            "Show sidebar",
                                        )
                                        .on_click(
                                            cx.listener(|this, _, _, cx| {
                                                this.sidebar_visible = true;
                                                cx.notify();
                                            }),
                                        ),
                                    )
                                    .child(self.new_lookup_button(cx))
                                    .child(self.analysis_button(cx)),
                            )
                        },
                    )
                    .when_some(
                        self.setup_error
                            .clone()
                            .filter(|_| self.page == Page::Dictionary),
                        |this, message| {
                            this.child(div().px_6().pt_2().child(message_box(
                                message,
                                theme.danger,
                                theme.border,
                            )))
                        },
                    )
                    .when_some(self.notice.clone(), |this, message| {
                        this.child(div().px_6().pt_2().child(message_box(
                            message,
                            theme.danger,
                            theme.border,
                        )))
                    })
                    .child(match self.page {
                        Page::History => self.history_view.clone().into_any_element(),
                        Page::Analysis => self.analysis_view.clone().into_any_element(),
                        Page::Dictionary => self.response(cx),
                    }),
            )
            .child(self.notification_layer(window))
    }
}

fn icon_button(id: &'static str, icon: IconName, label: &'static str) -> Button {
    Button::new(id)
        .small()
        .ghost()
        .icon(icon)
        .accessibility_label(label)
        .tooltip(label)
}

fn message_box(message: String, color: Hsla, border: Hsla) -> impl IntoElement {
    div()
        .id(SharedString::from(format!("message-{}", message.len())))
        .flex()
        .items_start()
        .gap_2()
        .p_2()
        .rounded_md()
        .border_1()
        .border_color(border)
        .text_sm()
        .text_color(color)
        .max_h(px(96.))
        .overflow_y_scroll()
        .child(Icon::new(IconName::CircleAlert).small())
        .child(div().min_w_0().flex_1().child(message))
}

fn token_count(id: impl Into<ElementId>, value: Option<u128>) -> Stateful<Div> {
    let exact = if value.is_some() {
        format!("{} tokens", formatting::exact_count(value))
    } else {
        "Token usage unavailable".into()
    };
    div()
        .id(id)
        .role(Role::Label)
        .aria_label(exact.clone())
        .whitespace_nowrap()
        .child(formatting::compact_count(value))
        .tooltip(move |window, cx| Tooltip::new(exact.clone()).build(window, cx))
}

fn duration(value: Option<u64>) -> String {
    value
        .map(|ms| format!("{:.1} s", ms as f64 / 1000.))
        .unwrap_or_else(|| "—".into())
}

fn short_date(value: &str) -> String {
    chrono::DateTime::parse_from_rfc3339(value)
        .map(|v| {
            v.with_timezone(&chrono::Local)
                .format("%b %-d, %H:%M")
                .to_string()
        })
        .unwrap_or_else(|_| value.into())
}

fn direction_items(input: &str) -> Vec<PickerItem<DirectionChoice>> {
    DirectionChoice::ALL
        .into_iter()
        .map(|value| PickerItem {
            value,
            label: if value == DirectionChoice::Auto && !input.trim().is_empty() {
                value
                    .resolve(input)
                    .map(|d| format!("Auto · {}", d.label()))
                    .unwrap_or_else(|_| "Auto · choose".into())
                    .into()
            } else {
                value.label().into()
            },
        })
        .collect()
}
