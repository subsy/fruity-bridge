use serde::{Deserialize, Serialize};

use crate::{Client, Result, encode_segment};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct List {
    pub id: String,
    pub title: String,
    #[serde(default)]
    pub color: Option<String>,
    #[serde(default)]
    pub account: Option<String>,
    #[serde(default = "yes")]
    pub editable: bool,
    /// `None` when not counted (`?counts=false`).
    #[serde(default)]
    pub open_count: Option<u32>,
}

fn yes() -> bool {
    true
}

fn null_as_default<'de, D: serde::Deserializer<'de>, T: Default + Deserialize<'de>>(d: D) -> std::result::Result<T, D::Error> {
    Ok(Option::<T>::deserialize(d)?.unwrap_or_default())
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Due {
    /// `YYYY-MM-DD`
    pub date: String,
    /// `HH:MM`, absent for all-day.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub time: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub time_zone: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Reminder {
    pub id: String,
    #[serde(default, deserialize_with = "null_as_default")]
    pub list_id: String,
    pub title: String,
    #[serde(default)]
    pub notes: Option<String>,
    #[serde(default)]
    pub url: Option<String>,
    /// 0 none, 1 high, 5 medium, 9 low.
    #[serde(default)]
    pub priority: u8,
    #[serde(default)]
    pub completed: bool,
    #[serde(default)]
    pub completed_at: Option<String>,
    #[serde(default)]
    pub due: Option<Due>,
    #[serde(default)]
    pub recurring: bool,
    #[serde(default)]
    pub recurrence_text: Option<String>,
    #[serde(default)]
    pub created_at: Option<String>,
    #[serde(default)]
    pub modified_at: Option<String>,
}

#[derive(Debug, Clone, Copy)]
pub enum Completed {
    Incomplete,
    /// Completed in the last 30 days (the bridge's default window).
    Complete,
    /// Incomplete, plus completed in the last 30 days.
    All,
}

#[derive(Debug, Clone)]
pub enum Query<'a> {
    List(&'a str, Completed),
    All(Completed),
    /// Incomplete, due today or overdue.
    Today,
    /// Incomplete with any due date.
    Scheduled,
}

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NewReminder {
    pub list_id: String,
    pub title: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub due: Option<Due>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub priority: Option<u8>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
}

/// Fields left `None` are unchanged. `due: Some(None)` clears the due date.
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReminderPatch {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub due: Option<Option<Due>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub priority: Option<u8>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub completed: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub list_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expected_modified_at: Option<String>,
}

#[derive(Deserialize)]
struct Lists {
    lists: Vec<List>,
}

#[derive(Deserialize)]
struct Reminders {
    reminders: Vec<Reminder>,
}

fn completed_param(c: Completed) -> &'static str {
    match c {
        Completed::Incomplete => "false",
        Completed::Complete => "true",
        Completed::All => "all",
    }
}

impl Client {
    /// The lists in sidebar order. `counts: false` skips the (costly) `openCount` per list.
    pub fn reminder_lists(&self, counts: bool) -> Result<Vec<List>> {
        let path = if counts { "/v1/reminders/lists" } else { "/v1/reminders/lists?counts=false" };
        Ok(self.get::<Lists>(path)?.lists)
    }

    pub fn reminder(&self, id: &str) -> Result<Reminder> {
        self.get(&format!("/v1/reminders/{}", encode_segment(id)))
    }

    pub fn reminders(&self, query: Query<'_>) -> Result<Vec<Reminder>> {
        let path = match query {
            Query::List(id, c) => format!("/v1/reminders?list={}&completed={}", encode_segment(id), completed_param(c)),
            Query::All(c) => format!("/v1/reminders?completed={}", completed_param(c)),
            Query::Today => "/v1/reminders?due=today".into(),
            Query::Scheduled => "/v1/reminders?due=scheduled".into(),
        };
        Ok(self.get::<Reminders>(&path)?.reminders)
    }

    pub fn create_reminder(&self, new: &NewReminder) -> Result<Reminder> {
        self.post("/v1/reminders", new)
    }

    pub fn update_reminder(&self, id: &str, patch: &ReminderPatch) -> Result<Reminder> {
        self.patch(&format!("/v1/reminders/{}", encode_segment(id)), patch)
    }

    pub fn delete_reminder(&self, id: &str) -> Result<()> {
        self.delete(&format!("/v1/reminders/{}", encode_segment(id)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tolerates_nulls() {
        let r: Reminder = serde_json::from_str(r#"{"id": "r", "listId": null, "title": "Milk"}"#).unwrap();
        assert_eq!(r.list_id, "");
        let l: List = serde_json::from_str(r#"{"id": "l", "title": "A", "openCount": null}"#).unwrap();
        assert_eq!(l.open_count, None);
    }
}
