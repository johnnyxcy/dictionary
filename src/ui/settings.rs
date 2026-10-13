use std::path::PathBuf;

use dictionary::settings::{Appearance, EFFORTS, Preferences, SettingsState};
use gpui_kit::{
    assets::IconName,
    component::{
        ActiveTheme, Disableable, Icon, Selectable, Sizable, WindowExt,
        button::{Button, ButtonVariant, ButtonVariants},
        input::{Input, InputEvent, InputState},
        select::{SearchableVec, Select, SelectEvent, SelectItem, SelectState},
        switch::Switch,
    },
    prelude::FluentBuilder,
    *,
};

#[derive(Clone)]
pub struct PickerItem<T: Clone + PartialEq> {
    pub value: T,
    pub label: SharedString,
}

impl<T: Clone + PartialEq> SelectItem for PickerItem<T> {
    type Value = T;
    fn title(&self) -> SharedString {
        self.label.clone()
    }
    fn value(&self) -> &T {
        &self.value
    }
}

pub enum SettingsEvent {
    Save(Preferences),
    Refresh,
}

pub struct SettingsView {
    state: SettingsState,
    draft: Preferences,
    baseline: Preferences,
    models: Entity<SelectState<SearchableVec<PickerItem<String>>>>,
    skills: Entity<SelectState<Vec<PickerItem<PathBuf>>>>,
    cli_path: Entity<InputState>,
    saving: bool,
    confirming_discard: bool,
    refreshing: bool,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<SettingsEvent> for SettingsView {}

impl SettingsView {
    pub fn new(
        state: SettingsState,
        refreshing: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let draft = state.preferences.clone().unwrap_or(Preferences {
            version: 1,
            model: String::new(),
            effort: String::new(),
            fast_mode: false,
            skill_path: PathBuf::new(),
            cli_path: None,
            appearance: Appearance::System,
        });
        let models = cx.new(|cx| {
            SelectState::new(
                SearchableVec::new(Vec::<PickerItem<String>>::new()),
                None,
                window,
                cx,
            )
            .searchable(true)
        });
        let skills =
            cx.new(|cx| SelectState::new(Vec::<PickerItem<PathBuf>>::new(), None, window, cx));
        let cli_path = cx.new(|cx| InputState::new(window, cx).placeholder("Automatic discovery"));
        cli_path.update(cx, |input, cx| {
            input.set_value(
                draft
                    .cli_path
                    .as_ref()
                    .map(|p| p.to_string_lossy().into_owned())
                    .unwrap_or_default(),
                window,
                cx,
            )
        });
        let subscriptions = vec![
            cx.subscribe_in(&cli_path, window, |this: &mut Self, input, event, _, cx| {
                if matches!(event, InputEvent::Change) {
                    let value = input.read(cx).value();
                    this.draft.cli_path =
                        (!value.trim().is_empty()).then(|| PathBuf::from(value.trim()));
                    this.changed(cx);
                }
            }),
            cx.subscribe_in(&models, window, |this: &mut Self, _, event, _, cx| {
                if let SelectEvent::Confirm(Some(value)) = event {
                    this.draft.model = value.clone();
                    // Keep the existing effort only if this model supports it.
                    if !this
                        .state
                        .catalog
                        .as_ref()
                        .and_then(|c| c.models.iter().find(|m| m.model == *value))
                        .is_some_and(|m| m.supports(&this.draft.effort))
                    {
                        this.draft.effort.clear();
                    }
                    this.changed(cx);
                }
            }),
            cx.subscribe_in(&skills, window, |this: &mut Self, _, event, _, cx| {
                if let SelectEvent::Confirm(Some(value)) = event {
                    this.draft.skill_path = value.clone();
                    this.changed(cx);
                }
            }),
        ];
        let mut view = Self {
            state,
            baseline: draft.clone(),
            draft,
            models,
            skills,
            cli_path,
            saving: false,
            confirming_discard: false,
            refreshing,
            _subscriptions: subscriptions,
        };
        view.sync_pickers(window, cx);
        view
    }

    fn changed(&mut self, cx: &mut Context<Self>) {
        cx.notify();
    }

