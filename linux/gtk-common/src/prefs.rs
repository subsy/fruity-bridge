//! Connection preferences: where the Mac bridge lives, and a connection test that shows what
//! the Mac allows and how to fix what it doesn't.

use std::rc::Rc;

use adw::prelude::*;
use fruity_bridge_client::{Client, Config, Health};
use gtk::glib;

use crate::bg;

/// A macOS permission the bridge reports in `/v1/health`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Permission {
    Reminders,
    NotesRead,
    NotesWrite,
}

impl Permission {
    fn title(self) -> &'static str {
        match self {
            Permission::Reminders => "Reminders Access",
            Permission::NotesRead => "Notes Access",
            Permission::NotesWrite => "Notes Editing",
        }
    }

    fn state(self, h: &Health) -> &str {
        match self {
            Permission::Reminders => &h.reminders,
            Permission::NotesRead => &h.notes,
            Permission::NotesWrite => &h.notes_write,
        }
    }

    fn allowed(self) -> &'static str {
        match self {
            Permission::Reminders => "The bridge can read and save your reminders.",
            Permission::NotesRead => "The bridge can read your notes.",
            Permission::NotesWrite => "The bridge can save changes to your notes.",
        }
    }

    /// What to do on the Mac when it isn't allowed yet.
    fn fix(self) -> &'static str {
        match self {
            Permission::Reminders => {
                "Not allowed yet. On your Mac, open System Settings › Privacy & Security › Reminders and turn on Fruity Bridge."
            }
            Permission::NotesRead => {
                "Not allowed yet. On your Mac, open System Settings › Privacy & Security › Full Disk Access and turn on Fruity Bridge."
            }
            Permission::NotesWrite => {
                "Not allowed yet. On your Mac, open System Settings › Privacy & Security › Automation, then under Fruity Bridge turn on Notes."
            }
        }
    }
}

/// What each app puts in the dialog.
pub struct Options<'a> {
    /// One line on how the app reaches the Mac.
    pub description: &'a str,
    /// Permission rows, the app's own first.
    pub permissions: &'a [Permission],
}

#[derive(Clone)]
struct StatusRow {
    row: adw::ActionRow,
    icon: gtk::Image,
}

impl StatusRow {
    fn new(title: &str) -> StatusRow {
        let row = adw::ActionRow::builder()
            .title(title)
            .use_markup(false)
            .subtitle_lines(4)
            .build();
        let icon = gtk::Image::builder().valign(gtk::Align::Center).build();
        row.add_suffix(&icon);
        StatusRow { row, icon }
    }

    fn set(&self, ok: bool, subtitle: &str) {
        self.row.set_subtitle(subtitle);
        self.icon.set_icon_name(Some(if ok {
            "object-select-symbolic"
        } else {
            "dialog-warning-symbolic"
        }));
        self.icon
            .set_css_classes(&[if ok { "success" } else { "warning" }]);
        self.icon
            .set_tooltip_text(Some(if ok { "Working" } else { "Needs attention" }));
    }
}

/// Opens the dialog over `parent` and tests the connection straight away. Applying a new
/// address saves it to the config file and calls `on_saved` with the new config.
pub fn present(
    parent: &impl IsA<gtk::Widget>,
    current_url: &str,
    options: Options,
    on_saved: impl Fn(Config) + 'static,
) -> adw::PreferencesDialog {
    let dialog = adw::PreferencesDialog::builder()
        .title("Preferences")
        .search_enabled(false)
        .content_width(560)
        .content_height(640)
        .build();
    let page = adw::PreferencesPage::builder()
        .title("Connection")
        .icon_name("network-server-symbolic")
        .build();

    let mut description = options.description.to_string();
    description.push_str(
        "\nThe address is in the bridge's menu on your Mac, e.g. https://my-mac.example-tailnet.ts.net:10000.",
    );
    if std::env::var("FRUITY_BRIDGE_URL").is_ok_and(|u| !u.is_empty()) {
        description.push_str(
            "\nThis session uses the address from FRUITY_BRIDGE_URL instead of the saved one.",
        );
    }
    let group = adw::PreferencesGroup::builder()
        .title("Your Mac")
        .description(glib::markup_escape_text(&description))
        .build();
    let url_row = adw::EntryRow::builder()
        .title("Bridge Address")
        .text(current_url)
        .show_apply_button(true)
        .input_purpose(gtk::InputPurpose::Url)
        .build();
    group.add(&url_row);
    let test = adw::ButtonRow::builder()
        .title("Test Connection")
        .start_icon_name("network-transmit-receive-symbolic")
        .build();
    group.add(&test);
    page.add(&group);

    let results = adw::PreferencesGroup::builder()
        .title("Status")
        .visible(false)
        .build();
    let bridge_row = StatusRow::new("Mac Bridge");
    results.add(&bridge_row.row);
    let permission_rows: Vec<(Permission, StatusRow)> = options
        .permissions
        .iter()
        .map(|p| {
            let row = StatusRow::new(p.title());
            results.add(&row.row);
            (*p, row)
        })
        .collect();
    page.add(&results);

    dialog.add(&page);

    let run_test = Rc::new(glib::clone!(
        #[weak]
        url_row,
        #[weak]
        test,
        #[weak]
        results,
        move || {
            let url = url_row.text().trim().trim_end_matches('/').to_string();
            if url.is_empty() || !test.is_sensitive() {
                return;
            }
            test.set_sensitive(false);
            test.set_title("Testing…");
            let bridge_row = bridge_row.clone();
            let permission_rows = permission_rows.clone();
            glib::spawn_future_local(async move {
                let client = Client::new(&Config { url });
                let health = bg(move || client.health()).await;
                test.set_sensitive(true);
                test.set_title("Test Connection");
                results.set_visible(true);
                show_health(&bridge_row, &permission_rows, health);
            });
        }
    ));

    url_row.connect_apply(glib::clone!(
        #[weak]
        dialog,
        #[strong]
        run_test,
        move |row| {
            let url = row.text().trim().trim_end_matches('/').to_string();
            if url.is_empty() {
                return;
            }
            let config = Config { url };
            let message = match config.save() {
                Ok(()) => "Saved".to_string(),
                Err(e) => format!("Couldn't save the address: {e}"),
            };
            dialog.add_toast(adw::Toast::new(&message));
            on_saved(config);
            run_test();
        }
    ));

    test.connect_activated(glib::clone!(
        #[strong]
        run_test,
        move |_| run_test()
    ));

    dialog.present(Some(parent));
    // Check straight away, so the status is there when the dialog opens.
    run_test();
    dialog
}

fn show_health(
    bridge: &StatusRow,
    permissions: &[(Permission, StatusRow)],
    health: fruity_bridge_client::Result<Health>,
) {
    match health {
        Ok(h) => {
            bridge.set(true, &format!("Connected · bridge version {}", h.version));
            for (p, row) in permissions {
                let (ok, text) = match p.state(&h) {
                    "ok" => (true, p.allowed()),
                    "missing" => (false, p.fix()),
                    _ => (false, "Your Mac reported a problem checking this permission."),
                };
                row.set(ok, text);
                row.row.set_visible(true);
            }
        }
        Err(e) => {
            let text = if e.is_offline() {
                "Can't reach your Mac. Check that it's awake, Tailscale is connected on both machines, and the address is right.".to_string()
            } else {
                format!("Your Mac answered, but something's wrong: {}", e.user_message())
            };
            bridge.set(false, &text);
            for (_, row) in permissions {
                row.row.set_visible(false);
            }
        }
    }
}
