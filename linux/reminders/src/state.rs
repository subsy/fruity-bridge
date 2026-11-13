//! What the app knows and how it slices it into the sections each view shows.

use std::collections::{HashMap, HashSet};

use fruity_bridge_client::reminders::{List, Reminder};
use fruity_bridge_client::{Client, Config};
use chrono::{NaiveDate, NaiveDateTime};
use serde::{Deserialize, Serialize};

use crate::dates;

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum View {
    Today,
    Scheduled,
    All,
    Completed,
    List(String),
}

impl View {
    pub fn is_smart(&self) -> bool {
        !matches!(self, View::List(_))
    }

    /// Which completed reminders this view needs, as a cache key and list filter.
    pub fn completed_scope(&self) -> CompletedScope {
        match self {
            View::List(id) => CompletedScope::List(id.clone()),
            _ => CompletedScope::All,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CompletedScope {
    All,
    List(String),
}

impl CompletedScope {
    pub fn cache_key(&self) -> String {
        match self {
            CompletedScope::All => "reminders-completed-all".into(),
            CompletedScope::List(id) => fruity_bridge_client::cache::key("reminders-completed", id),
        }
    }
}

/// Saved between runs so the app reopens where you left it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UiState {
    pub view: View,
    pub show_completed: bool,
}

impl Default for UiState {
    fn default() -> Self {
        UiState {
            view: View::Today,
            show_completed: false,
        }
    }
}

pub const LISTS_CACHE: &str = "reminders-lists";
pub const OPEN_CACHE: &str = "reminders-open";
pub const UI_CACHE: &str = "reminders-ui";

#[derive(Debug, Default, Clone, PartialEq)]
pub struct Data {
    pub lists: Vec<List>,
    /// Every incomplete reminder, across lists.
    pub open: Vec<Reminder>,
    /// Completed reminders for `completed_scope`, loaded only when they're shown.
    pub completed: Vec<Reminder>,
    pub completed_scope: Option<CompletedScope>,
}

pub struct State {
    pub config: Config,
    pub client: Client,
    pub data: Data,
    pub view: View,
    pub show_completed: bool,
    pub search: String,
    /// The last request reached the bridge.
    pub online: bool,
    /// Data has been shown (from the cache or the bridge).
    pub loaded: bool,
    /// Completed in this session, still on screen until the short "done" delay passes.
    pub lingering: HashMap<String, Reminder>,
    /// Deleted but still undoable; hidden from every view.
    pub pending_deletes: HashMap<String, Reminder>,
}

impl State {
    pub fn new(config: Config) -> Self {
        let client = Client::new(&config);
        State {
            config,
            client,
            data: Data::default(),
            view: View::Today,
            show_completed: false,
            search: String::new(),
            online: true,
            loaded: false,
            lingering: HashMap::new(),
            pending_deletes: HashMap::new(),
        }
    }

    pub fn list(&self, id: &str) -> Option<&List> {
        self.data.lists.iter().find(|l| l.id == id)
    }

    pub fn reminder(&self, id: &str) -> Option<&Reminder> {
        self.data
            .open
            .iter()
            .chain(self.data.completed.iter())
            .chain(self.lingering.values())
            .find(|r| r.id == id)
    }

    /// Replaces a reminder's local copy (after an edit), wherever it lives.
    pub fn upsert(&mut self, rem: Reminder) {
        self.data.open.retain(|r| r.id != rem.id);
        self.data.completed.retain(|r| r.id != rem.id);
        if rem.completed {
            self.data.completed.push(rem);
        } else {
            self.data.open.push(rem);
        }
        self.recount();
    }

    pub fn remove(&mut self, id: &str) {
        self.data.open.retain(|r| r.id != id);
        self.data.completed.retain(|r| r.id != id);
        self.lingering.remove(id);
        self.recount();
    }

    /// Counts each list's open reminders for the sidebar (lists are fetched without counts).
    pub fn recount(&mut self) {
        let hidden = &self.pending_deletes;
        for list in &mut self.data.lists {
            list.open_count = Some(
                self.data
                    .open
                    .iter()
                    .filter(|r| {
                        r.list_id == list.id && !r.completed && !hidden.contains_key(&r.id)
                    })
                    .count() as u32,
            );
        }
    }

    /// The list a new reminder goes into from the current view.
    pub fn target_list(&self) -> Option<&List> {
        if let View::List(id) = &self.view {
            return self.list(id).filter(|l| l.editable);
        }
        self.data.lists.iter().find(|l| l.editable)
    }

    fn visible(&self, r: &Reminder) -> bool {
        !self.pending_deletes.contains_key(&r.id)
    }

    /// Open reminders plus anything still lingering after being completed here.
    fn open_with_lingering(&self) -> Vec<&Reminder> {
        let mut out: Vec<&Reminder> = self.data.open.iter().filter(|r| self.visible(r)).collect();
        let present: HashSet<&str> = out.iter().map(|r| r.id.as_str()).collect();
        let extra: Vec<&Reminder> = self
            .lingering
            .values()
            .filter(|r| !present.contains(r.id.as_str()) && self.visible(r))
            .collect();
        out.extend(extra);
        out
    }

    fn completed_shown(&self) -> Vec<&Reminder> {
        let wants = self.show_completed || self.view == View::Completed || !self.search.is_empty();
        if !wants {
            return Vec::new();
        }
        let lingering: HashSet<&str> = self.lingering.keys().map(String::as_str).collect();
        self.data
            .completed
            .iter()
            .filter(|r| self.visible(r) && !lingering.contains(r.id.as_str()))
            .collect()
    }

    pub fn counts(&self, today: NaiveDate) -> Counts {
        let open: Vec<&Reminder> = self
            .data
            .open
            .iter()
            .filter(|r| self.visible(r) && !r.completed)
            .collect();
        let due_by = |r: &&Reminder, f: &dyn Fn(NaiveDate) -> bool| {
            r.due.as_ref().and_then(dates::due_date).is_some_and(f)
        };
        Counts {
            today: open.iter().filter(|r| due_by(r, &|d| d <= today)).count(),
            scheduled: open.iter().filter(|r| r.due.is_some()).count(),
            all: open.len(),
        }
    }

    /// Sections for the content pane: the current view, or search results when searching.
    pub fn sections(&self, now: NaiveDateTime) -> Vec<Section> {
        let today = now.date();
        let open = self.open_with_lingering();
        let completed = self.completed_shown();

        if !self.search.trim().is_empty() {
            let needle = self.search.trim().to_lowercase();
            let hit = |r: &&&Reminder| {
                r.title.to_lowercase().contains(&needle)
                    || r.notes
                        .as_deref()
                        .unwrap_or("")
                        .to_lowercase()
                        .contains(&needle)
            };
            let all: Vec<&Reminder> = open
                .iter()
                .filter(hit)
                .chain(completed.iter().filter(hit))
                .copied()
                .collect();
            return self.by_list(all);
        }

        match &self.view {
            View::Today => {
                let due_today = |r: &&Reminder| {
                    r.due
                        .as_ref()
                        .and_then(dates::due_date)
                        .is_some_and(|d| d <= today)
                };
                let mut items: Vec<&Reminder> = open.iter().copied().filter(due_today).collect();
                sort(&mut items);
                let (overdue, todays): (Vec<_>, Vec<_>) = items.into_iter().partition(|r| {
                    r.due
                        .as_ref()
                        .and_then(dates::due_date)
                        .is_some_and(|d| d < today)
                });
                let done: Vec<&Reminder> = completed
                    .iter()
                    .copied()
                    .filter(|r| r.completed_at.as_deref().and_then(dates::local_day) == Some(today))
                    .collect();
                let mut out = Vec::new();
                if !overdue.is_empty() {
                    out.push(Section::titled("Overdue", None, overdue));
                }
                if !todays.is_empty() || !done.is_empty() {
                    let title = if out.is_empty() {
                        None
                    } else {
                        Some("Today".to_string())
                    };
                    out.push(Section {
                        title,
                        color: None,
                        reminders: cloned(todays.into_iter().chain(done)),
                    });
                }
                out
            }
            View::Scheduled => {
                let mut items: Vec<&Reminder> = open
                    .iter()
                    .copied()
                    .chain(completed.iter().copied())
                    .filter(|r| r.due.is_some())
                    .collect();
                sort(&mut items);
                let mut out: Vec<Section> = Vec::new();
                for r in items {
                    let day = r.due.as_ref().and_then(dates::due_date);
                    let title = match day {
                        Some(d) if d < today && !r.completed => "Overdue".to_string(),
                        Some(d) => dates::section_label(d, today),
                        None => "Later".to_string(),
                    };
                    match out.iter_mut().find(|s| s.title.as_deref() == Some(&title)) {
                        Some(s) => s.reminders.push(r.clone()),
                        None => out.push(Section::titled(&title, None, vec![r])),
                    }
                }
                out
            }
            View::All => self.by_list(open.into_iter().chain(completed).collect()),
            View::Completed => {
                let mut done = completed;
                done.sort_by(|a, b| b.completed_at.cmp(&a.completed_at));
                self.by_list(done)
            }
            View::List(id) => {
                let mut items: Vec<&Reminder> =
                    open.into_iter().filter(|r| &r.list_id == id).collect();
                sort(&mut items);
                let mut done: Vec<&Reminder> =
                    completed.into_iter().filter(|r| &r.list_id == id).collect();
                done.sort_by(|a, b| b.completed_at.cmp(&a.completed_at));
                items.extend(done);
                if items.is_empty() {
                    Vec::new()
                } else {
                    vec![Section {
                        title: None,
                        color: None,
                        reminders: cloned(items),
                    }]
                }
            }
        }
    }

    /// One titled section per list, in sidebar order; incomplete before completed in each.
    fn by_list(&self, items: Vec<&Reminder>) -> Vec<Section> {
        let mut out = Vec::new();
        let mut order: Vec<(&str, &str, Option<&String>)> = self
            .data
            .lists
            .iter()
            .map(|l| (l.id.as_str(), l.title.as_str(), l.color.as_ref()))
            .collect();
        // Reminders from a list the sidebar doesn't know (shouldn't happen) still show.
        for r in &items {
            if !order.iter().any(|(id, _, _)| *id == r.list_id) {
                order.push((r.list_id.as_str(), "Other", None));
            }
        }
        for (id, title, color) in order {
            let mut mine: Vec<&Reminder> =
                items.iter().copied().filter(|r| r.list_id == id).collect();
            if mine.is_empty() {
                continue;
            }
            let (mut open, done): (Vec<_>, Vec<_>) = mine
                .drain(..)
                .partition(|r| !r.completed || self.lingering.contains_key(&r.id));
            sort(&mut open);
            open.extend(done);
            out.push(Section::titled(title, color.cloned(), open));
        }
        out
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Counts {
    pub today: usize,
    pub scheduled: usize,
    pub all: usize,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Section {
    pub title: Option<String>,
    /// List colour for list-titled sections.
    pub color: Option<String>,
    pub reminders: Vec<Reminder>,
}

impl Section {
    fn titled(title: &str, color: Option<String>, items: Vec<&Reminder>) -> Section {
        Section {
            title: Some(title.to_string()),
            color,
            reminders: cloned(items),
        }
    }
}

fn cloned<'a>(items: impl IntoIterator<Item = &'a Reminder>) -> Vec<Reminder> {
    items.into_iter().cloned().collect()
}

/// The bridge's order in local time: dated first (earliest first, all-day before timed), then
/// by creation.
fn sort(items: &mut [&Reminder]) {
    items.sort_by_cached_key(|r| {
        let due = r.due.as_ref().and_then(dates::local_due);
        (due.is_none(), due, r.created_at.clone().unwrap_or_default())
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use fruity_bridge_client::reminders::Due;

    fn rem(id: &str, list: &str, due: Option<&str>) -> Reminder {
        Reminder {
            id: id.into(),
            list_id: list.into(),
            title: id.into(),
            notes: None,
            url: None,
            priority: 0,
            completed: false,
            completed_at: None,
            due: due.map(|d| Due {
                date: d.into(),
                time: None,
                time_zone: None,
            }),
            recurring: false,
            recurrence_text: None,
            created_at: None,
            modified_at: None,
        }
    }

    fn state() -> State {
        let mut s = State::new(Config {
            url: "http://127.0.0.1:9".into(),
        });
        s.data.lists = vec![
            List {
                id: "a".into(),
                title: "A".into(),
                color: None,
                account: None,
                editable: true,
                open_count: None,
            },
            List {
                id: "b".into(),
                title: "B".into(),
                color: None,
                account: None,
                editable: true,
                open_count: None,
            },
        ];
        s.data.open = vec![
            rem("late", "a", Some("2026-11-10")),
            rem("today", "b", Some("2026-11-13")),
            rem("later", "a", Some("2026-11-20")),
            rem("undated", "b", None),
        ];
        s
    }

    fn now() -> NaiveDateTime {
        NaiveDate::from_ymd_opt(2026, 11, 13)
            .unwrap()
            .and_hms_opt(9, 0, 0)
            .unwrap()
    }

    #[test]
    fn counts_and_today() {
        let mut s = state();
        assert_eq!(
            s.counts(now().date()),
            Counts {
                today: 2,
                scheduled: 3,
                all: 4
            }
        );
        s.view = View::Today;
        let sections = s.sections(now());
        assert_eq!(sections.len(), 2);
        assert_eq!(sections[0].title.as_deref(), Some("Overdue"));
        assert_eq!(sections[1].reminders[0].id, "today");
    }

    #[test]
    fn lingering_stays_visible_and_deletes_hide() {
        let mut s = state();
        s.view = View::List("b".into());
        let mut done = s.data.open.remove(1);
        done.completed = true;
        s.lingering.insert(done.id.clone(), done);
        let ids: Vec<String> = s.sections(now())[0]
            .reminders
            .iter()
            .map(|r| r.id.clone())
            .collect();
        assert_eq!(ids, ["today", "undated"]);
        s.pending_deletes
            .insert("undated".into(), rem("undated", "b", None));
        assert_eq!(s.sections(now())[0].reminders.len(), 1);
    }

    #[test]
    fn search_spans_lists() {
        let mut s = state();
        s.search = "LATE".into();
        let sections = s.sections(now());
        let ids: Vec<&str> = sections
            .iter()
            .flat_map(|s| s.reminders.iter().map(|r| r.id.as_str()))
            .collect();
        assert_eq!(ids, ["late", "later"]);
    }
}
