//! One reminder in the content list: round check, priority marks, title, notes preview and a
//! line of details (due chip, repeat icon, link, list name).

use std::cell::Cell;
use std::rc::Rc;

use fruity_bridge_client::reminders::{List, Reminder};
use chrono::NaiveDateTime;
use gtk::prelude::*;

use crate::{dates, style};

pub struct RowContext<'a> {
    pub list: Option<&'a List>,
    /// Smart lists mix lists, so each row names its list.
    pub show_list: bool,
    pub editable: bool,
    pub now: NaiveDateTime,
}

/// The pieces of a row the window updates after it's built.
#[derive(Clone)]
pub struct RowHandle {
    pub row: gtk::ListBoxRow,
    revealer: gtk::Revealer,
    check: gtk::Button,
    title: gtk::Label,
    checked: Rc<Cell<bool>>,
}

impl RowHandle {
    /// Shows the row as checked (filled circle, struck-through title) or not.
    pub fn set_checked(&self, on: bool) {
        self.checked.set(on);
        if on {
            self.check.add_css_class("checked");
            self.title.add_css_class("done");
            self.check.set_tooltip_text(Some("Mark as Not Completed"));
        } else {
            self.check.remove_css_class("checked");
            self.title.remove_css_class("done");
            self.title.remove_css_class("completed");
            self.check.set_tooltip_text(Some("Mark as Completed"));
        }
    }

    /// Slides the row closed; `done` runs once it has collapsed.
    pub fn collapse(&self, done: impl FnOnce() + 'static) {
        self.revealer.set_reveal_child(false);
        let duration = self.revealer.transition_duration();
        glib_timeout(duration + 30, done);
    }
}

fn glib_timeout(ms: u32, f: impl FnOnce() + 'static) {
    gtk::glib::timeout_add_local_once(std::time::Duration::from_millis(ms.into()), f);
}

pub fn priority_marks(priority: u8) -> Option<&'static str> {
    match priority {
        1..=4 => Some("!!!"),
        5 => Some("!!"),
        6..=9 => Some("!"),
        _ => None,
    }
}

fn link_host(url: &str) -> String {
    let rest = url.split_once("://").map_or(url, |(_, r)| r);
    let host = rest.split(['/', '?', '#']).next().unwrap_or(rest);
    host.trim_start_matches("www.").to_string()
}

/// Called with a reminder id and its new completed state when its check is clicked.
pub type OnToggle = Rc<dyn Fn(&str, bool)>;

pub fn build(rem: &Reminder, ctx: &RowContext, on_toggle: OnToggle) -> RowHandle {
    let color = ctx.list.and_then(|l| l.color.as_deref());

    // Check button
    let dot = gtk::Box::builder().css_classes(["check-dot"]).build();
    let check = gtk::Button::builder()
        .child(&dot)
        .css_classes(["reminder-check"])
        .valign(gtk::Align::Start)
        .halign(gtk::Align::Center)
        .sensitive(ctx.editable)
        .build();

    // Title line
    let title_line = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    if let Some(marks) = priority_marks(rem.priority) {
        let p = gtk::Label::builder()
            .label(marks)
            .css_classes(["priority"])
            .valign(gtk::Align::Start)
            .build();
        title_line.append(&p);
    }
    let title = gtk::Label::builder()
        .label(if rem.title.trim().is_empty() {
            "Untitled"
        } else {
            rem.title.as_str()
        })
        .xalign(0.0)
        .wrap(true)
        .wrap_mode(gtk::pango::WrapMode::WordChar)
        .hexpand(true)
        .css_classes(["reminder-title"])
        .build();
    if rem.completed {
        title.add_css_class("completed");
    }
    title_line.append(&title);

    let text = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(3)
        .hexpand(true)
        .css_classes(["reminder-text"])
        .build();
    text.append(&title_line);

    if let Some(notes) = rem
        .notes
        .as_deref()
        .map(str::trim)
        .filter(|n| !n.is_empty())
    {
        let first = notes
            .lines()
            .find(|l| !l.trim().is_empty())
            .unwrap_or(notes);
        let label = gtk::Label::builder()
            .label(first)
            .xalign(0.0)
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .single_line_mode(true)
            .css_classes(["reminder-notes", "caption"])
            .build();
        text.append(&label);
    }

    // Details line
    let meta = gtk::Box::builder()
        .spacing(8)
        .css_classes(["reminder-meta"])
        .build();
    if let Some(due) = &rem.due {
        let chip = gtk::Label::builder()
            .label(dates::due_label(due, ctx.now.date()))
            .css_classes(["due-chip"])
            .valign(gtk::Align::Center)
            .build();
        if !rem.completed && dates::is_overdue(due, ctx.now) {
            chip.add_css_class("overdue");
        }
        meta.append(&chip);
    }
    if rem.recurring {
        let icon = gtk::Image::builder()
            .icon_name("media-playlist-repeat-symbolic")
            .pixel_size(14)
            .css_classes(["meta-icon"])
            .tooltip_text(rem.recurrence_text.as_deref().unwrap_or("Repeats"))
            .build();
        meta.append(&icon);
    }
    if let Some(url) = rem.url.as_deref().filter(|u| !u.trim().is_empty()) {
        let b = gtk::Box::new(gtk::Orientation::Horizontal, 4);
        b.append(
            &gtk::Image::builder()
                .icon_name("insert-link-symbolic")
                .pixel_size(14)
                .css_classes(["meta-icon"])
                .build(),
        );
        b.append(
            &gtk::Label::builder()
                .label(link_host(url))
                .css_classes(["caption", "dim-label"])
                .ellipsize(gtk::pango::EllipsizeMode::End)
                .build(),
        );
        b.set_tooltip_text(Some(url));
        meta.append(&b);
    }
    if let Some(list) = ctx.list.filter(|_| ctx.show_list) {
        let l = gtk::Label::builder()
            .label(&list.title)
            .css_classes(["list-badge"])
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .build();
        meta.append(&l);
    }
    if meta.first_child().is_some() {
        text.append(&meta);
    }

    let hbox = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    hbox.append(&check);
    hbox.append(&text);

    let revealer = gtk::Revealer::builder()
        .child(&hbox)
        .reveal_child(true)
        .transition_type(gtk::RevealerTransitionType::SlideUp)
        .transition_duration(220)
        .build();
    let row = gtk::ListBoxRow::builder()
        .child(&revealer)
        .activatable(true)
        .build();
    row.set_widget_name(&rem.id);
    style::set_color(&row, color);
    row.update_property(&[gtk::accessible::Property::Label(&rem.title)]);

    let handle = RowHandle {
        row,
        revealer,
        check: check.clone(),
        title,
        checked: Rc::new(Cell::new(false)),
    };
    handle.set_checked(rem.completed);
    if rem.completed {
        handle.title.remove_css_class("done");
        handle.title.add_css_class("completed");
    }

    let id = rem.id.clone();
    let h = handle.clone();
    check.connect_clicked(move |_| {
        let on = !h.checked.get();
        h.set_checked(on);
        on_toggle(&id, on);
    });
    handle
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn marks_and_hosts() {
        assert_eq!(priority_marks(1), Some("!!!"));
        assert_eq!(priority_marks(5), Some("!!"));
        assert_eq!(priority_marks(9), Some("!"));
        assert_eq!(priority_marks(0), None);
        assert_eq!(
            link_host("https://www.tailscale.com/kb/1312/serve"),
            "tailscale.com"
        );
        assert_eq!(link_host("example.org?q=1"), "example.org");
    }
}
