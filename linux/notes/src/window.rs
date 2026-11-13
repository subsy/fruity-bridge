//! The main window: three collapsing panes, data flow, editing, live updates and offline mode.

use std::cell::{Cell, RefCell};
use std::rc::{Rc, Weak};

use adw::prelude::*;
use fruity_bridge_client::notes::{Folder, Note, NoteQuery, NoteSummary};
use fruity_bridge_client::{Client, Config, Error, Event, Subscription, cache};
use fruity_bridge_gtk::{bg, prefs};
use gtk::{gio, glib};

use crate::attachments::{self, Textures};
use crate::editor::Palette;
use crate::folders::FolderSidebar;
use crate::list::{Empty, NoteList};
use crate::noteview::NoteView;
use crate::render;
use crate::screenshot::Shot;
use crate::state::{
    CACHE_FOLDERS, CACHE_UI, EditSession, ListSource, State, UiState, cache_list_key,
    cache_note_key, long_date,
};

pub struct Window {
    pub win: adw::ApplicationWindow,
    toasts: adw::ToastOverlay,
    outer: adw::NavigationSplitView,
    inner: adw::NavigationSplitView,
    folders: Rc<FolderSidebar>,
    list: Rc<NoteList>,
    view: NoteView,
    state: RefCell<State>,
    config: RefCell<Config>,
    client: RefCell<Client>,
    events: RefCell<Option<Subscription>>,
    textures: RefCell<Textures>,
    pub(crate) pending_images: Cell<usize>,
    /// Id of the note currently drawn in the reading view.
    pub(crate) shown_id: RefCell<Option<String>>,
    pub(crate) list_loaded_for: RefCell<Option<ListSource>>,
    pub(crate) shot: RefCell<Option<Shot>>,
}

fn menu() -> gio::Menu {
    let menu = gio::Menu::new();
    let s1 = gio::Menu::new();
    s1.append(Some("_Refresh"), Some("win.refresh"));
    menu.append_section(None, &s1);
    let s2 = gio::Menu::new();
    s2.append(Some("_Preferences"), Some("app.preferences"));
    s2.append(Some("_Keyboard Shortcuts"), Some("app.shortcuts"));
    s2.append(Some("_About Notes"), Some("app.about"));
    menu.append_section(None, &s2);
    menu
}

impl Window {
    pub fn new(app: &adw::Application, shot: Option<Shot>) -> Rc<Window> {
        let config = Config::load();
        let client = Client::new(&config);

        let folders = FolderSidebar::new(&menu().upcast());
        let list = NoteList::new();
        let view = NoteView::new();

        let inner = adw::NavigationSplitView::builder()
            .sidebar(&list.page)
            .content(&view.page)
            .min_sidebar_width(260.0)
            .max_sidebar_width(360.0)
            .sidebar_width_fraction(0.34)
            .build();
        let inner_page = adw::NavigationPage::builder()
            .title("Notes")
            .tag("inner")
            .child(&inner)
            .build();
        let outer = adw::NavigationSplitView::builder()
            .sidebar(&folders.page)
            .content(&inner_page)
            .min_sidebar_width(200.0)
            .max_sidebar_width(260.0)
            .sidebar_width_fraction(0.2)
            .build();
        let toasts = adw::ToastOverlay::new();
        toasts.set_child(Some(&outer));

        let win = adw::ApplicationWindow::builder()
            .application(app)
            .title("Notes")
            .default_width(1180)
            .default_height(760)
            .width_request(360)
            .height_request(294)
            .content(&toasts)
            .build();
        if let Some(s) = &shot {
            win.set_default_size(s.width, s.height);
        }

        let mid =
            adw::Breakpoint::new(adw::BreakpointCondition::parse("max-width: 860sp").unwrap());
        mid.add_setter(&outer, "collapsed", Some(&true.to_value()));
        mid.add_setter(&inner, "sidebar-width-fraction", Some(&0.38.to_value()));
        win.add_breakpoint(mid);
        let narrow =
            adw::Breakpoint::new(adw::BreakpointCondition::parse("max-width: 540sp").unwrap());
        narrow.add_setter(&outer, "collapsed", Some(&true.to_value()));
        narrow.add_setter(&inner, "collapsed", Some(&true.to_value()));
        win.add_breakpoint(narrow);

        let this = Rc::new(Window {
            win,
            toasts,
            outer,
            inner,
            folders,
            list,
            view,
            state: RefCell::default(),
            config: RefCell::new(config),
            client: RefCell::new(client),
            events: RefCell::default(),
            textures: RefCell::default(),
            pending_images: Cell::new(0),
            shown_id: RefCell::default(),
            list_loaded_for: RefCell::default(),
            shot: RefCell::new(shot),
        });
        this.setup_actions();
        this.connect_signals();
        this.update_chrome();
        this.view
            .editor
            .update_palette(&Palette::current(&this.win));
        this
    }

    fn weak(self: &Rc<Self>) -> Weak<Self> {
        Rc::downgrade(self)
    }

    fn client(&self) -> Client {
        self.client.borrow().clone()
    }

    pub fn toast(&self, text: &str) {
        let t = adw::Toast::new(text);
        t.set_timeout(4);
        self.toasts.add_toast(t);
    }

