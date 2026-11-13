//! Reminders: Apple Reminders on Linux, through the Fruity Bridge on your Mac.

mod dates;
mod editor;
mod row;
mod sidebar;
mod state;
mod style;
mod window;

use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;

use adw::prelude::*;
use gtk::{gio, glib};

pub const APP_ID: &str = "com.plgeek.FruityReminders";

/// Hidden options for rendering screenshots headlessly, e.g.
/// `--screenshot out.png --size 1100x720 --dark --view Shopping --edit Milk`.
#[derive(Default)]
struct Shot {
    path: Option<String>,
    size: Option<(i32, i32)>,
    dark: bool,
    view: Option<String>,
    sidebar: bool,
    edit: Option<String>,
    search: Option<String>,
    prefs: bool,
    completed: bool,
}

fn parse_args() -> Shot {
    let mut shot = Shot::default();
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--screenshot" => shot.path = args.next(),
            "--size" => shot.size = args.next().as_deref().and_then(fruity_bridge_gtk::parse_size),
            "--dark" => shot.dark = true,
            "--view" => shot.view = args.next(),
            "--sidebar" => shot.sidebar = true,
            "--edit" => shot.edit = args.next(),
            "--search" => shot.search = args.next(),
            "--prefs" => shot.prefs = true,
            "--show-completed" => shot.completed = true,
            _ => {}
        }
    }
    shot
}

fn main() -> glib::ExitCode {
    let shot = Rc::new(parse_args());
    // Screenshot runs mustn't hand off to (or be mistaken for) a running instance.
    let flags = if shot.path.is_some() {
        gio::ApplicationFlags::NON_UNIQUE
    } else {
        gio::ApplicationFlags::empty()
    };
    let app = adw::Application::builder()
        .application_id(APP_ID)
        .flags(flags)
        .build();

    app.connect_startup(|app| {
        style::load();
        gtk::Window::set_default_icon_name(APP_ID);
        let quit = gio::SimpleAction::new("quit", None);
        quit.connect_activate(glib::clone!(
            #[weak]
            app,
            move |_, _| {
                // The app exits once its windows are gone and pending deletes have been sent.
                for w in app.windows() {
                    w.close();
                }
            }
        ));
        app.add_action(&quit);
        for (action, accels) in [
            ("app.quit", &["<Control>q"][..]),
            ("win.new-reminder", &["<Control>n"]),
            ("win.search", &["<Control>f"]),
            ("win.refresh", &["<Control>r", "F5"]),
            ("win.preferences", &["<Control>comma"]),
            ("win.show-completed", &["<Control><Shift>h"]),
            ("win.shortcuts", &["<Control>question"]),
        ] {
            app.set_accels_for_action(action, accels);
        }
    });

    let window: Rc<RefCell<Option<Rc<window::Window>>>> = Rc::new(RefCell::new(None));
    app.connect_activate(move |app| {
        if let Some(win) = window.borrow().as_ref() {
            win.present();
            return;
        }
        if shot.dark {
            adw::StyleManager::default().set_color_scheme(adw::ColorScheme::ForceDark);
        }
        let win = window::Window::new(app);
        if let Some((w, h)) = shot.size {
            win.window.set_default_size(w, h);
        }
        win.present();
        if shot.path.is_some() {
            stage_screenshot(app, &win, shot.clone());
        }
        *window.borrow_mut() = Some(win);
    });

    // GTK gets no arguments: the hidden screenshot options are ours.
    app.run_with_args(&[std::env::args().next().unwrap_or_default()])
}

fn stage_screenshot(app: &adw::Application, win: &Rc<window::Window>, shot: Rc<Shot>) {
    let app = app.clone();
    let w = win.clone();
    win.after_first_load(move || {
        if shot.completed {
            ActionGroupExt::activate_action(&w.window, "show-completed", None);
        }
        if let Some(view) = &shot.view {
            w.select_named(view);
        }
        if let Some(text) = &shot.search {
            w.search_for(text);
        }
        if let Some(title) = &shot.edit {
            w.edit_titled(title);
        }
        if shot.prefs {
            w.show_preferences();
        }
        if shot.sidebar {
            w.show_content(false);
        }
        let path = shot.path.clone().unwrap_or_default();
        glib::timeout_add_local_once(Duration::from_millis(1500), move || {
            match fruity_bridge_gtk::capture(&w.window, &path) {
                Ok(()) => eprintln!("saved {path}"),
                Err(e) => eprintln!("screenshot failed: {e}"),
            }
            app.quit();
        });
    });
}
