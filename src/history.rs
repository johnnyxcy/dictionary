//! Searchable attempt history and pagination, independent of the desktop UI.
use unicode_normalization::UnicodeNormalization;

use crate::{
    direction::Direction,
    model::{AttemptStatus, HistoryEntry, ServiceTier, Usage},
};

pub const PAGE_SIZE: usize = 25;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct HistoryFilter {
    pub search: String,
    pub direction: Option<Direction>,
    pub status: Option<AttemptStatus>,
}

impl HistoryFilter {
    pub fn search_key(&self) -> String {
        self.search.trim().nfc().collect::<String>().to_lowercase()
    }

    pub fn is_empty(&self) -> bool {
        self.search.trim().is_empty() && self.direction.is_none() && self.status.is_none()
    }
}

/// Immutable ordering fields. The cursor remains usable if its row is deleted.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HistoryCursor {
    pub created_at: String,
    pub id: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct HistoryQuery {
    pub filter: HistoryFilter,
    pub before: Option<HistoryCursor>,
}

#[derive(Clone, Debug)]
pub struct HistoryRow {
    pub entry: HistoryEntry,
    pub version: u64,
    pub model: String,
    pub effort: String,
    pub tier: Option<ServiceTier>,
    pub usage: Usage,
    pub duration_ms: Option<u64>,
    /// Includes active versions outside this page or the chosen filters.
    pub lookup_active: bool,
}

#[derive(Clone, Debug, Default)]
pub struct HistoryPage {
    pub rows: Vec<HistoryRow>,
    pub next: Option<HistoryCursor>,
    pub attempts: u64,
    pub lookups: u64,
}

pub struct HistoryState {
    pub query: HistoryQuery,
    pub page: HistoryPage,
    pub loading: bool,
    pub error: Option<String>,
    dirty: bool,
    request_id: u64,
    previous: Vec<Option<HistoryCursor>>,
}

impl Default for HistoryState {
    fn default() -> Self {
        Self {
            query: HistoryQuery::default(),
            page: HistoryPage::default(),
            loading: false,
            error: None,
            dirty: true,
            request_id: 0,
            previous: Vec::new(),
        }
    }
}

impl HistoryState {
    pub fn invalidate(&mut self) {
        self.dirty = true;
    }

    pub fn needs_refresh(&self) -> bool {
        self.dirty && !self.loading
    }

    pub fn page_number(&self) -> usize {
        self.previous.len() + 1
    }

    pub fn can_previous(&self) -> bool {
        !self.previous.is_empty() && !self.loading
    }

    pub fn can_next(&self) -> bool {
        self.page.next.is_some() && !self.loading
    }

    pub fn request(&mut self) -> (u64, HistoryQuery) {
        self.request_id += 1;
        self.loading = true;
        self.error = None;
        self.dirty = false;
        (self.request_id, self.query.clone())
    }

    pub fn filter(&mut self, filter: HistoryFilter) -> bool {
        if self.query.filter == filter {
            return false;
        }
        self.query = HistoryQuery {
            filter,
            before: None,
        };
        self.previous.clear();
        self.page = HistoryPage::default();
        self.dirty = true;
        true
    }

    pub fn next_page(&mut self) -> bool {
        if !self.can_next() {
            return false;
        }
        self.previous.push(self.query.before.clone());
        self.query.before = self.page.next.take();
        self.page.rows.clear();
        self.dirty = true;
        true
    }

    pub fn previous_page(&mut self) -> bool {
        if self.loading {
            return false;
        }
        let Some(cursor) = self.previous.pop() else {
            return false;
        };
        self.query.before = cursor;
        self.page.rows.clear();
        self.page.next = None;
        self.dirty = true;
        true
    }

    pub fn receive(&mut self, request_id: u64, page: HistoryPage) -> bool {
        if request_id != self.request_id {
            return false;
        }
        self.page = page;
        self.loading = false;
        self.error = None;
        // Deletion may empty an older page. Step back through saved cursors,
        // retaining the filter, and let the visible view request that page.
        if self.page.rows.is_empty() {
            self.previous_page();
        }
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