    // -----------------------------------------------------------------------------------------
    // Setup

    fn setup_actions(self: &Rc<Self>) {
        let add = |name: &str, f: fn(&Rc<Window>)| {
            let action = gio::SimpleAction::new(name, None);
            let weak = self.weak();
            action.connect_activate(move |_, _| {
                if let Some(this) = weak.upgrade() {
                    f(&this);
                }
            });
            self.win.add_action(&action);
        };
        add("new-note", |w| w.new_note());
        add("search", |w| w.start_search());
        add("save", |w| w.save());
        add("edit", |w| w.begin_edit());
        add("cancel-edit", |w| w.cancel_edit());
        add("delete", |w| w.confirm_delete());
        add("refresh", |w| w.refresh_all(true));
    }

    fn action(&self, name: &str) -> Option<gio::SimpleAction> {
        self.win
            .lookup_action(name)
            .and_downcast::<gio::SimpleAction>()
    }

    fn connect_signals(self: &Rc<Self>) {
        let w = self.weak();
        let w2 = self.weak();
        self.folders.connect_selected(
            move |id| {
                // Selection only loads; activation (click, Enter) navigates. Focus entering the
                // list also selects a row, which must not jump pages on narrow layouts.
                if let Some(this) = w.upgrade() {
                    this.select_folder(id, false);
                }
            },
            move || {
                if let Some(this) = w2.upgrade() {
                    this.outer.set_show_content(true);
                }
            },
        );

        let w = self.weak();
        let w2 = self.weak();
        self.list.connect_selected(
            move |id| {
                if let Some(this) = w.upgrade() {
                    this.open_note(id, false);
                }
            },
            move || {
                if let Some(this) = w2.upgrade()
                    && this.state.borrow().selected_id.is_some()
                {
                    this.inner.set_show_content(true);
                }
            },
        );

        let w = self.weak();
        self.list.search_entry.connect_search_changed(move |entry| {
            if let Some(this) = w.upgrade() {
                let text = entry.text().to_string();
                if this.state.borrow().search.as_deref().unwrap_or("") != text {
                    this.state.borrow_mut().search = Some(text);
                    this.load_list(false);
                }
            }
        });
        let w = self.weak();
        self.list
            .search_bar
            .connect_search_mode_enabled_notify(move |bar| {
                if let Some(this) = w.upgrade()
                    && !bar.is_search_mode()
                    && this.state.borrow().search.is_some()
                {
                    this.state.borrow_mut().search = None;
                    this.list.search_entry.set_text("");
                    this.load_list(false);
                }
            });

        let w = self.weak();
        self.inner.connect_collapsed_notify(move |_| {
            if let Some(this) = w.upgrade() {
                this.update_chrome();
            }
        });

        // Restyle text tags when dark mode or the accent colour changes.
        let sm = adw::StyleManager::default();
        let w = self.weak();
        let restyle = move || {
            if let Some(this) = w.upgrade() {
                this.view
                    .editor
                    .update_palette(&Palette::current(&this.win));
                if !this.state.borrow().is_editing() {
                    this.show_note(true);
                }
            }
        };
        let r = restyle.clone();
        sm.connect_dark_notify(move |_| r());
        sm.connect_accent_color_notify(move |_| restyle());

        let w = self.weak();
        self.view.editor.view.buffer().connect_changed(move |_| {
            if let Some(this) = w.upgrade() {
                this.update_chrome();
            }
        });

        let w = self.weak();
        self.win.connect_close_request(move |win| {
            let Some(this) = w.upgrade() else {
                return glib::Propagation::Proceed;
            };
            if !this.is_dirty() {
                return glib::Propagation::Proceed;
            }
            let win = win.clone();
            let weak = this.weak();
            this.confirm_discard(move || {
                if let Some(this) = weak.upgrade() {
                    this.state.borrow_mut().edit = None;
                }
                win.close();
            });
            glib::Propagation::Stop
        });
    }

    /// Shows saved data straight away, then fetches fresh data and follows live events.
    pub fn start(self: &Rc<Self>) {
        let ui: UiState = cache::load(CACHE_UI).unwrap_or_default();
        let folders: Option<Vec<Folder>> = cache::load(CACHE_FOLDERS);
        {
            let mut st = self.state.borrow_mut();
            st.folder_id = ui.folder_id.clone();
            st.selected_id = ui.note_id.clone();
            if let Some(f) = &folders {
                st.folders = f.clone();
            }
        }
        if let Some(f) = &folders {
            self.folders.set_folders(f, ui.folder_id.as_deref());
        }
        let list_cached = self.show_cached_list();
        let note_cached = self.show_cached_note();
        if self.config.borrow().is_set() {
            self.load_folders();
            if self.state.borrow().folder_id.is_some() {
                self.load_list(list_cached);
            }
            if let Some(id) = ui.note_id {
                self.load_note(id, note_cached);
            }
            self.follow_events();
        } else {
            // First run: nothing to load until the user gives the bridge address.
            self.show_preferences();
        }
        gio::spawn_blocking(attachments::prune);
        if self.shot.borrow().is_some() {
            crate::screenshot::drive(self);
        }
    }

    pub fn present(&self) {
        self.win.present();
    }

