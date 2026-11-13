//! Middle pane: the note list with search.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use fruity_bridge_client::notes::NoteSummary;
use gtk::pango;
use gtk::prelude::*;

use crate::state::{clean_snippet, list_date};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Empty {
    Folder,
    Trash,
    Search,
}

pub struct NoteList {
    pub page: adw::NavigationPage,
    pub title: adw::WindowTitle,
    pub search_bar: gtk::SearchBar,
    pub search_entry: gtk::SearchEntry,
    pub banner: adw::Banner,
    stack: gtk::Stack,
    list: gtk::ListBox,
    empty: adw::StatusPage,
    notes: RefCell<Vec<NoteSummary>>,
    suppress: Cell<bool>,
}

impl NoteList {
    pub fn new() -> Rc<NoteList> {
        let title = adw::WindowTitle::new("Notes", "");
        let header = adw::HeaderBar::builder().title_widget(&title).build();
        let new_button = gtk::Button::builder()
            .icon_name("document-new-symbolic")
            .action_name("win.new-note")
            .tooltip_text("New Note (Ctrl+N)")
            .build();
        let search_button = gtk::ToggleButton::builder()
            .icon_name("system-search-symbolic")
            .tooltip_text("Search (Ctrl+F)")
            .build();
        header.pack_end(&new_button);
        header.pack_start(&search_button);

        let search_entry = gtk::SearchEntry::builder()
            .placeholder_text("Search all notes")
            .hexpand(true)
            .search_delay(250)
            .build();
        let clamp = adw::Clamp::builder()
            .maximum_size(400)
            .child(&search_entry)
            .build();
        let search_bar = gtk::SearchBar::builder().child(&clamp).build();
        search_bar.connect_entry(&search_entry);
        search_bar
            .bind_property("search-mode-enabled", &search_button, "active")
            .bidirectional()
            .sync_create()
            .build();

        let banner = adw::Banner::new("Can't reach your Mac — showing saved notes");

        let list = gtk::ListBox::builder()
            .selection_mode(gtk::SelectionMode::Single)
            .build();
        list.add_css_class("navigation-sidebar");
        list.add_css_class("note-list");
        list.update_property(&[gtk::accessible::Property::Label("Notes")]);
        let scroller = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vexpand(true)
            .child(&list)
            .build();

        let empty = adw::StatusPage::builder()
            .icon_name("document-edit-symbolic")
            .title("No Notes")
            .build();
        empty.add_css_class("compact");
        let spinner = adw::Spinner::builder()
            .width_request(32)
            .height_request(32)
            .halign(gtk::Align::Center)
            .valign(gtk::Align::Center)
            .build();

        let stack = gtk::Stack::builder()
            .transition_type(gtk::StackTransitionType::Crossfade)
            .build();
        stack.add_named(&scroller, Some("list"));
        stack.add_named(&empty, Some("empty"));
        stack.add_named(&spinner, Some("loading"));

        let toolbar = adw::ToolbarView::new();
        toolbar.add_top_bar(&header);
        toolbar.add_top_bar(&search_bar);
        toolbar.add_top_bar(&banner);
        toolbar.set_content(Some(&stack));
        let page = adw::NavigationPage::builder()
            .title("Notes")
            .tag("list")
            .child(&toolbar)
            .build();

        let this = Rc::new(NoteList {
            page,
            title,
            search_bar,
            search_entry,
            banner,
            stack,
            list,
            empty,
            notes: RefCell::default(),
            suppress: Cell::new(false),
        });

        let weak = Rc::downgrade(&this);
        this.list.set_header_func(move |row, before| {
            let Some(this) = weak.upgrade() else { return };
            let notes = this.notes.borrow();
            let i = row.index() as usize;
            let pinned = notes.get(i).is_some_and(|n| n.pinned);
            let prev_pinned = before
                .and_then(|b| notes.get(b.index() as usize))
                .map(|n| n.pinned);
            let label = match (pinned, prev_pinned) {
                (true, None) => Some("Pinned"),
                (false, Some(true)) => Some("Notes"),
                _ => None,
            };
            row.set_header(
                label
                    .map(|t| {
                        let l = gtk::Label::builder()
                            .label(t)
                            .xalign(0.0)
                            .margin_start(12)
                            .margin_top(if i == 0 { 6 } else { 14 })
                            .margin_bottom(4)
                            .build();
                        l.add_css_class("heading");
                        l.add_css_class("dim-label");
                        l
                    })
                    .as_ref(),
            );
        });
        this
    }

    /// `f(note_id)` when the user picks a note; `activated` on any click (to navigate forward).
    pub fn connect_selected(
        self: &Rc<Self>,
        f: impl Fn(String) + 'static,
        activated: impl Fn() + 'static,
    ) {
        let weak = Rc::downgrade(self);
        self.list.connect_row_selected(move |_, row| {
            let Some(this) = weak.upgrade() else { return };
            if this.suppress.get() {
                return;
            }
            let id = row.and_then(|r| {
                this.notes
                    .borrow()
                    .get(r.index() as usize)
                    .map(|n| n.id.clone())
            });
            if let Some(id) = id {
                f(id);
            }
        });
        self.list.connect_row_activated(move |_, _| activated());
    }

