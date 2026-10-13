//! Lookup presentation state, independent of GPUI and subprocess scheduling.
use std::collections::HashMap;

use crate::{
    model::{Attempt, HistoryEntry},
    service::{Event, RequestId},
};

#[derive(Default)]
pub struct LookupState {
    pub history: Vec<HistoryEntry>,
    pub selected: Option<Attempt>,
    pub activity_expanded: bool,
    active: HashMap<String, Attempt>,
    unsaved: HashMap<String, (Attempt, String)>,
    navigation: RequestId,
}

impl LookupState {
    pub fn begin_navigation(&mut self) -> RequestId {
        self.navigation += 1;
        self.navigation
    }

    pub fn accepts_response(&self, request_id: RequestId) -> bool {
        self.navigation == request_id
    }

    pub fn new_lookup(&mut self) {
        self.begin_navigation();
        self.selected = None;
        self.activity_expanded = false;
    }

    pub fn has_active(&self) -> bool {
        !self.active.is_empty()
    }

    pub fn lookup_active(&self, lookup_id: &str) -> bool {
        self.active
            .values()
            .any(|attempt| attempt.lookup_id == lookup_id)
    }

    pub fn save_error(&self, attempt_id: &str) -> Option<&str> {
        self.unsaved
            .get(attempt_id)
            .map(|(_, error)| error.as_str())
    }

    pub fn receive(&mut self, event: &Event) {
        match event {
            Event::History(entries) => {
                self.history = entries.clone();
                // A database refresh must not erase another job's live preview,
                // nor hide a result whose final save failed.
                for attempt in self
                    .active
                    .values()
                    .chain(self.unsaved.values().map(|(a, _)| a))
                {
                    update_history(&mut self.history, attempt);
                }
                // Cached search can open an entry outside the recent-history
                // window. Keep that selection visible when other work finishes.
                if let Some(attempt) = &self.selected {
                    update_history(&mut self.history, attempt);
                }
            }
            Event::Opened {
                request_id,
                attempt,
            } => {
                if attempt.status.is_active() {
                    self.active.insert(attempt.id.clone(), *attempt.clone());
                    update_history(&mut self.history, attempt);
                }
                if self.accepts_response(*request_id) {
                    let attempt = self
                        .unsaved
                        .get(&attempt.id)
                        .map(|(attempt, _)| attempt.clone())
                        .unwrap_or_else(|| *attempt.clone());
                    update_history(&mut self.history, &attempt);
                    self.activity_expanded = attempt.status.is_active();
                    self.selected = Some(attempt);
                }
            }
            Event::Started(attempt) => {
                self.active.insert(attempt.id.clone(), *attempt.clone());
                update_history(&mut self.history, attempt);
                if self.selected.as_ref().is_some_and(|a| a.id == attempt.id) {
                    self.selected = Some(*attempt.clone());
                }
            }
            Event::Progress(progress) => {
                if let Some(attempt) = self.active.get_mut(&progress.attempt_id) {
                    progress.apply_to(attempt);
                    update_history(&mut self.history, attempt);
                    if self.selected.as_ref().is_some_and(|a| a.id == attempt.id) {
                        self.selected = Some(attempt.clone());
                    }
                }
            }
            Event::Finished {
                attempt,
                save_error,
            } => {
                self.active.remove(&attempt.id);
                if let Some(error) = save_error {
                    self.unsaved
                        .insert(attempt.id.clone(), (*attempt.clone(), error.clone()));
                } else {
                    self.unsaved.remove(&attempt.id);
                }
                update_history(&mut self.history, attempt);
                if self.selected.as_ref().is_some_and(|a| a.id == attempt.id) {
                    self.selected = Some(*attempt.clone());
                    self.activity_expanded = false;
                }
            }
            Event::DeletedLookup(lookup_id) => {
                self.history.retain(|entry| &entry.lookup_id != lookup_id);
                self.unsaved
                    .retain(|_, (attempt, _)| &attempt.lookup_id != lookup_id);
                if self
                    .selected
                    .as_ref()
                    .is_some_and(|a| &a.lookup_id == lookup_id)
                {
                    self.selected = None;
                    self.activity_expanded = false;
                }
            }
            _ => {}
        }
    }
}

fn update_history(history: &mut Vec<HistoryEntry>, attempt: &Attempt) {
    let entry = HistoryEntry::from(attempt);
    if let Some(existing) = history.iter_mut().find(|entry| entry.id == attempt.id) {
        *existing = entry;
    } else {
        history.insert(0, entry);
    }
}
