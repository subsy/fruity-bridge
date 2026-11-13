//! The details editor: an AdwDialog (a bottom sheet on narrow windows) that saves on close.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use adw::prelude::*;
use fruity_bridge_client::reminders::{Due, List, Reminder, ReminderPatch};
use chrono::{Datelike, Days, NaiveDate, Timelike};
use gtk::glib;

use crate::dates;

/// Priority choices in menu order, as EventKit values.
const PRIORITIES: [(&str, u8); 4] = [("None", 0), ("Low", 9), ("Medium", 5), ("High", 1)];

fn priority_index(p: u8) -> u32 {
    match p {
        0 => 0,
        1..=4 => 3,
        5 => 2,
        _ => 1,
    }
}

pub struct EditorArgs<'a> {
    pub reminder: &'a Reminder,
    pub lists: &'a [List],
    /// `None` when editing is allowed, else why not (shown in a banner).
    pub read_only: Option<&'a str>,
}

pub fn present(
    parent: &impl IsA<gtk::Widget>,
    args: EditorArgs,
    on_save: impl Fn(ReminderPatch) + 'static,
    on_delete: impl Fn() + 'static,
) -> adw::Dialog {
    let rem = args.reminder.clone();
    let editable = args.read_only.is_none();

    let dialog = adw::Dialog::builder()
        .title("Details")
        .content_width(460)
        .content_height(700)
        .build();
    let header = adw::HeaderBar::new();
    header.set_show_end_title_buttons(false);
    let done = gtk::Button::builder()
        .label("Done")
        .css_classes(["suggested-action"])
        .build();
    header.pack_end(&done);

    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&header);
    if let Some(reason) = args.read_only {
        let banner = adw::Banner::builder().title(reason).revealed(true).build();
        toolbar.add_top_bar(&banner);
    }
    let page = adw::PreferencesPage::new();
    toolbar.set_content(Some(&page));
    dialog.set_child(Some(&toolbar));

    // ----- Title and notes
    let main = adw::PreferencesGroup::new();
    let title_row = adw::EntryRow::builder()
        .title("Title")
        .text(&rem.title)
        .build();
    main.add(&title_row);

    let notes_buffer = gtk::TextBuffer::new(None);
    notes_buffer.set_text(rem.notes.as_deref().unwrap_or(""));
    let notes_view = gtk::TextView::builder()
        .buffer(&notes_buffer)
        .wrap_mode(gtk::WrapMode::WordChar)
        .css_classes(["notes-view"])
        .height_request(56)
        .accepts_tab(false)
        .editable(editable)
        .build();
    let placeholder = gtk::Label::builder()
        .label("Notes")
        .xalign(0.0)
        .yalign(0.0)
        .css_classes(["notes-placeholder"])
        .can_target(false)
        .build();
    placeholder.set_visible(notes_buffer.char_count() == 0);
    notes_buffer.connect_changed(glib::clone!(
        #[weak]
        placeholder,
        move |b| placeholder.set_visible(b.char_count() == 0)
    ));
    let overlay = gtk::Overlay::builder().child(&notes_view).build();
    overlay.add_overlay(&placeholder);
    notes_view.update_property(&[gtk::accessible::Property::Label("Notes")]);
    let notes_row = gtk::ListBoxRow::builder()
        .child(&overlay)
        .activatable(false)
        .selectable(false)
        .build();
    main.add(&notes_row);
    page.add(&main);

    // ----- Date and time
    let when = adw::PreferencesGroup::builder()
        .title("Date and Time")
        .build();
    // Shown and edited in local time; a changed time is sent with this computer's zone.
    let orig_local = rem.due.as_ref().and_then(dates::local_due);
    let orig_day = orig_local.map(|d| d.day);
    let orig_time = orig_local.and_then(|d| d.time);
    let day: Rc<Cell<NaiveDate>> =
        Rc::new(Cell::new(orig_day.unwrap_or_else(|| dates::now().date())));

    let date_switch = adw::SwitchRow::builder()
        .title("Date")
        .active(orig_day.is_some())
        .build();
    date_switch.add_prefix(&gtk::Image::from_icon_name("x-office-calendar-symbolic"));
    when.add(&date_switch);

    let day_button = gtk::MenuButton::builder()
        .valign(gtk::Align::Center)
        .css_classes(["flat"])
        .always_show_arrow(true)
        .build();
    let day_row = adw::ActionRow::builder().title("Day").build();
    day_row.add_suffix(&day_button);
    day_row.set_activatable_widget(Some(&day_button));
    when.add(&day_row);

    let time_switch = adw::SwitchRow::builder()
        .title("Time")
        .active(orig_time.is_some())
        .build();
    time_switch.add_prefix(&gtk::Image::from_icon_name("alarm-symbolic"));
    when.add(&time_switch);

    let start = orig_time.unwrap_or_else(|| chrono::NaiveTime::from_hms_opt(9, 0, 0).unwrap());
    let hour = time_spin(23.0, start.hour() as f64);
    let minute = time_spin(59.0, start.minute() as f64);
    hour.update_property(&[gtk::accessible::Property::Label("Hour")]);
    minute.update_property(&[gtk::accessible::Property::Label("Minute")]);
    let time_box = gtk::Box::builder()
        .spacing(6)
        .margin_top(6)
        .margin_bottom(6)
        .margin_start(6)
        .margin_end(6)
        .build();
    time_box.append(&hour);
    time_box.append(
        &gtk::Label::builder()
            .label(":")
            .css_classes(["title-2"])
            .build(),
    );
    time_box.append(&minute);
    let time_button = gtk::MenuButton::builder()
        .valign(gtk::Align::Center)
        .css_classes(["flat"])
        .always_show_arrow(true)
        .popover(&gtk::Popover::builder().child(&time_box).build())
        .build();
    let show_time = glib::clone!(
        #[weak]
        time_button,
        #[weak]
        hour,
        #[weak]
        minute,
        move || time_button.set_label(&format!(
            "{:02}:{:02}",
            hour.value() as u32,
            minute.value() as u32
        ))
    );
    show_time();
    hour.connect_value_changed(glib::clone!(
        #[strong]
        show_time,
        move |_| show_time()
    ));
    minute.connect_value_changed(move |_| show_time());
    let time_row = adw::ActionRow::builder().title("At").build();
    time_row.add_suffix(&time_button);
    time_row.set_activatable_widget(Some(&time_button));
    when.add(&time_row);

    if rem.recurring {
        let repeat = adw::ActionRow::builder()
            .title("Repeat")
            .subtitle(format!(
                "{} · change on your iPhone or Mac",
                rem.recurrence_text.as_deref().unwrap_or("Repeats")
            ))
            .build();
        repeat.add_prefix(&gtk::Image::from_icon_name(
            "media-playlist-repeat-symbolic",
        ));
        when.add(&repeat);
    }
    page.add(&when);

    // Calendar popover with quick picks.
    let calendar = gtk::Calendar::new();
    let quick = gtk::Box::builder()
        .spacing(6)
        .halign(gtk::Align::Center)
        .css_classes(["date-quick"])
        .build();
    let popover_box = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(8)
        .margin_top(6)
        .margin_bottom(6)
        .margin_start(6)
        .margin_end(6)
        .build();
    popover_box.append(&quick);
    popover_box.append(&calendar);
    let popover = gtk::Popover::builder().child(&popover_box).build();
    day_button.set_popover(Some(&popover));

    let refresh_day = Rc::new(glib::clone!(
        #[strong]
        day,
        #[weak]
        day_button,
        #[weak]
        date_switch,
        #[weak]
        calendar,
        move || {
            let d = day.get();
            day_button.set_label(&d.format("%a %-d %b %Y").to_string());
            date_switch.set_subtitle(&if date_switch.is_active() {
                dates::long_date(d)
            } else {
                String::new()
            });
            if let Ok(dt) =
                glib::DateTime::from_local(d.year(), d.month() as i32, d.day() as i32, 12, 0, 0.0)
            {
                calendar.set_date(&dt);
            }
        }
    ));
    let today = dates::now().date();
    for (label, offset) in [("Today", 0u64), ("Tomorrow", 1), ("Next Week", 7)] {
        let b = gtk::Button::builder()
            .label(label)
            .css_classes(["pill", "small"])
            .build();
        let (day, refresh_day, popover) = (day.clone(), refresh_day.clone(), popover.clone());
        b.connect_clicked(move |_| {
            day.set(today + Days::new(offset));
            refresh_day();
            popover.popdown();
        });
        quick.append(&b);
    }
    calendar.connect_day_selected(glib::clone!(
        #[strong]
        day,
        #[strong]
        refresh_day,
        #[weak]
        popover,
        move |cal| {
            let dt = cal.date();
            if let Some(d) =
                NaiveDate::from_ymd_opt(dt.year(), dt.month() as u32, dt.day_of_month() as u32)
                && d != day.get()
            {
                day.set(d);
                refresh_day();
                popover.popdown();
            }
        }
    ));

    let sync_visibility = Rc::new(glib::clone!(
        #[weak]
        date_switch,
        #[weak]
        time_switch,
        #[weak]
        day_row,
        #[weak]
        time_row,
        #[strong]
        refresh_day,
        move || {
            let on = date_switch.is_active();
            day_row.set_visible(on);
            time_switch.set_visible(on);
            time_row.set_visible(on && time_switch.is_active());
            refresh_day();
        }
    ));
    sync_visibility();
    date_switch.connect_active_notify(glib::clone!(
        #[strong]
        sync_visibility,
        move |_| sync_visibility()
    ));
    time_switch.connect_active_notify(glib::clone!(
        #[strong]
        sync_visibility,
        move |_| sync_visibility()
    ));

    // ----- Priority, list, URL
    let details = adw::PreferencesGroup::new();
    let priority_row = adw::ComboRow::builder()
        .title("Priority")
        .model(&gtk::StringList::new(&PRIORITIES.map(|(name, _)| name)))
        .selected(priority_index(rem.priority))
        .build();
    priority_row.add_prefix(&gtk::Image::from_icon_name("dialog-warning-symbolic"));
    details.add(&priority_row);

    let choices: Vec<&List> = args
        .lists
        .iter()
        .filter(|l| l.editable || l.id == rem.list_id)
        .collect();
    let list_row = adw::ComboRow::builder()
        .title("List")
        .model(&gtk::StringList::new(
            &choices.iter().map(|l| l.title.as_str()).collect::<Vec<_>>(),
        ))
        .selected(
            choices
                .iter()
                .position(|l| l.id == rem.list_id)
                .unwrap_or(0) as u32,
        )
        .build();
    list_row.add_prefix(&gtk::Image::from_icon_name("view-list-bullet-symbolic"));
    let list_ids: Vec<String> = choices.iter().map(|l| l.id.clone()).collect();
    details.add(&list_row);

    let url_row = adw::EntryRow::builder()
        .title("URL")
        .text(rem.url.as_deref().unwrap_or(""))
        .input_purpose(gtk::InputPurpose::Url)
        .build();
    let open = gtk::Button::builder()
        .icon_name("adw-external-link-symbolic")
        .valign(gtk::Align::Center)
        .tooltip_text("Open Link")
        .css_classes(["flat"])
        .build();
    open.connect_clicked(glib::clone!(
        #[weak]
        url_row,
        move |b| {
            let text = url_row.text().trim().to_string();
            if text.is_empty() {
                return;
            }
            let uri = if text.contains("://") {
                text
            } else {
                format!("https://{text}")
            };
            let window = b.root().and_downcast::<gtk::Window>();
            gtk::UriLauncher::new(&uri).launch(
                window.as_ref(),
                gtk::gio::Cancellable::NONE,
                |_| {},
            );
        }
    ));
    url_row.add_suffix(&open);
    details.add(&url_row);
    page.add(&details);

    // ----- Delete
    let deleting = Rc::new(Cell::new(false));
    if editable {
        let danger = adw::PreferencesGroup::new();
        let delete = adw::ButtonRow::builder()
            .title("Delete Reminder")
            .start_icon_name("user-trash-symbolic")
            .build();
        delete.add_css_class("destructive-action");
        let on_delete = RefCell::new(Some(on_delete));
        delete.connect_activated(glib::clone!(
            #[weak]
            dialog,
            #[strong]
            deleting,
            move |_| {
                deleting.set(true);
                dialog.close();
                if let Some(f) = on_delete.borrow_mut().take() {
                    f();
                }
            }
        ));
        danger.add(&delete);
        page.add(&danger);
    } else {
        for w in [
            title_row.upcast_ref::<gtk::Widget>(),
            date_switch.upcast_ref(),
            day_row.upcast_ref(),
            time_switch.upcast_ref(),
            time_row.upcast_ref(),
            priority_row.upcast_ref(),
            list_row.upcast_ref(),
        ] {
            w.set_sensitive(false);
        }
        url_row.set_editable(false);
    }

    done.connect_clicked(glib::clone!(
        #[weak]
        dialog,
        move |_| {
            dialog.close();
        }
    ));
    // Enter in the title saves and closes, as in Apple's info popover.
    title_row.connect_entry_activated(glib::clone!(
        #[weak]
        dialog,
        move |_| {
            dialog.close();
        }
    ));

    dialog.connect_closed(move |_| {
        if !editable || deleting.get() {
            return;
        }
        let mut patch = ReminderPatch::default();
        let title = title_row.text().trim().to_string();
        if !title.is_empty() && title != rem.title {
            patch.title = Some(title);
        }
        let (start, end) = notes_buffer.bounds();
        let notes = notes_buffer
            .text(&start, &end, false)
            .trim_end()
            .to_string();
        if notes != rem.notes.as_deref().unwrap_or("").trim_end() {
            patch.notes = Some(notes);
        }
        let new_local = date_switch.is_active().then(|| dates::LocalDue {
            day: day.get(),
            time: time_switch.is_active().then(|| {
                chrono::NaiveTime::from_hms_opt(hour.value() as u32, minute.value() as u32, 0)
                    .unwrap_or_default()
            }),
        });
        if new_local != orig_local {
            patch.due = Some(new_local.map(|d| Due {
                date: d.day.format("%Y-%m-%d").to_string(),
                time: d.time.map(|t| t.format("%H:%M").to_string()),
                time_zone: d.time.and_then(|_| dates::local_zone_name()),
            }));
        }
        let priority = PRIORITIES[priority_row.selected() as usize % PRIORITIES.len()].1;
        if priority_index(priority) != priority_index(rem.priority) {
            patch.priority = Some(priority);
        }
        if let Some(list) = list_ids.get(list_row.selected() as usize)
            && *list != rem.list_id
        {
            patch.list_id = Some(list.clone());
        }
        let url = url_row.text().trim().to_string();
        if url != rem.url.as_deref().unwrap_or("").trim() {
            patch.url = Some(url);
        }
        if is_empty(&patch) {
            return;
        }
        patch.expected_modified_at = rem.modified_at.clone();
        on_save(patch);
    });
    dialog.present(Some(parent));
    dialog
}

fn is_empty(p: &ReminderPatch) -> bool {
    p.title.is_none()
        && p.notes.is_none()
        && p.due.is_none()
        && p.priority.is_none()
        && p.completed.is_none()
        && p.list_id.is_none()
        && p.url.is_none()
}

fn time_spin(max: f64, value: f64) -> gtk::SpinButton {
    let spin = gtk::SpinButton::with_range(0.0, max, 1.0);
    spin.set_value(value);
    spin.set_wrap(true);
    spin.set_numeric(true);
    spin.set_width_chars(2);
    spin.set_orientation(gtk::Orientation::Vertical);
    spin.add_css_class("time-spin");
    spin.connect_output(|s| {
        s.set_text(&format!("{:02}", s.value() as u32));
        glib::Propagation::Stop
    });
    spin
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn priorities_round_trip() {
        for (_, p) in PRIORITIES {
            assert_eq!(PRIORITIES[priority_index(p) as usize].1, p);
        }
        assert_eq!(priority_index(3), 3);
        assert_eq!(priority_index(7), 1);
    }
}
