use serde::{Deserialize, Serialize};

use crate::{Client, Result, encode_segment};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Folder {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub account: Option<String>,
    #[serde(default)]
    pub parent_id: Option<String>,
    #[serde(default)]
    pub note_count: Option<u32>,
    #[serde(default)]
    pub is_trash: bool,
    #[serde(default)]
    pub is_smart: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct NoteSummary {
    pub id: String,
    #[serde(default)]
    pub folder_id: Option<String>,
    /// Defaults to empty so a partial write response (just `id`) still decodes.
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub snippet: Option<String>,
    #[serde(default)]
    pub created_at: Option<String>,
    #[serde(default)]
    pub modified_at: Option<String>,
    #[serde(default)]
    pub pinned: bool,
    #[serde(default)]
    pub locked: bool,
    #[serde(default)]
    pub editable: bool,
    #[serde(default)]
    pub has_attachments: bool,
    #[serde(default)]
    pub has_checklist: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct Run {
    pub text: String,
    #[serde(default)]
    pub bold: bool,
    #[serde(default)]
    pub italic: bool,
    #[serde(default)]
    pub underline: bool,
    #[serde(default)]
    pub strikethrough: bool,
    #[serde(default)]
    pub link: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct Attachment {
    pub id: String,
    /// image, pdf, drawing, audio, video, link, table, file, other
    pub kind: String,
    #[serde(default)]
    pub uti: Option<String>,
    #[serde(default)]
    pub filename: Option<String>,
    /// Path on the bridge (e.g. `/v1/notes/attachments/…`) when bytes can be served.
    #[serde(default)]
    pub url: Option<String>,
    /// Link cards: the web address the card points to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub href: Option<String>,
    /// Link cards: the page title.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// Scan galleries: a bridge path per page (`url` is the first).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pages: Vec<String>,
    /// When known; changes when the bytes do, so it belongs in cache keys.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub modified_at: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum Block {
    Title { runs: Vec<Run> },
    Heading { runs: Vec<Run> },
    Subheading { runs: Vec<Run> },
    Body { runs: Vec<Run> },
    Monospace { runs: Vec<Run> },
    Quote { runs: Vec<Run> },
    Bullet { #[serde(default)] indent: u32, runs: Vec<Run> },
    Dash { #[serde(default)] indent: u32, runs: Vec<Run> },
    Numbered { #[serde(default)] indent: u32, #[serde(default)] number: u32, runs: Vec<Run> },
    Checklist { #[serde(default)] indent: u32, #[serde(default)] checked: bool, runs: Vec<Run> },
    Attachment { attachment: Attachment },
    Table { rows: Vec<Vec<String>> },
    /// Any block type this client doesn't know yet.
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Note {
    #[serde(flatten)]
    pub summary: NoteSummary,
    #[serde(default)]
    pub blocks: Vec<Block>,
    /// Edit format; present only when `editable`.
    #[serde(default)]
    pub markdown: Option<String>,
}

#[derive(Debug, Clone)]
pub enum NoteQuery<'a> {
    Folder(&'a str),
    Search(&'a str),
    Recent(u32),
}

#[derive(Deserialize)]
struct Folders {
    folders: Vec<Folder>,
}

#[derive(Deserialize)]
struct Notes {
    notes: Vec<NoteSummary>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct NewNote<'a> {
    folder_id: &'a str,
    markdown: &'a str,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct UpdateNote<'a> {
    markdown: &'a str,
    expected_modified_at: &'a str,
}

/// How long to keep re-reading a note whose write response was partial.
const REFETCH_TRIES: u32 = 3;

impl Client {
    pub fn note_folders(&self) -> Result<Vec<Folder>> {
        Ok(self.get::<Folders>("/v1/notes/folders")?.folders)
    }

    pub fn notes(&self, query: NoteQuery<'_>) -> Result<Vec<NoteSummary>> {
        let path = match query {
            NoteQuery::Folder(id) => format!("/v1/notes?folder={}", encode_segment(id)),
            NoteQuery::Search(q) => format!("/v1/notes?q={}", encode_segment(q)),
            NoteQuery::Recent(n) => format!("/v1/notes?recent={n}"),
        };
        Ok(self.get::<Notes>(&path)?.notes)
    }

    pub fn note(&self, id: &str) -> Result<Note> {
        self.get(&format!("/v1/notes/{}", encode_segment(id)))
    }

    /// Creates a note. See [`Client::update_note`] for what a partial response means.
    pub fn create_note(&self, folder_id: &str, markdown: &str) -> Result<Note> {
        let note: Note = self.post("/v1/notes", &NewNote { folder_id, markdown })?;
        Ok(self.complete_write(note, None))
    }

    /// Replaces a note's text. `expected_modified_at` is the `modifiedAt` the edit started from.
    ///
    /// The write succeeded whenever this returns `Ok`. If the bridge couldn't re-read the note
    /// in time it answers with just the id; the note is then fetched again, and if that still
    /// fails the partial note comes back with `modified_at: None` (callers should reload it).
    pub fn update_note(&self, id: &str, markdown: &str, expected_modified_at: &str) -> Result<Note> {
        let note: Note =
            self.put(&format!("/v1/notes/{}", encode_segment(id)), &UpdateNote { markdown, expected_modified_at })?;
        Ok(self.complete_write(note, Some(expected_modified_at)))
    }

    fn complete_write(&self, note: Note, previous_modified_at: Option<&str>) -> Note {
        if note.summary.modified_at.is_some() {
            return note;
        }
        for attempt in 0..REFETCH_TRIES {
            if attempt > 0 {
                std::thread::sleep(std::time::Duration::from_secs(1));
            }
            match self.note(&note.summary.id) {
                // Still the copy from before the write: the store hasn't caught up yet.
                Ok(fresh) if fresh.summary.modified_at.is_some()
                    && fresh.summary.modified_at.as_deref() != previous_modified_at =>
                {
                    return fresh;
                }
                Ok(_) => {}
                Err(_) => break,
            }
        }
        note
    }

    pub fn delete_note(&self, id: &str) -> Result<()> {
        self.delete(&format!("/v1/notes/{}", encode_segment(id)))
    }

    /// Streams an attachment (`path` is its `url` field) into `out`; returns the content type.
    pub fn download_attachment(&self, path: &str, out: &mut dyn std::io::Write) -> Result<String> {
        self.download(path, out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn partial_write_response_decodes() {
        let n: Note = serde_json::from_str(r#"{"id": "x", "modifiedAt": null}"#).unwrap();
        assert_eq!(n.summary.id, "x");
        assert!(n.summary.modified_at.is_none());
        let n: Note = serde_json::from_str(r#"{"id": "x"}"#).unwrap();
        assert!(n.blocks.is_empty());
    }

    #[test]
    fn link_cards_and_galleries_decode() {
        let b: Block = serde_json::from_str(
            r#"{"type": "attachment", "attachment": {"id": "a", "kind": "link", "href": "https://e.com",
                "title": "E", "pages": ["/v1/notes/attachments/p1"], "modifiedAt": "2026-11-13T09:30:00Z"}}"#,
        )
        .unwrap();
        let Block::Attachment { attachment } = b else { panic!() };
        assert_eq!(attachment.href.as_deref(), Some("https://e.com"));
        assert_eq!(attachment.pages.len(), 1);
        assert!(attachment.modified_at.is_some());
    }
}
