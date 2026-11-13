//! Hidden `--screenshot <path>` mode: drives the window to a scene, renders it to a PNG and
//! quits. Used for visual checks under a virtual display.

use std::rc::Rc;
use std::time::Duration;

use gtk::prelude::*;
use gtk::glib;

use crate::state::ListSource;
use crate::window::Window;

#[derive(Debug, Clone)]
pub struct Shot {
    pub path: String,
    pub width: i32,
    pub height: i32,
    pub dark: bool,
    pub folder: Option<String>,
    pub note: Option<String>,
    pub search: Option<String>,
    pub edit: bool,
    /// Text typed into the editor after it opens.
    pub type_text: Option<String>,
    /// Open the new-note editor instead of a note.
    pub new_note: bool,
    /// Show the folder sidebar page on narrow layouts.
    pub show_folders: bool,
    /// Open Preferences (the connection test runs straight away).
    pub prefs: bool,
}

impl Shot {
    /// Parses and removes screenshot flags from `args`.
    pub fn from_args(args: &mut Vec<String>) -> Option<Shot> {
        let mut take = |flag: &str| -> Option<String> {
            let i = args.iter().position(|a| a == flag)?;
            args.remove(i);
            (i < args.len()).then(|| args.remove(i))
        };
        let path = take("--screenshot")?;
        let (width, height) = take("--size")
            .as_deref()
            .and_then(fruity_bridge_gtk::parse_size)
            .unwrap_or((1180, 760));
        let folder = take("--folder");
        let note = take("--note");
        let search = take("--search");
        let type_text = take("--type");
        let mut flag = |f: &str| match args.iter().position(|a| a == f) {
            Some(i) => {
                args.remove(i);
                true
            }
            None => false,
        };
        let dark = flag("--dark");
        let edit = flag("--edit");
        let new_note = flag("--new");
        let show_folders = flag("--show-folders");
        let prefs = flag("--prefs");
        Some(Shot {
            path,
            width,
            height,
            dark,
            folder,
            note,
            search,
            edit,
            type_text,
            new_note,
            show_folders,
            prefs,
        })
    }
}

/// Steps through the scene on a timer, then captures.
pub fn drive(win: &Rc<Window>) {
    let weak = Rc::downgrade(win);
    let mut step = 0u32;
    let mut ticks = 0u32;
    let mut settle = 0u32;
    glib::timeout_add_local(Duration::from_millis(150), move || {
        let Some(w) = weak.upgrade() else {
            return glib::ControlFlow::Break;
        };
        let Some(shot) = w.shot.borrow().clone() else {
            return glib::ControlFlow::Break;
        };
        ticks += 1;
        if ticks > 120 {
            eprintln!("screenshot: timed out at step {step}; capturing anyway");
            step = 99;
        }
        match step {
            0 => {
                if w.state().folders.is_empty() {
                    return glib::ControlFlow::Continue;
                }
                let folder = shot.folder.as_ref().and_then(|name| {
                    w.state()
                        .folders
                        .iter()
                        .find(|f| &f.name == name)
                        .map(|f| f.id.clone())
                });
                if let Some(id) = folder {
                    w.select_folder(id, !shot.show_folders);
                }
                if let Some(q) = &shot.search {
                    w.set_search_text(q);
                }
                step = 1;
            }
            1 => {
                let want = w.state().source();
                let loaded = w.list_loaded_for.borrow().clone();
                if want.is_none()
                    || loaded != want
                    || matches!(want, Some(ListSource::Search(_))) && w.state().search.is_none()
                {
                    return glib::ControlFlow::Continue;
                }
                if shot.new_note {
                    w.activate_action("win.new-note");
                    step = 3;
                    return glib::ControlFlow::Continue;
                }
                let id = shot.note.as_ref().and_then(|t| {
                    w.state()
                        .notes
                        .iter()
                        .find(|n| &n.title == t)
                        .map(|n| n.id.clone())
                });
                if let Some(id) = id {
                    w.open_note(id, true);
                }
                step = 2;
            }
            2 => {
                let st = w.state();
                let shown = w.shown_id.borrow().clone();
                let ready = shot.note.is_none() || (shown.is_some() && shown == st.selected_id);
                drop(st);
                if !ready || w.pending_images.get() > 0 {
                    return glib::ControlFlow::Continue;
                }
                if shot.edit {
                    w.begin_edit();
                }
                if shot.prefs {
                    w.show_preferences();
                }
                step = 3;
            }
            3 => {
                if let Some(t) = &shot.type_text {
                    w.editor_append(&t.replace("\\n", "\n"));
                }
                step = 4;
            }
            4 => {
                settle += 1;
                if settle >= 6 {
                    step = 99;
                }
            }
            _ => {
                if let Err(e) = fruity_bridge_gtk::capture(&w.win, &shot.path) {
                    eprintln!("screenshot failed: {e}");
                } else {
                    eprintln!("screenshot saved to {}", shot.path);
                }
                if let Some(app) = w.win.application() {
                    app.quit();
                }
                return glib::ControlFlow::Break;
            }
        }
        glib::ControlFlow::Continue
    });
}

impl Window {
    fn activate_action(&self, name: &str) {
        let _ = WidgetExt::activate_action(&self.win, name, None);
    }
}
