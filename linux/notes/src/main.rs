//! Notes: browse, search and edit Apple Notes from Linux through the Mac bridge.

mod attachments;
mod editor;
mod folders;
mod list;
mod noteview;
mod render;
mod screenshot;
mod state;
mod window;

use std::cell::RefCell;
use std::rc::Rc;

use adw::prelude::*;
use gtk::{gdk, gio, glib};

use screenshot::Shot;
use window::Window;

const APP_ID: &str = "com.plgeek.FruityNotes";

const CSS: &str = r#"
.note-body label { line-height: 1.45; }
.note-para, .note-list-item, .note-quote-text, .note-marker { font-size: 1.07em; }
.note-title { font-size: 2em; font-weight: 800; line-height: 1.2; margin-bottom: 6px; }
.note-heading { font-size: 1.45em; font-weight: 700; line-height: 1.25; margin-top: 14px; }
.note-subheading { font-size: 1.17em; font-weight: 700; margin-top: 8px; }
.note-marker { color: alpha(currentColor, 0.55); font-weight: 600; font-feature-settings: "tnum"; }
.note-check { margin-top: 2px; margin-right: 2px; }
.note-block { margin-top: 6px; margin-bottom: 6px; }
.note-code {
  background: alpha(currentColor, 0.06);
  border-radius: 10px;
  padding: 12px 16px;
  margin-top: 4px;
  margin-bottom: 4px;
}
.note-code-text { font-family: "Adwaita Mono", monospace; font-size: 0.95em; line-height: 1.5; }
.note-quote {
  border-left: 3px solid var(--accent-bg-color);
  padding: 2px 0 2px 16px;
  margin-top: 4px;
  margin-bottom: 4px;
}
.note-quote-text { font-style: italic; }
.note-image { border-radius: 12px; }
.note-image-placeholder { background: alpha(currentColor, 0.05); border-radius: 12px; }
.note-file { padding: 0; }
.note-file-icon { color: var(--accent-color); }
.note-table { border: 1px solid alpha(currentColor, 0.15); border-radius: 10px; }
.note-cell {
  padding: 8px 16px;
  border-right: 1px solid alpha(currentColor, 0.12);
  border-bottom: 1px solid alpha(currentColor, 0.12);
}
.note-cell.last-col { border-right: none; }
.note-cell.last-row { border-bottom: none; }
.note-info {
  background: alpha(currentColor, 0.06);
  border-radius: 999px;
  padding: 5px 14px;
  margin-bottom: 4px;
}
.note-info image { opacity: 0.7; }
.note-date { color: var(--accent-color); }
.note-editor, .note-editor > text { background: transparent; }
.note-editor { font-size: 1.07em; }
.note-list row { border-radius: 10px; }
statuspage.dim-status image { opacity: 0.5; }
"#;

fn load_css() {
    // Selectable note text shouldn't light up whenever it takes focus.
    if let Some(settings) = gtk::Settings::default() {
        settings.set_gtk_label_select_on_focus(false);
    }
    let provider = gtk::CssProvider::new();
    provider.load_from_string(CSS);
    if let Some(display) = gdk::Display::default() {
        gtk::style_context_add_provider_for_display(
            &display,
            &provider,
            gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
        );
        // Debug builds run from the source tree; find the app icons next to the crate.
        #[cfg(debug_assertions)]
        {
            let data = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("data");
            if data.is_dir() {
                gtk::IconTheme::for_display(&display).add_search_path(data);
            }
        }
    }
}

fn shortcuts_dialog() -> adw::ShortcutsDialog {
    fruity_bridge_gtk::shortcuts_dialog(
        "Notes",
        &[
            ("New Note", "<Control>n"),
            ("Search", "<Control>f"),
            ("Edit Note", "<Control>e"),
            ("Save", "<Control>s"),
            ("Refresh", "<Control>r"),
        ],
    )
}

type Slot = Rc<RefCell<Option<Rc<Window>>>>;

fn add_app_action(
    app: &adw::Application,
    window: &Slot,
    name: &str,
    f: impl Fn(&adw::Application, Option<Rc<Window>>) + 'static,
) {
    let action = gio::SimpleAction::new(name, None);
    let app_ref = app.downgrade();
    let w = window.clone();
    action.connect_activate(move |_, _| {
        if let Some(app) = app_ref.upgrade() {
            let win = w.borrow().clone();
            f(&app, win);
        }
    });
    app.add_action(&action);
}

fn main() -> glib::ExitCode {
    let mut args: Vec<String> = std::env::args().collect();
    let shot = Shot::from_args(&mut args);

    let mut flags = gio::ApplicationFlags::empty();
    if shot.is_some() {
        // Never hand off to (or show up in) an instance on the user's desktop.
        flags |= gio::ApplicationFlags::NON_UNIQUE;
    }
    let app = adw::Application::builder()
        .application_id(APP_ID)
        .flags(flags)
        .build();
    let window: Slot = Rc::default();

    app.connect_startup(|_| load_css());

    let w = window.clone();
    app.connect_activate(move |app| {
        if let Some(win) = w.borrow().as_ref() {
            win.present();
            return;
        }
        if let Some(s) = &shot {
            let scheme = if s.dark {
                adw::ColorScheme::ForceDark
            } else {
                adw::ColorScheme::ForceLight
            };
            adw::StyleManager::default().set_color_scheme(scheme);
        }
        let win = Window::new(app, shot.clone());
        win.present();
        win.start();
        *w.borrow_mut() = Some(win);
    });

    add_app_action(&app, &window, "quit", |app, win| match win {
        // Closing asks first if a note has unsaved changes.
        Some(w) => w.win.close(),
        None => app.quit(),
    });
    add_app_action(&app, &window, "preferences", |_, win| {
        if let Some(w) = win {
            w.show_preferences();
        }
    });
    add_app_action(&app, &window, "shortcuts", |_, win| {
        if let Some(w) = win {
            shortcuts_dialog().present(Some(&w.win));
        }
    });
    add_app_action(&app, &window, "about", |_, win| {
        fruity_bridge_gtk::about_dialog(
            "Notes",
            APP_ID,
            env!("CARGO_PKG_VERSION"),
            "Your Apple Notes on Linux, through the bridge on your Mac.",
        )
        .present(win.as_ref().map(|w| &w.win));
    });

    for (action, accels) in [
        ("app.quit", &["<Control>q"][..]),
        ("app.preferences", &["<Control>comma"]),
        ("app.shortcuts", &["<Control>question"]),
        ("win.new-note", &["<Control>n"]),
        ("win.search", &["<Control>f"]),
        ("win.save", &["<Control>s"]),
        ("win.edit", &["<Control>e"]),
        ("win.refresh", &["<Control>r", "F5"]),
    ] {
        app.set_accels_for_action(action, accels);
    }

    app.run_with_args(&args)
}
