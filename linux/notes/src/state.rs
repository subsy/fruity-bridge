//! Plain data and pure helpers shared by the window: folder ordering, dates, cache keys.

use fruity_bridge_client::notes::{Folder, Note, NoteSummary};
use gtk::glib;
use serde::{Deserialize, Serialize};

/// What the middle pane is showing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ListSource {
    Folder(String),
    Search(String),
}

/// An open editor. `note_id` is `None` for a note that hasn't been created yet.
#[derive(Debug, Clone)]
pub struct EditSession {
    pub note_id: Option<String>,
    pub folder_id: String,
    /// `modifiedAt` the edit started from; sent as `expectedModifiedAt`. `None` only for a
    /// new note.
    pub base_modified: Option<String>,
    pub original: String,
    pub saving: bool,
}

#[derive(Debug, Default)]
pub struct State {
    pub folders: Vec<Folder>,
    pub folder_id: Option<String>,
    pub search: Option<String>,
    pub notes: Vec<NoteSummary>,
    pub list_loaded: bool,
    pub selected_id: Option<String>,
    pub note: Option<Note>,
    pub edit: Option<EditSession>,
    pub offline: bool,
    pub list_token: u64,
    pub note_token: u64,
}

impl State {
    pub fn source(&self) -> Option<ListSource> {
        match (&self.search, &self.folder_id) {
            (Some(q), _) if !q.trim().is_empty() => Some(ListSource::Search(q.trim().to_string())),
            (_, Some(f)) => Some(ListSource::Folder(f.clone())),
            _ => None,
        }
    }

    pub fn folder(&self, id: &str) -> Option<&Folder> {
        self.folders.iter().find(|f| f.id == id)
    }

    pub fn current_folder(&self) -> Option<&Folder> {
        self.folder_id.as_deref().and_then(|id| self.folder(id))
    }

    /// Folder a new note goes into: the current one if it can hold notes, else the first regular one.
    pub fn target_folder(&self) -> Option<&Folder> {
        self.current_folder()
            .filter(|f| !f.is_trash && !f.is_smart)
            .or_else(|| self.folders.iter().find(|f| !f.is_trash && !f.is_smart))
    }

    pub fn is_editing(&self) -> bool {
        self.edit.is_some()
    }
}

/// Last place the user was, restored on launch.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct UiState {
    pub folder_id: Option<String>,
    pub note_id: Option<String>,
}

pub const CACHE_FOLDERS: &str = "notes-folders";
pub const CACHE_UI: &str = "notes-ui";

pub fn cache_list_key(folder_id: &str) -> String {
    fruity_bridge_client::cache::key("notes-list", folder_id)
}

pub fn cache_note_key(note_id: &str) -> String {
    fruity_bridge_client::cache::key("notes-note", note_id)
}

#[derive(Debug, Clone, PartialEq)]
pub struct FolderRow {
    pub folder: Folder,
    pub depth: u32,
    /// Set on the first row of each account when there is more than one account.
    pub account_header: Option<String>,
}

/// Sidebar order: per account, folders in API order with children under their parent
/// (depth-first), then that account's Recently Deleted last.
pub fn ordered_folders(folders: &[Folder]) -> Vec<FolderRow> {
    let mut accounts: Vec<Option<String>> = Vec::new();
    for f in folders {
        if !accounts.contains(&f.account) {
            accounts.push(f.account.clone());
        }
    }
    let multi = accounts.len() > 1;
    let known = |id: &Option<String>| {
        id.as_ref()
            .is_some_and(|p| folders.iter().any(|f| &f.id == p))
    };
    let mut out = Vec::new();
    for account in &accounts {
        let start = out.len();
        let mine: Vec<&Folder> = folders.iter().filter(|f| &f.account == account).collect();
        fn walk(
            f: &Folder,
            depth: u32,
            all: &[&Folder],
            out: &mut Vec<FolderRow>,
            seen: &mut Vec<String>,
        ) {
            if seen.contains(&f.id) {
                return;
            }
            seen.push(f.id.clone());
            out.push(FolderRow {
                folder: f.clone(),
                depth,
                account_header: None,
            });
            for c in all
                .iter()
                .filter(|c| c.parent_id.as_deref() == Some(f.id.as_str()) && !c.is_trash)
            {
                walk(c, depth + 1, all, out, seen);
            }
        }
        let mut seen = Vec::new();
        for f in mine.iter().filter(|f| !f.is_trash && !known(&f.parent_id)) {
            walk(f, 0, &mine, &mut out, &mut seen);
        }
        for f in mine.iter().filter(|f| f.is_trash) {
            out.push(FolderRow {
                folder: (*f).clone(),
                depth: 0,
                account_header: None,
            });
        }
        if multi && out.len() > start {
            out[start].account_header =
                Some(account.clone().unwrap_or_else(|| "On My Computer".into()));
        }
    }
    out
}