    /// Follows the bridge's event stream; a new call replaces (and stops) the previous one.
    fn follow_events(self: &Rc<Self>) {
        // Stop the old stream first, so its last events can't land after the new one starts.
        self.events.replace(None);
        let config = self.config.borrow().clone();
        let subscription =
            fruity_bridge_gtk::follow_events(&config, self.weak(), |this, ev| match ev {
                Event::Connected | Event::Notes => this.refresh_all(true),
                Event::Disconnected => this.set_offline(true),
                Event::Reminders => {}
            });
        self.events.replace(Some(subscription));
    }

    /// Points the app at a new bridge (saved from Preferences).
    fn set_config(self: &Rc<Self>, config: Config) {
        *self.client.borrow_mut() = Client::new(&config);
        *self.config.borrow_mut() = config;
        self.follow_events();
        self.refresh_all(true);
    }

    pub fn show_preferences(self: &Rc<Self>) {
        let url = self.config.borrow().url.clone();
        let weak = self.weak();
        prefs::present(
            &self.win,
            &url,
            prefs::Options {
                description: "Notes reads and saves your Apple Notes through the Fruity Bridge app on your Mac, over Tailscale.",
                permissions: &[
                    prefs::Permission::NotesRead,
                    prefs::Permission::NotesWrite,
                    prefs::Permission::Reminders,
                ],
            },
            move |config| {
                if let Some(this) = weak.upgrade() {
                    this.set_config(config);
                }
            },
        );
    }

    // -----------------------------------------------------------------------------------------
    // Chrome: banners, buttons, titles, action states

    fn set_offline(self: &Rc<Self>, offline: bool) {
        if self.state.borrow().offline == offline {
            return;
        }
        self.state.borrow_mut().offline = offline;
        self.update_chrome();
        if offline {
            // The event stream retries with a growing backoff; poll gently as well so the app
            // recovers within seconds of the Mac coming back.
            let weak = self.weak();
            glib::timeout_add_local(std::time::Duration::from_secs(6), move || {
                let Some(this) = weak.upgrade() else {
                    return glib::ControlFlow::Break;
                };
                if !this.state.borrow().offline {
                    return glib::ControlFlow::Break;
                }
                this.refresh_all(true);
                glib::ControlFlow::Continue
            });
        }
    }

    pub(crate) fn is_dirty(&self) -> bool {
        let st = self.state.borrow();
        st.edit
            .as_ref()
            .is_some_and(|e| self.view.editor.text() != e.original)
    }

    fn update_chrome(&self) {
        let st = self.state.borrow();
        let editing = st.edit.is_some();
        let saving = st.edit.as_ref().is_some_and(|e| e.saving);
        let note = st
            .note
            .as_ref()
            .filter(|n| st.selected_id.as_deref() == Some(n.summary.id.as_str()));
        let in_trash = |n: &Note| {
            n.summary
                .folder_id
                .as_deref()
                .and_then(|f| st.folder(f))
                .is_some_and(|f| f.is_trash)
        };
        // Saving needs the `modifiedAt` the edit starts from.
        let can_edit = note.is_some_and(|n| {
            n.summary.editable
                && n.markdown.is_some()
                && n.summary.modified_at.is_some()
                && !in_trash(n)
        });

        // One banner, over the widest visible pane.
        self.list
            .banner
            .set_revealed(st.offline && self.inner.is_collapsed());
        self.view.banner.set_revealed(st.offline);

        let set = |name: &str, on: bool| {
            if let Some(a) = self.action(name) {
                a.set_enabled(on);
            }
        };
        set("edit", can_edit && !editing && !st.offline);
        set("save", editing && !saving && !st.offline);
        set("cancel-edit", editing && !saving);
        set(
            "delete",
            note.is_some_and(|n| !in_trash(n)) && !editing && !st.offline,
        );
        set(
            "new-note",
            !editing && !st.offline && st.target_folder().is_some(),
        );

        self.view
            .edit_button
            .set_visible(!editing && note.is_some_and(|n| !n.summary.locked));
        self.view
            .delete_button
            .set_visible(!editing && note.is_some_and(|n| !in_trash(n)));
        self.view.save_button.set_visible(editing);
        // Save or Cancel are the ways out of the editor; this also frees header space on phones.
        self.view.page.set_can_pop(!editing);
        self.view.cancel_button.set_visible(editing);
        self.view
            .save_button
            .set_label(if saving { "Saving…" } else { "_Save" });
        self.view.edit_button.set_tooltip_text(Some(if can_edit {
            "Edit (Ctrl+E)"
        } else if st.offline {
            "Can't edit while your Mac is unreachable"
        } else {
            "Edit this note on your iPhone or Mac"
        }));

        if editing {
            let new = st.edit.as_ref().is_some_and(|e| e.note_id.is_none());
            let dirty = st
                .edit
                .as_ref()
                .is_some_and(|e| self.view.editor.text() != e.original);
            self.view
                .title
                .set_title(if new { "New Note" } else { "Editing" });
            self.view
                .title
                .set_subtitle(if dirty { "Unsaved changes" } else { "" });
        } else {
            self.view.title.set_title("");
            self.view.title.set_subtitle("");
        }

        // List title.
        match st.source() {
            Some(ListSource::Search(_)) => {
                self.list.title.set_title("Search");
                self.list.page.set_title("Search");
            }
            _ => {
                let name = st
                    .current_folder()
                    .map(|f| f.name.clone())
                    .unwrap_or_else(|| "Notes".into());
                self.list.title.set_title(&name);
                self.list.page.set_title(&name);
            }
        }
        let n = st.notes.len();
        let count = match (&st.source(), n) {
            (_, 0) if !st.list_loaded => String::new(),
            (Some(ListSource::Search(_)), 1) => "1 result".into(),
            (Some(ListSource::Search(_)), n) => format!("{n} results"),
            (_, 1) => "1 note".into(),
            (_, n) => format!("{n} notes"),
        };
        self.list.title.set_subtitle(&count);
        if let Some(n) = note {
            self.view.page.set_title(if n.summary.title.is_empty() {
                "Note"
            } else {
                &n.summary.title
            });
        }
    }