    fn has_unsaved_changes(&self) -> bool {
        self.draft != self.baseline
    }

    fn is_valid(&self) -> bool {
        if self.draft.cli_path != self.baseline.cli_path {
            return true;
        }
        if self.state.preferences.is_some() && self.draft.same_generation_settings(&self.baseline) {
            return true;
        }
        self.state
            .catalog
            .as_ref()
            .is_some_and(|catalog| self.draft.setup(catalog).is_ok())
    }

    pub fn save(&mut self, cx: &mut Context<Self>) {
        if self.saving || self.confirming_discard || !self.is_valid() {
            return;
        }
        self.saving = true;
        cx.emit(SettingsEvent::Save(self.draft.clone()));
        cx.notify();
    }

    /// Return whether the caller may close Settings immediately.
    pub fn request_cancel(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        if self.saving || self.confirming_discard {
            return false;
        }
        if !self.has_unsaved_changes() {
            return true;
        }

        self.confirming_discard = true;
        let view = cx.entity().downgrade();
        window.open_alert_dialog(cx, move |alert, _, _| {
            let keep_editing = view.clone();
            let discard = view.clone();
            alert
                .title("Discard changes?")
                .description("Unsaved changes will be discarded.")
                .confirm()
                .cancel_text("Keep editing")
                .ok_text("Discard")
                .ok_variant(ButtonVariant::Danger)
                .on_cancel(move |_, _, cx| {
                    keep_editing
                        .update(cx, |view, cx| {
                            view.confirming_discard = false;
                            cx.notify();
                        })
                        .ok();
                    true
                })
                .on_ok(move |_, window, cx| {
                    discard
                        .update(cx, |view, cx| {
                            view.confirming_discard = false;
                            cx.notify();
                        })
                        .ok();
                    // Settings opens only when no other dialog is present;
                    // close it and this confirmation without an extra auto-close.
                    window.close_all_dialogs(cx);
                    false
                })
        });
        cx.notify();
        false
    }

    fn sync_pickers(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let models: Vec<_> = self
            .state
            .catalog
            .as_ref()
            .map(|c| {
                c.models
                    .iter()
                    .map(|m| PickerItem {
                        value: m.model.clone(),
                        label: m.display_name.clone().into(),
                    })
                    .collect()
            })
            .unwrap_or_default();
        self.models.update(cx, |state, cx| {
            state.set_items(SearchableVec::new(models), window, cx);
            state.set_selected_value(&self.draft.model, window, cx);
        });
        let mut skills: Vec<_> = self
            .state
            .catalog
            .as_ref()
            .map(|c| {
                c.skills
                    .iter()
                    .map(|s| PickerItem {
                        value: s.path.clone(),
                        label: format!("{} · {}", s.name, s.path.display()).into(),
                    })
                    .collect()
            })
            .unwrap_or_default();
        if !self.draft.skill_path.as_os_str().is_empty()
            && !skills.iter().any(|s| s.value == self.draft.skill_path)
        {
            skills.push(PickerItem {
                value: self.draft.skill_path.clone(),
                label: format!("dict · {}", self.draft.skill_path.display()).into(),
            });
        }
        self.skills.update(cx, |state, cx| {
            state.set_items(skills, window, cx);
            state.set_selected_value(&self.draft.skill_path, window, cx);
        });
    }

    pub fn update_state(
        &mut self,
        state: SettingsState,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(preferences) = &state.preferences {
            if !self.has_unsaved_changes() {
                self.draft = preferences.clone();
                self.cli_path.update(cx, |input, cx| {
                    input.set_value(
                        preferences
                            .cli_path
                            .as_ref()
                            .map(|p| p.to_string_lossy().into_owned())
                            .unwrap_or_default(),
                        window,
                        cx,
                    )
                });
            }
            self.baseline = preferences.clone();
        }
        self.state = state;
        self.refreshing = false;
        self.sync_pickers(window, cx);
        cx.notify();
    }

    pub fn set_refreshing(&mut self, cx: &mut Context<Self>) {
        self.refreshing = true;
        cx.notify();
    }