/// Apple-style list date: time today, "Yesterday", weekday this week, else a date.
pub fn relative_date(then: &glib::DateTime, now: &glib::DateTime) -> String {
    let fmt = |d: &glib::DateTime, f: &str| d.format(f).map(|s| s.to_string()).unwrap_or_default();
    let day = |d: &glib::DateTime| (d.year(), d.day_of_year());
    let days_ago = {
        let a =
            glib::DateTime::from_local(then.year(), then.month(), then.day_of_month(), 0, 0, 0.0);
        let b = glib::DateTime::from_local(now.year(), now.month(), now.day_of_month(), 0, 0, 0.0);
        match (a, b) {
            (Ok(a), Ok(b)) => (b.difference(&a).as_seconds() as f64 / 86400.0).round() as i64,
            _ => i64::MAX,
        }
    };
    if day(then) == day(now) {
        fmt(then, "%H:%M")
    } else if days_ago == 1 {
        "Yesterday".into()
    } else if (2..7).contains(&days_ago) {
        fmt(then, "%A")
    } else if then.year() == now.year() {
        fmt(then, "%-d %B")
    } else {
        fmt(then, "%-d %b %Y")
    }
}

/// Parses an RFC 3339 timestamp into local time.
pub fn parse_time(s: &str) -> Option<glib::DateTime> {
    glib::DateTime::from_iso8601(s, None).ok()?.to_local().ok()
}

pub fn list_date(s: Option<&str>) -> String {
    let (Some(then), Ok(now)) = (s.and_then(parse_time), glib::DateTime::now_local()) else {
        return String::new();
    };
    relative_date(&then, &now)
}

/// Long form shown above an open note: "13 November 2026 at 09:30".
pub fn long_date(s: Option<&str>) -> String {
    s.and_then(parse_time)
        .and_then(|d| d.format("%-d %B %Y at %H:%M").ok())
        .map(|s| s.to_string())
        .unwrap_or_default()
}

/// One tidy line from a snippet.
pub fn clean_snippet(s: Option<&str>) -> String {
    let s = s
        .unwrap_or("")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    if s.is_empty() {
        "No additional text".into()
    } else {
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn folder(id: &str, parent: Option<&str>, trash: bool, account: &str) -> Folder {
        Folder {
            id: id.into(),
            name: id.into(),
            account: Some(account.into()),
            parent_id: parent.map(Into::into),
            note_count: Some(1),
            is_trash: trash,
            is_smart: false,
        }
    }

    #[test]
    fn folders_nest_and_trash_goes_last() {
        let fs = vec![
            folder("trash", None, true, "iCloud"),
            folder("notes", None, false, "iCloud"),
            folder("baking", Some("recipes"), false, "iCloud"),
            folder("recipes", None, false, "iCloud"),
            folder("cakes", Some("baking"), false, "iCloud"),
        ];
        let rows = ordered_folders(&fs);
        let got: Vec<(&str, u32)> = rows
            .iter()
            .map(|r| (r.folder.id.as_str(), r.depth))
            .collect();
        assert_eq!(
            got,
            vec![
                ("notes", 0),
                ("recipes", 0),
                ("baking", 1),
                ("cakes", 2),
                ("trash", 0)
            ]
        );
        assert!(rows.iter().all(|r| r.account_header.is_none()));
    }

    #[test]
    fn multiple_accounts_get_headers_and_own_trash() {
        let fs = vec![
            folder("a", None, false, "iCloud"),
            folder("at", None, true, "iCloud"),
            folder("b", None, false, "Gmail"),
            folder("bt", None, true, "Gmail"),
        ];
        let rows = ordered_folders(&fs);
        let ids: Vec<&str> = rows.iter().map(|r| r.folder.id.as_str()).collect();
        assert_eq!(ids, vec!["a", "at", "b", "bt"]);
        assert_eq!(rows[0].account_header.as_deref(), Some("iCloud"));
        assert_eq!(rows[2].account_header.as_deref(), Some("Gmail"));
    }

    #[test]
    fn orphaned_child_is_shown_at_top_level() {
        let fs = vec![folder("x", Some("missing"), false, "iCloud")];
        assert_eq!(ordered_folders(&fs)[0].depth, 0);
    }

    fn local(y: i32, mo: i32, d: i32, h: i32, mi: i32) -> glib::DateTime {
        glib::DateTime::from_local(y, mo, d, h, mi, 0.0).unwrap()
    }

    #[test]
    fn relative_dates() {
        let now = local(2026, 11, 13, 15, 0);
        assert_eq!(relative_date(&local(2026, 11, 13, 9, 5), &now), "09:05");
        assert_eq!(
            relative_date(&local(2026, 11, 12, 23, 59), &now),
            "Yesterday"
        );
        assert_eq!(relative_date(&local(2026, 11, 10, 8, 0), &now), "Tuesday");
        assert_eq!(relative_date(&local(2026, 10, 1, 8, 0), &now), "1 October");
        assert_eq!(relative_date(&local(2025, 3, 4, 8, 0), &now), "4 Mar 2025");
    }

    #[test]
    fn source_prefers_search() {
        let mut s = State {
            folder_id: Some("f".into()),
            ..Default::default()
        };
        assert_eq!(s.source(), Some(ListSource::Folder("f".into())));
        s.search = Some("  ".into());
        assert_eq!(s.source(), Some(ListSource::Folder("f".into())));
        s.search = Some(" milk ".into());
        assert_eq!(s.source(), Some(ListSource::Search("milk".into())));
    }

    #[test]
    fn snippets_are_one_line() {
        assert_eq!(clean_snippet(Some("a\n  b\tc ")), "a b c");
        assert_eq!(clean_snippet(None), "No additional text");
    }
}