    fn save_ui_state(&self) {
        let st = self.state.borrow();
        let ui = UiState {
            folder_id: st.folder_id.clone(),
            note_id: st.selected_id.clone(),
        };
        gio::spawn_blocking(move || cache::store(CACHE_UI, &ui));
    }

    // -----------------------------------------------------------------------------------------
    // Loading

    pub fn refresh_all(self: &Rc<Self>, soft: bool) {
        self.load_folders();
        if self.state.borrow().source().is_some() {
            self.load_list(soft);
        }
        let open = {
            let st = self.state.borrow();
            if st.is_editing() {
                None
            } else {
                st.selected_id.clone()
            }
        };
        if let Some(id) = open {
            self.load_note(id, true);
        }
    }

    fn handle_error(self: &Rc<Self>, e: &Error, quiet_offline: bool) {
        if e.is_offline() {
            self.set_offline(true);
            if !quiet_offline {
                self.toast(&describe(e));
            }
        } else {
            self.toast(&describe(e));
        }
    }

    fn load_folders(self: &Rc<Self>) {
        let client = self.client();
        let weak = self.weak();
        glib::spawn_future_local(async move {
            let res = bg(move || {
                let r = client.note_folders();
                if let Ok(f) = &r {
                    cache::store(CACHE_FOLDERS, f);
                }
                r
            })
            .await;
            let Some(this) = weak.upgrade() else { return };
            match res {
                Ok(folders) => {
                    this.set_offline(false);
                    let need_folder = {
                        let mut st = this.state.borrow_mut();
                        st.folders = folders.clone();
                        let valid = st
                            .folder_id
                            .as_deref()
                            .is_some_and(|id| st.folder(id).is_some());
                        if !valid {
                            st.folder_id = crate::state::ordered_folders(&folders)
                                .into_iter()
                                .find(|r| !r.folder.is_trash)
                                .map(|r| r.folder.id);
                        }
                        !valid
                    };
                    let selected = this.state.borrow().folder_id.clone();
                    this.folders.set_folders(&folders, selected.as_deref());
                    if need_folder {
                        this.load_list(false);
                    }
                    this.update_chrome();
                }
                Err(e) => this.handle_error(&e, true),
            }
        });
    }

    fn show_cached_list(&self) -> bool {
        let Some(ListSource::Folder(id)) = self.state.borrow().source() else {
            return false;
        };
        let Some(notes) = cache::load::<Vec<NoteSummary>>(&cache_list_key(&id)) else {
            return false;
        };
        self.apply_list(notes);
        true
    }

    fn apply_list(&self, notes: Vec<NoteSummary>) {
        let (selected, empty) = {
            let mut st = self.state.borrow_mut();
            st.notes = notes;
            st.list_loaded = true;
            let empty = match st.source() {
                Some(ListSource::Search(_)) => Empty::Search,
                _ if st.current_folder().is_some_and(|f| f.is_trash) => Empty::Trash,
                _ => Empty::Folder,
            };
            (st.selected_id.clone(), empty)
        };
        let notes = self.state.borrow().notes.clone();
        self.list.set_notes(&notes, selected.as_deref(), empty);
        self.update_chrome();
    }

    pub(crate) fn load_list(self: &Rc<Self>, soft: bool) {
        let Some(source) = self.state.borrow().source() else {
            return;
        };
        let token = {
            let mut st = self.state.borrow_mut();
            st.list_token += 1;
            st.list_token
        };
        *self.list_loaded_for.borrow_mut() = None;
        if !soft && !self.show_cached_list() {
            self.state.borrow_mut().list_loaded = false;
            self.list.show_loading();
            self.update_chrome();
        }
        let client = self.client();
        let weak = self.weak();
        let src = source.clone();
        glib::spawn_future_local(async move {
            let res = bg(move || {
                let r = match &src {
                    ListSource::Folder(id) => client.notes(NoteQuery::Folder(id)),
                    ListSource::Search(q) => client.notes(NoteQuery::Search(q)),
                };
                if let (Ok(notes), ListSource::Folder(id)) = (&r, &src) {
                    cache::store(&cache_list_key(id), notes);
                }
                r
            })
            .await;
            let Some(this) = weak.upgrade() else { return };
            if this.state.borrow().list_token != token {
                return;
            }
            match res {
                Ok(notes) => {
                    this.set_offline(false);
                    this.apply_list(notes);
                    *this.list_loaded_for.borrow_mut() = Some(source.clone());
                    this.auto_open_first();
                }
                Err(e) => {
                    if !this.state.borrow().list_loaded {
                        this.apply_list(Vec::new());
                        if e.is_offline() {
                            this.state.borrow_mut().list_loaded = false;
                            this.list.show_unavailable();
                            this.update_chrome();
                        }
                    }
                    *this.list_loaded_for.borrow_mut() = Some(source.clone());
                    this.handle_error(&e, true);
                }
            }
        });
    }