    pub fn finish_save(&mut self, cx: &mut Context<Self>) {
        self.saving = false;
        cx.notify();
    }

    pub fn refresh_failed(&mut self, cx: &mut Context<Self>) {
        self.refreshing = false;
        cx.notify();
    }

    fn browse_skill(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let paths = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some("Choose dictionary SKILL.md".into()),
        });
        cx.spawn_in(window, async move |view, cx| {
            if let Ok(Ok(Some(paths))) = paths.await
                && let Some(path) = paths.into_iter().next()
            {
                view.update_in(cx, |this, window, cx| {
                    this.draft.skill_path = path;
                    this.sync_pickers(window, cx);
                    this.changed(cx);
                })
                .ok();
            }
        })
        .detach();
    }

    fn browse_cli(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let paths = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some("Choose the Codex executable".into()),
        });
        cx.spawn_in(window, async move |view, cx| {
            if let Ok(Ok(Some(paths))) = paths.await
                && let Some(path) = paths.into_iter().next()
            {
                view.update_in(cx, |this, window, cx| {
                    this.cli_path.update(cx, |input, cx| {
                        input.set_value(path.to_string_lossy().into_owned(), window, cx)
                    });
                    this.draft.cli_path = Some(path);
                    this.changed(cx);
                })
                .ok();
            }
        })
        .detach();
    }

    pub fn footer(&self, cx: &Context<Self>) -> Div {
        div()
            .flex()
            .justify_end()
            .gap_2()
            .pt_2()
            .child(
                Button::new("cancel-settings")
                    .label("Cancel")
                    .disabled(self.saving)
                    .on_click(cx.listener(|this, _, window, cx| {
                        if this.request_cancel(window, cx) {
                            window.close_dialog(cx);
                        }
                    })),
            )
            .child(
                Button::new("save-settings")
                    .primary()
                    .icon(IconName::Check)
                    .label(if self.saving { "Saving…" } else { "Save" })
                    .disabled(!self.is_valid() || self.saving)
                    .on_click(cx.listener(|this, _, _, cx| this.save(cx))),
            )
    }
}