    pub fn show_loading(&self) {
        self.notes.borrow_mut().clear();
        self.list.remove_all();
        self.stack.set_visible_child_name("loading");
    }

    pub fn set_notes(&self, notes: &[NoteSummary], selected: Option<&str>, empty: Empty) {
        if *self.notes.borrow() != notes {
            self.suppress.set(true);
            self.list.remove_all();
            *self.notes.borrow_mut() = notes.to_vec();
            for n in notes {
                self.list.append(&row_widget(n));
            }
            self.suppress.set(false);
        }
        match empty {
            Empty::Folder => {
                self.empty.set_icon_name(Some("document-edit-symbolic"));
                self.empty.set_title("No Notes");
                self.empty
                    .set_description(Some("Press Ctrl+N to write one."));
            }
            Empty::Trash => {
                self.empty.set_icon_name(Some("user-trash-symbolic"));
                self.empty.set_title("Nothing Here");
                self.empty
                    .set_description(Some("Deleted notes stay here for 30 days."));
            }
            Empty::Search => {
                self.empty.set_icon_name(Some("system-search-symbolic"));
                self.empty.set_title("No Results");
                self.empty.set_description(Some("Try a different word."));
            }
        }
        self.stack
            .set_visible_child_name(if notes.is_empty() { "empty" } else { "list" });
        self.select(selected);
    }

    /// Empty state for a folder that has never been loaded while the Mac is unreachable.
    pub fn show_unavailable(&self) {
        self.empty.set_icon_name(Some("network-offline-symbolic"));
        self.empty.set_title("Not Saved on This Computer");
        self.empty
            .set_description(Some("These notes will appear when your Mac is reachable."));
        self.stack.set_visible_child_name("empty");
    }

    pub fn select(&self, id: Option<&str>) {
        self.suppress.set(true);
        let index = id.and_then(|id| self.notes.borrow().iter().position(|n| n.id == id));
        match index.and_then(|i| self.list.row_at_index(i as i32)) {
            Some(row) => self.list.select_row(Some(&row)),
            None => self.list.unselect_all(),
        }
        self.suppress.set(false);
    }

    pub fn first_id(&self) -> Option<String> {
        self.notes.borrow().first().map(|n| n.id.clone())
    }

    /// Focuses the selected row, if any. (Focusing an unselected list would select a row.)
    pub fn focus_selected(&self) -> bool {
        self.list.selected_row().is_some_and(|row| row.grab_focus())
    }
}

fn row_widget(n: &NoteSummary) -> gtk::ListBoxRow {
    let outer = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(3)
        .margin_top(7)
        .margin_bottom(7)
        .margin_start(4)
        .margin_end(4)
        .build();

    let top = gtk::Box::builder().spacing(6).build();
    let title_text = if n.title.trim().is_empty() {
        "New Note"
    } else {
        n.title.trim()
    };
    let title = gtk::Label::builder()
        .label(title_text)
        .xalign(0.0)
        .hexpand(true)
        .ellipsize(pango::EllipsizeMode::End)
        .build();
    title.add_css_class("heading");
    top.append(&title);
    let badge = |icon: &str, tip: &str| {
        let i = gtk::Image::from_icon_name(icon);
        i.add_css_class("dim-label");
        i.set_pixel_size(14);
        i.set_tooltip_text(Some(tip));
        i
    };
    if n.locked {
        top.append(&badge("system-lock-screen-symbolic", "Locked"));
    }
    if n.pinned {
        top.append(&badge("view-pin-symbolic", "Pinned"));
    }
    outer.append(&top);

    let bottom = gtk::Box::builder().spacing(8).build();
    let date = gtk::Label::new(Some(&list_date(n.modified_at.as_deref())));
    date.add_css_class("caption-heading");
    date.add_css_class("numeric");
    date.add_css_class("note-date");
    let snippet_text = if n.locked {
        "Locked".to_string()
    } else {
        clean_snippet(n.snippet.as_deref())
    };
    let snippet = gtk::Label::builder()
        .label(&snippet_text)
        .xalign(0.0)
        .hexpand(true)
        .ellipsize(pango::EllipsizeMode::End)
        .build();
    snippet.add_css_class("caption");
    snippet.add_css_class("dim-label");
    bottom.append(&date);
    bottom.append(&snippet);
    if n.has_checklist {
        bottom.append(&badge("checkbox-checked-symbolic", "Has a checklist"));
    }
    if n.has_attachments {
        bottom.append(&badge("mail-attachment-symbolic", "Has attachments"));
    }
    outer.append(&bottom);

    let row = gtk::ListBoxRow::builder().child(&outer).build();
    row.update_property(&[gtk::accessible::Property::Label(&format!(
        "{title_text}, {snippet_text}"
    ))]);
    row
}