    /// On wide layouts, open the first note when nothing is open (as Notes does).
    fn auto_open_first(self: &Rc<Self>) {
        if self.inner.is_collapsed()
            || self
                .shot
                .borrow()
                .as_ref()
                .is_some_and(|s| s.note.is_some())
        {
            return;
        }
        let first = {
            let st = self.state.borrow();
            if st.selected_id.is_some()
                || st.is_editing()
                || matches!(st.source(), Some(ListSource::Search(_)))
            {
                return;
            }
            self.list.first_id()
        };
        if let Some(id) = first {
            self.open_note(id, false);
        }
    }

    fn show_cached_note(self: &Rc<Self>) -> bool {
        let Some(id) = self.state.borrow().selected_id.clone() else {
            return false;
        };
        let Some(note) = cache::load::<Note>(&cache_note_key(&id)) else {
            return false;
        };
        self.state.borrow_mut().note = Some(note);
        self.show_note(true);
        true
    }

    fn load_note(self: &Rc<Self>, id: String, soft: bool) {
        let token = {
            let mut st = self.state.borrow_mut();
            st.note_token += 1;
            st.note_token
        };
        if !soft && !self.show_cached_note() {
            self.view.stack.set_visible_child_name("loading");
        }
        let client = self.client();
        let weak = self.weak();
        let fetch_id = id.clone();
        glib::spawn_future_local(async move {
            let res = bg(move || {
                let r = client.note(&fetch_id);
                if let Ok(n) = &r {
                    cache::store(&cache_note_key(&fetch_id), n);
                }
                r
            })
            .await;
            let Some(this) = weak.upgrade() else { return };
            {
                let st = this.state.borrow();
                if st.note_token != token || st.selected_id.as_deref() != Some(id.as_str()) {
                    return;
                }
            }
            match res {
                Ok(note) => {
                    this.set_offline(false);
                    if this.state.borrow().is_editing() {
                        return;
                    }
                    let changed = this.state.borrow().note.as_ref() != Some(&note);
                    if changed || this.shown_id.borrow().as_deref() != Some(id.as_str()) {
                        this.state.borrow_mut().note = Some(note);
                        this.show_note(true);
                    }
                }
                Err(e) if e.is_not_found() => {
                    {
                        let mut st = this.state.borrow_mut();
                        st.selected_id = None;
                        st.note = None;
                    }
                    this.show_note(false);
                    this.save_ui_state();
                    if !soft {
                        this.toast("That note no longer exists.");
                    }
                }
                Err(e) => {
                    let shown = this.shown_id.borrow().as_deref() == Some(id.as_str());
                    if !shown {
                        this.view.stack.set_visible_child_name(if e.is_offline() {
                            "missing"
                        } else {
                            "none"
                        });
                    }
                    this.handle_error(&e, soft || e.is_offline());
                }
            }
        });
    }

    // -----------------------------------------------------------------------------------------
    // Navigation

    pub(crate) fn select_folder(self: &Rc<Self>, id: String, user: bool) {
        let weak = self.weak();
        self.leave_edit(move || {
            let Some(this) = weak.upgrade() else { return };
            if user && this.outer.is_collapsed() {
                this.outer.set_show_content(true);
            }
            let same = this.state.borrow().folder_id.as_deref() == Some(id.as_str())
                && this.state.borrow().search.is_none();
            if same {
                return;
            }
            {
                let mut st = this.state.borrow_mut();
                st.folder_id = Some(id.clone());
                st.search = None;
                st.selected_id = None;
                st.note = None;
            }
            this.list.search_bar.set_search_mode(false);
            this.folders.select(Some(&id));
            this.show_note(false);
            this.save_ui_state();
            this.load_list(false);
        });
    }

    pub(crate) fn open_note(self: &Rc<Self>, id: String, user: bool) {
        let weak = self.weak();
        self.leave_edit(move || {
            let Some(this) = weak.upgrade() else { return };
            if user && this.inner.is_collapsed() {
                this.inner.set_show_content(true);
            }
            if this.state.borrow().selected_id.as_deref() == Some(id.as_str())
                && this.shown_id.borrow().as_deref() == Some(id.as_str())
            {
                return;
            }
            {
                let mut st = this.state.borrow_mut();
                st.selected_id = Some(id.clone());
                st.note = None;
            }
            this.list.select(Some(&id));
            this.save_ui_state();
            this.load_note(id, false);
        });
    }

    fn start_search(self: &Rc<Self>) {
        if self.inner.is_collapsed() {
            self.inner.set_show_content(false);
        }
        if self.outer.is_collapsed() {
            self.outer.set_show_content(true);
        }
        self.list.search_bar.set_search_mode(true);
        self.list.search_entry.grab_focus();
    }