impl Render for SettingsView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let model = self
            .state
            .catalog
            .as_ref()
            .and_then(|c| c.models.iter().find(|m| m.model == self.draft.model));
        let supports_fast = model.is_some_and(|m| m.supports_fast);
        let catalog_status = self
            .state
            .catalog
            .as_ref()
            .map(|c| {
                format!(
                    "{} · {} · {}",
                    if self.state.cached {
                        "Cached catalog"
                    } else {
                        "Catalog updated"
                    },
                    super::short_date(&c.retrieved_at),
                    c.cli_version
                )
            })
            .unwrap_or_else(|| "Model catalog unavailable".into());
        div().flex().flex_col().gap_4().w_full()
            .child(div().text_sm().text_color(theme.muted_foreground).child("Choose how new entries are generated. Running and saved entries keep their original settings."))
            .child(div().flex().flex_col().gap_2()
                .child(field_label(IconName::Monitor, "Appearance"))
                .child(div().flex().gap_2().children([(Appearance::System, "System"), (Appearance::Light, "Light"), (Appearance::Dark, "Dark")].map(|(appearance, label)| {
                    Button::new(label).label(label).flex_1().selected(self.draft.appearance == appearance).disabled(self.saving)
                        .on_click(cx.listener(move |this, _, _, cx| { this.draft.appearance = appearance; this.changed(cx); }))
                }))))
            .child(div().flex().flex_col().gap_2()
                .child(field_label(IconName::Cpu, "Model"))
                .child(div().w_full().h(px(34.)).child(Select::new(&self.models).w_full().placeholder("Choose a model").accessibility_label("Model").disabled(self.saving))))
            .child(div().flex().flex_col().gap_2()
                .child(field_label(IconName::Sparkles, "Thinking effort"))
                .child(div().flex().gap_2().children(EFFORTS.iter().map(|&(effort, label)| {
                    let supported = model.is_some_and(|m| m.supports(effort));
                    Button::new(effort).label(label).flex_1().selected(self.draft.effort == effort)
                        .disabled(!supported || self.saving)
                        .tooltip(if supported { format!("Use {label} thinking effort") } else { format!("{label} is unavailable for this model") })
                        .on_click(cx.listener(move |this, _, _, cx| { this.draft.effort = effort.into(); this.changed(cx); }))
                })))
                .child(div().text_xs().text_color(theme.muted_foreground).child(if model.is_some_and(|m| !m.supports(&self.draft.effort)) { "Select a supported effort for this model." } else { "Lower effort favors speed; higher effort allows more reasoning." })))
            .child(div().flex().items_center().justify_between().gap_4()
                .child(div().flex().flex_col().gap_1().flex_1()
                    .child(field_label(IconName::Zap, "Fast mode"))
                    .child(div().text_xs().text_color(theme.muted_foreground).child(if supports_fast {
                        "Faster responses with higher usage. Thinking effort stays the same."
                    } else if self.draft.fast_mode {
                        "Turn off Fast mode to use this model."
                    } else {
                        "This model does not advertise Fast mode."
                    })))
                .child(Switch::new("fast-mode").checked(self.draft.fast_mode).accessibility_label("Fast mode")
                    .disabled(self.saving || (!supports_fast && !self.draft.fast_mode))
                    .on_change(cx.listener(|this, checked, _, cx| { this.draft.fast_mode = *checked; this.changed(cx); }))))
            .child(div().flex().flex_col().gap_2()
                .child(field_label(IconName::BookOpen, "Dictionary skill"))
                .child(div().flex().items_center().gap_2()
                    .child(div().flex_1().min_w_0().h(px(34.)).child(Select::new(&self.skills).w_full().placeholder("Choose a dictionary skill").accessibility_label("Dictionary skill").disabled(self.saving)))
                    .child(Button::new("choose-skill").icon(IconName::FolderOpen).label("Browse…").disabled(self.saving).on_click(cx.listener(|this, _, window, cx| this.browse_skill(window, cx)))))
                .child(div().text_xs().text_color(theme.muted_foreground).child("Each lookup saves a copy of the selected skill for its entry.")))
            .child(div().flex().flex_col().gap_2().pt_3().border_t_1().border_color(theme.border)
                .child(field_label(IconName::Terminal, "CLI location"))
                .child(div().flex().items_center().gap_2()
                    .child(div().flex_1().min_w_0().child(Input::new(&self.cli_path).aria_label("CLI executable path").disabled(self.saving)))
                    .child(Button::new("choose-cli").icon(IconName::FolderOpen).label("Browse…").accessibility_label("Browse for CLI executable").disabled(self.saving).on_click(cx.listener(|this, _, window, cx| this.browse_cli(window, cx)))))
                .child(div().text_xs().text_color(theme.muted_foreground).child("Paste the absolute path from command -v codex in Terminal, or leave blank for automatic discovery. Save connects using this location."))
                .child(div().text_xs().text_color(theme.muted_foreground).child("Authentication uses your existing CLI login. If needed, run codex login in Terminal, then Refresh."))
                .child(div().flex().items_center().justify_between().gap_2()
                    .child(div().text_xs().text_color(theme.muted_foreground).child(catalog_status))
                    .child(Button::new("refresh-catalog").small().ghost().icon(IconName::RefreshCw).label(if self.refreshing { "Refreshing…" } else { "Refresh" }).disabled(self.refreshing || self.saving || self.draft.cli_path != self.baseline.cli_path).on_click(cx.listener(|this, _, _, cx| { this.refreshing = true; cx.emit(SettingsEvent::Refresh); cx.notify(); }))))
                .when_some(self.state.catalog.as_ref(), |this, c| this.child(div().text_xs().text_color(theme.muted_foreground).child(format!("CLI: {}", c.executable.display()))))
                .when_some(self.state.diagnostic.clone(), |this, text| this.child(super::message_box(text, theme.danger, theme.border))))
    }
}

fn field_label(icon: IconName, label: &'static str) -> impl IntoElement {
    div()
        .flex()
        .items_center()
        .gap_2()
        .text_sm()
        .font_weight(FontWeight::MEDIUM)
        .child(Icon::new(icon).small())
        .child(label)
}