    // -----------------------------------------------------------------------------------------
    // Reading view

    /// Draws `state.note` (or the empty state). `keep_scroll` keeps the position when the same
    /// note is redrawn after a live update.
    fn show_note(self: &Rc<Self>, keep_scroll: bool) {
        let note = {
            let st = self.state.borrow();
            st.note
                .clone()
                .filter(|n| st.selected_id.as_deref() == Some(n.summary.id.as_str()))
        };
        let Some(note) = note else {
            *self.shown_id.borrow_mut() = None;
            self.view.stack.set_visible_child_name("none");
            self.update_chrome();
            return;
        };
        let same = self.shown_id.borrow().as_deref() == Some(note.summary.id.as_str());
        *self.shown_id.borrow_mut() = Some(note.summary.id.clone());
        if note.summary.locked {
            self.view.stack.set_visible_child_name("locked");
            self.update_chrome();
            return;
        }
        let scroll = self.view.scroller.vadjustment().value();
        let st = self.state.borrow();
        let in_trash = note
            .summary
            .folder_id
            .as_deref()
            .and_then(|f| st.folder(f))
            .is_some_and(|f| f.is_trash);
        drop(st);
        self.view
            .date
            .set_label(&long_date(note.summary.modified_at.as_deref()));
        let info = if in_trash {
            Some("In Recently Deleted — recover it on your iPhone or Mac")
        } else if !note.summary.editable {
            Some(if note.summary.has_checklist || note.summary.has_attachments {
                "Has checklists or attachments — edit on your iPhone or Mac"
            } else {
                "Has tags, mentions or note links — edit on your iPhone or Mac"
            })
        } else {
            None
        };
        self.view.info.set_visible(info.is_some());
        self.view.info_label.set_label(info.unwrap_or(""));

        let rendered = render::build(&note);
        self.view.body.set_child(Some(&rendered.root));
        self.view.stack.set_visible_child_name("note");
        let adj = self.view.scroller.vadjustment();
        if same && keep_scroll {
            glib::idle_add_local_once(move || adj.set_value(scroll));
        } else {
            self.view.scroll_to_top();
        }
        for (att, slot) in rendered.images {
            let key = attachments::version_key(&att);
            if let Some(texture) = self.textures.borrow_mut().get(&key) {
                render::set_image(&slot, Some(&texture), &att);
                continue;
            }
            self.pending_images.set(self.pending_images.get() + 1);
            let client = self.client();
            let weak = self.weak();
            glib::spawn_future_local(async move {
                let a = att.clone();
                let texture = bg(move || {
                    let path = attachments::fetch(&client, &a).ok()?;
                    attachments::decode(&path, attachments::IMAGE_WIDTH)
                })
                .await;
                render::set_image(&slot, texture.as_ref(), &att);
                if let Some(this) = weak.upgrade() {
                    if let Some(t) = texture {
                        this.textures.borrow_mut().insert(key, t);
                    }
                    this.pending_images
                        .set(this.pending_images.get().saturating_sub(1));
                }
            });
        }
        for (att, button) in rendered.files {
            let weak = self.weak();
            button.connect_clicked(move |button| {
                let window = button.root().and_downcast::<gtk::Window>();
                if let Some(href) = render::link_href(&att) {
                    gtk::UriLauncher::new(href).launch(
                        window.as_ref(),
                        gio::Cancellable::NONE,
                        |_| {},
                    );
                    return;
                }
                let Some(this) = weak.upgrade() else { return };
                let client = this.client();
                let att = att.clone();
                let button = button.clone();
                let weak = this.weak();
                button.set_sensitive(false);
                glib::spawn_future_local(async move {
                    let res = bg(move || attachments::fetch(&client, &att)).await;
                    button.set_sensitive(true);
                    match res {
                        Ok(path) => {
                            let file = gio::File::for_path(path);
                            gtk::FileLauncher::new(Some(&file)).launch(
                                window.as_ref(),
                                gio::Cancellable::NONE,
                                |_| {},
                            );
                        }
                        Err(e) => {
                            if let Some(this) = weak.upgrade() {
                                this.handle_error(&e, false);
                            }
                        }
                    }
                });
            });
        }
        self.update_chrome();
    }

    // -----------------------------------------------------------------------------------------
    // Editing

    pub(crate) fn begin_edit(self: &Rc<Self>) {
        if !self.action("edit").is_some_and(|a| a.is_enabled()) {
            return;
        }
        let session = {
            let st = self.state.borrow();
            let Some(note) = st.note.as_ref() else { return };
            let Some(md) = note.markdown.clone() else {
                return;
            };
            EditSession {
                note_id: Some(note.summary.id.clone()),
                folder_id: note
                    .summary
                    .folder_id
                    .clone()
                    .or_else(|| st.folder_id.clone())
                    .unwrap_or_default(),
                base_modified: note.summary.modified_at.clone(),
                original: md,
                saving: false,
            }
        };
        self.view.editor.set_text(&session.original);
        self.state.borrow_mut().edit = Some(session);
        self.view.stack.set_visible_child_name("editor");
        self.view.editor.view.grab_focus();
        self.update_chrome();
    }

    fn new_note(self: &Rc<Self>) {
        if !self.action("new-note").is_some_and(|a| a.is_enabled()) {
            return;
        }
        let Some(folder) = self.state.borrow().target_folder().cloned() else {
            return;
        };
        let session = EditSession {
            note_id: None,
            folder_id: folder.id,
            base_modified: None,
            original: "# ".into(),
            saving: false,
        };
        self.view.editor.set_text(&session.original);
        {
            let mut st = self.state.borrow_mut();
            st.edit = Some(session);
            st.selected_id = None;
            st.note = None;
        }
        *self.shown_id.borrow_mut() = None;
        self.list.select(None);
        if self.outer.is_collapsed() {
            self.outer.set_show_content(true);
        }
        if self.inner.is_collapsed() {
            self.inner.set_show_content(true);
        }
        self.view.stack.set_visible_child_name("editor");
        self.view.editor.view.grab_focus();
        self.update_chrome();
    }

    fn end_edit(self: &Rc<Self>) {
        self.state.borrow_mut().edit = None;
        self.show_note(true);
        if self.state.borrow().note.is_none() && self.inner.is_collapsed() {
            self.inner.set_show_content(false);
        }
        self.focus_after_edit();
    }

    /// Moves focus off the hidden editor so it doesn't land in (and put a caret on) note text.
    fn focus_after_edit(&self) {
        if self.inner.is_collapsed() || !self.list.focus_selected() {
            self.view.edit_button.grab_focus();
        }
    }

    fn cancel_edit(self: &Rc<Self>) {
        let weak = self.weak();
        self.leave_edit(move || {
            if let Some(this) = weak.upgrade() {
                // Pick up anything that changed elsewhere while editing.
                let id = this.state.borrow().selected_id.clone();
                if let Some(id) = id {
                    this.load_note(id, true);
                }
            }
        });
    }

    /// Runs `then` once any open editor is closed, asking first if there are unsaved changes.
    fn leave_edit(self: &Rc<Self>, then: impl FnOnce() + 'static) {
        if !self.state.borrow().is_editing() {
            then();
            return;
        }
        if !self.is_dirty() {
            self.end_edit();
            then();
            return;
        }
        let weak = self.weak();
        // Put the sidebar and list highlights back while the user decides.
        let (note, folder) = {
            let st = self.state.borrow();
            (st.selected_id.clone(), st.folder_id.clone())
        };
        self.list.select(note.as_deref());
        self.folders.select(folder.as_deref());
        self.confirm_discard(move || {
            if let Some(this) = weak.upgrade() {
                this.end_edit();
                then();
            }
        });
    }

    fn confirm_discard(self: &Rc<Self>, discard: impl FnOnce() + 'static) {
        let dialog = adw::AlertDialog::new(
            Some("Discard Changes?"),
            Some("Your edits to this note haven't been saved."),
        );
        dialog.add_responses(&[("keep", "_Keep Editing"), ("discard", "_Discard")]);
        dialog.set_response_appearance("discard", adw::ResponseAppearance::Destructive);
        dialog.set_default_response(Some("keep"));
        dialog.set_close_response("keep");
        let discard = RefCell::new(Some(discard));
        dialog.connect_response(None, move |_, response| {
            if response == "discard"
                && let Some(f) = discard.take()
            {
                f();
            }
        });
        dialog.present(Some(&self.win));
    }

    pub(crate) fn save(self: &Rc<Self>) {
        if !self.action("save").is_some_and(|a| a.is_enabled()) {
            return;
        }
        let text = self.view.editor.text();
        let Some(session) = self.state.borrow().edit.clone() else {
            return;
        };
        let has_content = text
            .lines()
            .any(|l| !l.trim().is_empty() && l.trim() != "#");
        if !has_content {
            self.toast("Write something first.");
            return;
        }
        if let Some(e) = self.state.borrow_mut().edit.as_mut() {
            e.saving = true;
        }
        self.update_chrome();
        let client = self.client();
        let weak = self.weak();
        let body = text.clone();
        let s = session.clone();
        glib::spawn_future_local(async move {
            let res = bg(move || {
                let r = match &s.note_id {
                    Some(id) => {
                        let expected = s.base_modified.as_deref().unwrap_or_default();
                        client.update_note(id, &body, expected)
                    }
                    None => client.create_note(&s.folder_id, &body),
                };
                if let Ok(n) = &r
                    && n.summary.modified_at.is_some()
                {
                    cache::store(&cache_note_key(&n.summary.id), n);
                }
                r
            })
            .await;
            let Some(this) = weak.upgrade() else { return };
            if let Some(e) = this.state.borrow_mut().edit.as_mut() {
                e.saving = false;
            }
            match res {
                Ok(note) => {
                    let id = note.summary.id.clone();
                    // Saved, but the bridge couldn't read it back yet: fetch it again.
                    let partial = note.summary.modified_at.is_none();
                    {
                        let mut st = this.state.borrow_mut();
                        st.edit = None;
                        st.selected_id = Some(id.clone());
                        st.note = (!partial).then_some(note);
                    }
                    *this.shown_id.borrow_mut() = None;
                    if partial {
                        this.load_note(id, false);
                    } else {
                        this.show_note(false);
                    }
                    this.focus_after_edit();
                    this.save_ui_state();
                    this.load_folders();
                    this.load_list(true);
                    if session.note_id.is_none() {
                        this.toast("Note added");
                    }
                }
                Err(e) if e.is_conflict() => this.on_conflict(&e, &text),
                Err(e) if e.is_not_editable() => {
                    this.toast(&describe(&e));
                    this.state.borrow_mut().edit = None;
                    this.refresh_all(true);
                }
                Err(e) if e.is_offline() => {
                    this.set_offline(true);
                    this.toast("Couldn't save — can't reach your Mac. Your text is still here.");
                }
                Err(e) => {
                    this.toast(&describe(&e));
                    this.update_chrome();
                }
            }
        });
    }

    fn on_conflict(self: &Rc<Self>, e: &Error, text: &str) {
        self.win.clipboard().set_text(text);
        let theirs = match e {
            Error::Api {
                current: Some(c), ..
            } => c
                .get("modifiedAt")
                .and_then(|v| v.as_str())
                .map(str::to_string),
            _ => None,
        };
        self.update_chrome();
        let dialog = adw::AlertDialog::new(
            Some("Changed on Another Device"),
            Some(
                "Someone changed this note while you were editing. Your text is still here, and a copy is on the clipboard.\n\nReload to see their version, or keep editing — saving again replaces their changes.",
            ),
        );
        dialog.add_responses(&[("reload", "_Reload Theirs"), ("keep", "_Keep Editing")]);
        dialog.set_response_appearance("reload", adw::ResponseAppearance::Destructive);
        dialog.set_default_response(Some("keep"));
        dialog.set_close_response("keep");
        let weak = self.weak();
        dialog.connect_response(None, move |_, response| {
            let Some(this) = weak.upgrade() else { return };
            if response == "reload" {
                this.state.borrow_mut().edit = None;
                let id = this.state.borrow().selected_id.clone();
                *this.shown_id.borrow_mut() = None;
                this.update_chrome();
                this.focus_after_edit();
                if let Some(id) = id {
                    this.load_note(id, false);
                }
            } else {
                if let Some(e) = this.state.borrow_mut().edit.as_mut()
                    && theirs.is_some()
                {
                    e.base_modified = theirs.clone();
                }
                this.view.editor.view.grab_focus();
            }
        });
        dialog.present(Some(&self.win));
    }

    // -----------------------------------------------------------------------------------------
    // Delete

    fn confirm_delete(self: &Rc<Self>) {
        if !self.action("delete").is_some_and(|a| a.is_enabled()) {
            return;
        }
        let Some(note) = self.state.borrow().note.clone() else {
            return;
        };
        let title = if note.summary.title.trim().is_empty() {
            "this note".to_string()
        } else {
            format!("“{}”", note.summary.title.trim())
        };
        let dialog = adw::AlertDialog::new(
            Some(&format!("Delete {title}?")),
            Some(
                "It moves to Recently Deleted, where you can recover it on your iPhone or Mac for 30 days.",
            ),
        );
        dialog.add_responses(&[("cancel", "_Cancel"), ("delete", "_Delete")]);
        dialog.set_response_appearance("delete", adw::ResponseAppearance::Destructive);
        dialog.set_default_response(Some("cancel"));
        dialog.set_close_response("cancel");
        let weak = self.weak();
        dialog.connect_response(None, move |_, response| {
            if response != "delete" {
                return;
            }
            let Some(this) = weak.upgrade() else { return };
            let client = this.client();
            let id = note.summary.id.clone();
            let weak = this.weak();
            glib::spawn_future_local(async move {
                let del = id.clone();
                let res = bg(move || client.delete_note(&del)).await;
                let Some(this) = weak.upgrade() else { return };
                match res {
                    Ok(()) => {
                        let was_open =
                            this.state.borrow().selected_id.as_deref() == Some(id.as_str());
                        if was_open {
                            let mut st = this.state.borrow_mut();
                            st.selected_id = None;
                            st.note = None;
                        }
                        this.show_note(false);
                        if this.inner.is_collapsed() {
                            this.inner.set_show_content(false);
                        }
                        this.save_ui_state();
                        this.toast("Moved to Recently Deleted");
                        this.load_folders();
                        this.load_list(true);
                    }
                    Err(e) => this.handle_error(&e, false),
                }
            });
        });
        dialog.present(Some(&self.win));
    }

    // -----------------------------------------------------------------------------------------
    // Screenshot driver hooks

    pub(crate) fn state(&self) -> std::cell::Ref<'_, State> {
        self.state.borrow()
    }

    pub(crate) fn set_search_text(&self, q: &str) {
        self.list.search_bar.set_search_mode(true);
        self.list.search_entry.set_text(q);
    }

    pub(crate) fn editor_append(&self, text: &str) {
        let buffer = self.view.editor.view.buffer();
        buffer.insert(&mut buffer.end_iter(), text);
    }
}

/// A short, plain sentence for a toast.
fn describe(e: &Error) -> String {
    if e.is_not_found() {
        "That note no longer exists.".into()
    } else if e.is_not_editable() {
        "This note has checklists, attachments or tags, so edit it on your iPhone or Mac.".into()
    } else {
        e.user_message()
    }
}
