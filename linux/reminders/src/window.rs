//! The main window: split view, data flow (cache → bridge → UI), live updates and edits.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;
use std::time::Duration;

use adw::prelude::*;
use fruity_bridge_client::reminders::{
    Completed, List, NewReminder, Query, Reminder, ReminderPatch,
};
use fruity_bridge_client::{Config, Error, Event, Subscription, cache};
use fruity_bridge_gtk::{bg, prefs};
use gtk::{gio, glib};

use crate::row::{self, RowContext, RowHandle};
use crate::sidebar::{OnSelect, Sidebar};
use crate::state::{
    CompletedScope, Data, LISTS_CACHE, OPEN_CACHE, Section, State, UI_CACHE, UiState, View,
};
use crate::{dates, editor, style};

/// How long a just-completed reminder stays on screen before it slides away.
const LINGER: Duration = Duration::from_millis(1400);
/// While offline, how often to try the bridge again (the event stream also retries).
const RETRY: Duration = Duration::from_secs(10);
/// How long sending pending deletes may hold up closing the window.
const FLUSH_TIMEOUT: Duration = Duration::from_secs(10);

pub struct Window {
    pub window: adw::ApplicationWindow,
    state: RefCell<State>,
    toasts: adw::ToastOverlay,
    split: adw::NavigationSplitView,
    sidebar: Rc<Sidebar>,
    content_page: adw::NavigationPage,
    banners: [adw::Banner; 2],
    stack: gtk::Stack,
    view_header: gtk::Box,
    title_label: gtk::Label,
    count_label: gtk::Label,
    sections_box: gtk::Box,
    empty: adw::StatusPage,
    /// Notes that completed reminders only go back 30 days.
    completed_note: gtk::Label,
    new_row: gtk::Box,
    new_entry: gtk::Entry,
    search_bar: gtk::SearchBar,
    search_entry: gtk::SearchEntry,
    rows: RefCell<HashMap<String, RowHandle>>,
    /// "Deleted · Undo" toasts still showing, by reminder id.
    undo_toasts: RefCell<HashMap<String, adw::Toast>>,
    /// What the rows currently show, so unchanged refreshes don't rebuild them.
    rendered: RefCell<Option<RenderKey>>,
    refreshing: Cell<bool>,
    refresh_again: Cell<bool>,
    events: RefCell<Option<Subscription>>,
    retry: RefCell<Option<glib::SourceId>>,
    /// Hidden while pending deletes are sent before closing.
    closing: Cell<bool>,
    after_load: RefCell<Vec<Box<dyn FnOnce()>>>,
}

/// Everything that changes how the rows look.
#[derive(PartialEq)]
struct RenderKey {
    view: View,
    search: String,
    editable: bool,
    day: chrono::NaiveDate,
    lists: Vec<(String, String, Option<String>, bool)>,
    rows: Vec<(Option<String>, Vec<RowKey>)>,
}

#[derive(PartialEq)]
struct RowKey {
    id: String,
    list_id: String,
    title: String,
    notes: Option<String>,
    url: Option<String>,
    priority: u8,
    completed: bool,
    due: Option<(String, Option<String>)>,
    recurring: bool,
}

impl RowKey {
    fn of(r: &Reminder) -> RowKey {
        RowKey {
            id: r.id.clone(),
            list_id: r.list_id.clone(),
            title: r.title.clone(),
            notes: r.notes.clone(),
            url: r.url.clone(),
            priority: r.priority,
            completed: r.completed,
            due: r.due.as_ref().map(|d| (d.date.clone(), d.time.clone())),
            recurring: r.recurring,
        }
    }
}

type WindowAction = dyn Fn(&Rc<Window>);
type Fetched = (
    Vec<List>,
    Vec<Reminder>,
    Option<(CompletedScope, Vec<Reminder>)>,
);

const OFFLINE_EDIT: &str = "Can't reach your Mac, so changes are off for now.";

impl Window {
    pub fn new(app: &adw::Application) -> Rc<Self> {
        let config = Config::load();

        let window = adw::ApplicationWindow::builder()
            .application(app)
            .title("Reminders")
            .icon_name(crate::APP_ID)
            .default_width(1040)
            .default_height(720)
            .width_request(360)
            .height_request(294)
            .build();

        // ----- Sidebar page
        // The sidebar is built before the window it reports to, so selection goes through a slot.
        let on_select: Rc<RefCell<Option<OnSelect>>> = Rc::new(RefCell::new(None));
        let on_select_proxy: OnSelect = {
            let on_select = on_select.clone();
            Rc::new(move |v| {
                let cb = on_select.borrow().clone();
                if let Some(cb) = cb {
                    cb(v)
                }
            })
        };
        let sidebar = Sidebar::new(on_select_proxy);
        let sidebar_header = adw::HeaderBar::new();
        sidebar_header.set_title_widget(Some(&adw::WindowTitle::new("Reminders", "")));
        let menu = gio::Menu::new();
        let section = gio::Menu::new();
        section.append(Some("Show Completed"), Some("win.show-completed"));
        section.append(Some("Refresh"), Some("win.refresh"));
        menu.append_section(None, &section);
        let section = gio::Menu::new();
        section.append(Some("Connection Settings…"), Some("win.preferences"));
        section.append(Some("Keyboard Shortcuts"), Some("win.shortcuts"));
        section.append(Some("About Reminders"), Some("win.about"));
        menu.append_section(None, &section);
        let menu_button = gtk::MenuButton::builder()
            .icon_name("open-menu-symbolic")
            .menu_model(&menu)
            .primary(true)
            .tooltip_text("Main Menu")
            .build();
        sidebar_header.pack_end(&menu_button);

        let offline_text = "Can't reach your Mac — showing saved reminders";
        let banners = [
            adw::Banner::new(offline_text),
            adw::Banner::new(offline_text),
        ];
        for b in &banners {
            b.set_button_label(Some("Retry"));
            b.set_action_name(Some("win.refresh"));
        }
        let sidebar_view = adw::ToolbarView::new();
        sidebar_view.add_top_bar(&sidebar_header);
        sidebar_view.add_top_bar(&banners[0]);
        sidebar_view.set_content(Some(&sidebar.widget));
        let sidebar_page = adw::NavigationPage::builder()
            .title("Reminders")
            .tag("sidebar")
            .child(&sidebar_view)
            .build();

        // ----- Content page
        let content_header = adw::HeaderBar::new();
        content_header.set_show_title(false);
        let add_button = gtk::Button::builder()
            .icon_name("list-add-symbolic")
            .tooltip_text("New Reminder")
            .action_name("win.new-reminder")
            .build();
        let search_button = gtk::ToggleButton::builder()
            .icon_name("edit-find-symbolic")
            .tooltip_text("Search")
            .build();
        let completed_button = gtk::ToggleButton::builder()
            .icon_name("checkbox-checked-symbolic")
            .tooltip_text("Show Completed")
            .action_name("win.show-completed")
            .build();
        content_header.pack_end(&add_button);
        content_header.pack_end(&search_button);
        content_header.pack_end(&completed_button);

        let search_entry = gtk::SearchEntry::builder()
            .placeholder_text("Search all lists")
            .hexpand(true)
            .build();
        let search_clamp = adw::Clamp::builder()
            .maximum_size(560)
            .child(&search_entry)
            .build();
        let search_bar = gtk::SearchBar::builder().child(&search_clamp).build();
        search_bar.connect_entry(&search_entry);
        search_button
            .bind_property("active", &search_bar, "search-mode-enabled")
            .bidirectional()
            .build();

        let title_label = gtk::Label::builder()
            .xalign(0.0)
            .hexpand(true)
            .wrap(true)
            .wrap_mode(gtk::pango::WrapMode::WordChar)
            .css_classes(["view-title"])
            .build();
        title_label.set_accessible_role(gtk::AccessibleRole::Heading);
        let count_label = gtk::Label::builder()
            .css_classes(["view-count"])
            .valign(gtk::Align::Start)
            .build();
        let view_header = gtk::Box::builder()
            .spacing(12)
            .margin_start(6)
            .margin_end(6)
            .css_classes(["view-header"])
            .build();
        view_header.append(&title_label);
        view_header.append(&count_label);

        let sections_box = gtk::Box::new(gtk::Orientation::Vertical, 0);
        let empty = adw::StatusPage::builder()
            .css_classes(["compact"])
            .vexpand(true)
            .build();
        let completed_note = gtk::Label::builder()
            .label("Completed reminders from the last 30 days")
            .xalign(0.0)
            .margin_start(6)
            .margin_top(10)
            .css_classes(["caption", "dim-label"])
            .visible(false)
            .build();

        let add_icon = gtk::Image::builder()
            .icon_name("list-add-symbolic")
            .css_classes(["add-icon"])
            .valign(gtk::Align::Center)
            .build();
        let new_entry = gtk::Entry::builder()
            .placeholder_text("New Reminder")
            .hexpand(true)
            .build();
        new_entry.update_property(&[gtk::accessible::Property::Label("New Reminder")]);
        let new_row = gtk::Box::builder()
            .spacing(12)
            .css_classes(["new-reminder"])
            .margin_top(2)
            .build();
        new_row.append(&add_icon);
        new_row.append(&new_entry);

        let column = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .margin_top(12)
            .margin_bottom(32)
            .margin_start(12)
            .margin_end(12)
            .build();
        column.append(&view_header);
        column.append(&sections_box);
        column.append(&empty);
        column.append(&new_row);
        column.append(&completed_note);
        let clamp = adw::Clamp::builder()
            .maximum_size(780)
            .tightening_threshold(560)
            .child(&column)
            .build();
        let scroller = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .child(&clamp)
            .vexpand(true)
            .build();

        let loading = adw::Spinner::builder()
            .width_request(32)
            .height_request(32)
            .halign(gtk::Align::Center)
            .valign(gtk::Align::Center)
            .build();
        let unreachable = adw::StatusPage::builder()
            .icon_name("network-offline-symbolic")
            .title("Can't Reach Your Mac")
            .description("Your reminders come from the Fruity Bridge app on your Mac. Check that the Mac is awake and Tailscale is connected on both machines.")
            .build();
        let buttons = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(12)
            .halign(gtk::Align::Center)
            .build();
        buttons.append(
            &gtk::Button::builder()
                .label("Try Again")
                .css_classes(["pill", "suggested-action"])
                .action_name("win.refresh")
                .build(),
        );
        buttons.append(
            &gtk::Button::builder()
                .label("Connection Settings…")
                .css_classes(["pill"])
                .action_name("win.preferences")
                .build(),
        );
        unreachable.set_child(Some(&buttons));

        let stack = gtk::Stack::builder()
            .transition_type(gtk::StackTransitionType::Crossfade)
            .hhomogeneous(false)
            .build();
        stack.add_named(&loading, Some("loading"));
        stack.add_named(&scroller, Some("content"));
        stack.add_named(&unreachable, Some("unreachable"));

        let content_view = adw::ToolbarView::builder()
            .css_classes(["content-view"])
            .build();
        content_view.add_top_bar(&content_header);
        content_view.add_top_bar(&banners[1]);
        content_view.add_top_bar(&search_bar);
        content_view.set_content(Some(&stack));
        let content_page = adw::NavigationPage::builder()
            .title("Today")
            .tag("content")
            .child(&content_view)
            .build();

        let split = adw::NavigationSplitView::builder()
            .sidebar(&sidebar_page)
            .content(&content_page)
            .min_sidebar_width(270.0)
            .max_sidebar_width(330.0)
            .sidebar_width_fraction(0.3)
            .build();
        let toasts = adw::ToastOverlay::new();
        toasts.set_child(Some(&split));
        window.set_content(Some(&toasts));

        let breakpoint =
            adw::Breakpoint::new(adw::BreakpointCondition::parse("max-width: 600sp").unwrap());
        breakpoint.add_setter(&split, "collapsed", Some(&true.to_value()));
        window.add_breakpoint(breakpoint);

        let mut state = State::new(config);
        let ui: UiState = cache::load(UI_CACHE).unwrap_or_default();
        state.view = ui.view;
        state.show_completed = ui.show_completed;
        if let (Some(lists), Some(open)) = (
            cache::load::<Vec<List>>(LISTS_CACHE),
            cache::load::<Vec<Reminder>>(OPEN_CACHE),
        ) {
            let scope = state.view.completed_scope();
            let completed: Option<Vec<Reminder>> = cache::load(&scope.cache_key());
            state.data = Data {
                lists,
                open,
                completed_scope: completed.as_ref().map(|_| scope),
                completed: completed.unwrap_or_default(),
            };
            state.loaded = true;
        }

        let win = Rc::new(Window {
            window,
            state: RefCell::new(state),
            toasts,
            split,
            sidebar,
            content_page,
            banners,
            stack,
            view_header,
            title_label,
            count_label,
            sections_box,
            empty,
            completed_note,
            new_row,
            new_entry,
            search_bar,
            search_entry,
            rows: RefCell::new(HashMap::new()),
            undo_toasts: RefCell::new(HashMap::new()),
            rendered: RefCell::new(None),
            refreshing: Cell::new(false),
            refresh_again: Cell::new(false),
            events: RefCell::new(None),
            retry: RefCell::new(None),
            closing: Cell::new(false),
            after_load: RefCell::new(Vec::new()),
        });

        let weak = Rc::downgrade(&win);
        *on_select.borrow_mut() = Some(Rc::new(move |view| {
            if let Some(win) = weak.upgrade() {
                win.select_view(view);
            }
        }));
        win.connect_signals();
        win.add_actions();
        win.render();
        if win.state.borrow().config.is_set() {
            win.refresh();
            win.start_events();
        } else {
            // First run: nothing to load until the user gives the bridge address.
            let weak = Rc::downgrade(&win);
            glib::idle_add_local_once(move || {
                if let Some(win) = weak.upgrade() {
                    win.show_preferences();
                }
            });
        }
        win
    }

    fn connect_signals(self: &Rc<Self>) {
        self.new_entry.connect_activate(glib::clone!(
            #[weak(rename_to = win)]
            self,
            move |entry| {
                let title = entry.text().trim().to_string();
                if !title.is_empty() {
                    entry.set_text("");
                    win.create(title);
                }
            }
        ));
        self.search_entry.connect_search_changed(glib::clone!(
            #[weak(rename_to = win)]
            self,
            move |entry| {
                win.state.borrow_mut().search = entry.text().to_string();
                win.render();
            }
        ));
        self.search_bar
            .connect_search_mode_enabled_notify(glib::clone!(
                #[weak(rename_to = win)]
                self,
                move |bar| {
                    if !bar.is_search_mode() {
                        win.search_entry.set_text("");
                    }
                }
            ));
        self.split.connect_collapsed_notify(glib::clone!(
            #[weak(rename_to = win)]
            self,
            move |_| win.update_banners()
        ));
        self.window.connect_close_request(glib::clone!(
            #[weak(rename_to = win)]
            self,
            #[upgrade_or]
            glib::Propagation::Proceed,
            move |_| {
                if win.flush_deletes_then_close() {
                    glib::Propagation::Stop
                } else {
                    glib::Propagation::Proceed
                }
            }
        ));
    }

    fn add_actions(self: &Rc<Self>) {
        let action = |name: &str, f: Box<WindowAction>| {
            let a = gio::SimpleAction::new(name, None);
            let weak = Rc::downgrade(self);
            a.connect_activate(move |_, _| {
                if let Some(win) = weak.upgrade() {
                    f(&win)
                }
            });
            self.window.add_action(&a);
        };
        action("new-reminder", Box::new(|w| w.focus_new()));
        action("search", Box::new(|w| w.start_search()));
        action("refresh", Box::new(|w| w.refresh()));
        action("preferences", Box::new(|w| w.show_preferences()));
        action("shortcuts", Box::new(|w| w.show_shortcuts()));
        action("about", Box::new(|w| w.show_about()));

        let show_completed = self.state.borrow().show_completed;
        let toggle =
            gio::SimpleAction::new_stateful("show-completed", None, &show_completed.to_variant());
        let weak = Rc::downgrade(self);
        toggle.connect_activate(move |a, _| {
            let on = !a.state().and_then(|s| s.get::<bool>()).unwrap_or(false);
            a.set_state(&on.to_variant());
            if let Some(win) = weak.upgrade() {
                win.state.borrow_mut().show_completed = on;
                win.save_ui();
                win.ensure_completed_loaded();
                win.render();
            }
        });
        self.window.add_action(&toggle);
    }

    // ----- Public entry points (also used by the screenshot mode)

    pub fn present(&self) {
        self.window.present();
    }

    /// Runs `f` once the first refresh has finished, whether or not it reached the bridge.
    pub fn after_first_load(&self, f: impl FnOnce() + 'static) {
        self.after_load.borrow_mut().push(Box::new(f));
    }

    pub fn select_view(self: &Rc<Self>, view: View) {
        {
            let mut st = self.state.borrow_mut();
            st.view = view;
        }
        if self.search_bar.is_search_mode() {
            self.search_bar.set_search_mode(false);
        }
        self.save_ui();
        self.ensure_completed_loaded();
        self.render();
        self.split.set_show_content(true);
    }

    /// Selects a list by title or a smart list by name (for screenshots).
    pub fn select_named(self: &Rc<Self>, name: &str) {
        let view = match name.to_lowercase().as_str() {
            "today" => View::Today,
            "scheduled" => View::Scheduled,
            "all" => View::All,
            "completed" => View::Completed,
            _ => {
                let st = self.state.borrow();
                match st
                    .data
                    .lists
                    .iter()
                    .find(|l| l.title.eq_ignore_ascii_case(name))
                {
                    Some(l) => View::List(l.id.clone()),
                    None => return,
                }
            }
        };
        self.select_view(view);
    }

    pub fn show_content(&self, show: bool) {
        self.split.set_show_content(show);
    }

    pub fn search_for(self: &Rc<Self>, text: &str) {
        self.start_search();
        self.search_entry.set_text(text);
    }

    pub fn edit_titled(self: &Rc<Self>, title: &str) {
        let id = self
            .state
            .borrow()
            .data
            .open
            .iter()
            .chain(self.state.borrow().data.completed.iter())
            .find(|r| r.title.eq_ignore_ascii_case(title))
            .map(|r| r.id.clone());
        if let Some(id) = id {
            self.open_editor(&id);
        }
    }

    // ----- Data loading

    pub fn refresh(self: &Rc<Self>) {
        if self.refreshing.replace(true) {
            self.refresh_again.set(true);
            return;
        }
        let (client, scope) = {
            let st = self.state.borrow();
            let wants = st.show_completed || st.view == View::Completed;
            (st.client.clone(), wants.then(|| st.view.completed_scope()))
        };
        let this = self.clone();
        glib::spawn_future_local(async move {
            let result = bg(move || {
                let c = client;
                // Counts come from the open reminders fetched here, so skip the bridge's.
                let lists = c.reminder_lists(false)?;
                let open = c.reminders(Query::All(Completed::Incomplete))?;
                let completed = match scope {
                    Some(scope) => {
                        // The bridge's default: completed in the last 30 days.
                        let query = match &scope {
                            CompletedScope::All => Query::All(Completed::Complete),
                            CompletedScope::List(id) => Query::List(id, Completed::Complete),
                        };
                        let done = c.reminders(query)?;
                        Some((scope, done))
                    }
                    None => None,
                };
                cache::store(LISTS_CACHE, &lists);
                cache::store(OPEN_CACHE, &open);
                if let Some((scope, done)) = &completed {
                    cache::store(&scope.cache_key(), done);
                }
                Ok::<_, Error>((lists, open, completed))
            })
            .await;
            this.refreshing.set(false);
            this.apply_refresh(result);
            if this.refresh_again.replace(false) {
                this.refresh();
            }
        });
    }

    fn apply_refresh(self: &Rc<Self>, result: fruity_bridge_client::Result<Fetched>) {
        match result {
            Ok((lists, open, completed)) => {
                {
                    let mut st = self.state.borrow_mut();
                    st.data.lists = lists;
                    st.data.open = open;
                    if let Some((scope, done)) = completed {
                        st.data.completed = done;
                        st.data.completed_scope = Some(scope);
                    }
                    st.loaded = true;
                    st.recount();
                    if let View::List(id) = &st.view
                        && st.list(id).is_none()
                    {
                        st.view = View::Today;
                    }
                }
                self.set_online(true);
            }
            Err(e) if e.is_offline() => {
                self.set_online(false);
            }
            Err(e) => {
                self.set_online(true);
                self.toast(&e.user_message());
            }
        }
        self.render();
        let callbacks: Vec<_> = self.after_load.borrow_mut().drain(..).collect();
        for f in callbacks {
            f();
        }
    }

    /// When the view shows completed reminders: shows the saved copy at once if the loaded ones
    /// don't cover it, then fetches fresh ones.
    fn ensure_completed_loaded(self: &Rc<Self>) {
        let (wants, have, scope) = {
            let st = self.state.borrow();
            let scope = st.view.completed_scope();
            let have = st
                .data
                .completed_scope
                .as_ref()
                .is_some_and(|s| *s == scope || *s == CompletedScope::All);
            (st.show_completed || st.view == View::Completed, have, scope)
        };
        if !wants {
            return;
        }
        if !have && let Some(done) = cache::load::<Vec<Reminder>>(&scope.cache_key()) {
            let mut st = self.state.borrow_mut();
            st.data.completed = done;
            st.data.completed_scope = Some(scope);
        }
        self.refresh();
    }

    fn set_online(self: &Rc<Self>, online: bool) {
        self.state.borrow_mut().online = online;
        self.update_banners();
        let mut retry = self.retry.borrow_mut();
        if online {
            if let Some(id) = retry.take() {
                id.remove();
            }
        } else if retry.is_none() {
            let weak = Rc::downgrade(self);
            *retry = Some(glib::timeout_add_local(RETRY, move || {
                match weak.upgrade() {
                    Some(win) => {
                        win.refresh();
                        glib::ControlFlow::Continue
                    }
                    None => glib::ControlFlow::Break,
                }
            }));
        }
    }

    /// The offline banner sits on the content page; the sidebar repeats it only when it's shown
    /// on its own (narrow window).
    fn update_banners(&self) {
        let st = self.state.borrow();
        let offline = !st.online && st.loaded;
        self.banners[1].set_revealed(offline);
        self.banners[0].set_revealed(offline && self.split.is_collapsed());
    }

    /// Follows the bridge's event stream; a new call replaces (and stops) the previous one.
    fn start_events(self: &Rc<Self>) {
        let config = self.state.borrow().config.clone();
        // Stop the old stream first, so its last events can't land after the new one starts.
        self.events.replace(None);
        let subscription =
            fruity_bridge_gtk::follow_events(&config, Rc::downgrade(self), |win, ev| match ev {
                Event::Connected | Event::Reminders | Event::Disconnected => win.refresh(),
                Event::Notes => {}
            });
        self.events.replace(Some(subscription));
    }

    fn save_ui(&self) {
        let st = self.state.borrow();
        let ui = UiState {
            view: st.view.clone(),
            show_completed: st.show_completed,
        };
        gio::spawn_blocking(move || cache::store(UI_CACHE, &ui));
    }

    // ----- Rendering

    fn render(self: &Rc<Self>) {
        let st = self.state.borrow();
        let now = dates::now();
        self.sidebar.update(
            &st.data.lists,
            st.loaded.then(|| st.counts(now.date())),
            &st.view,
        );

        let searching = !st.search.trim().is_empty();
        let (title, color_class) = if searching {
            ("Results".to_string(), "c-neutral".to_string())
        } else {
            match &st.view {
                View::Today => ("Today".into(), "c-accent".into()),
                View::Scheduled => ("Scheduled".into(), style::color_class(Some("#e5484d"))),
                View::All => ("All".into(), "c-neutral".into()),
                View::Completed => ("Completed".into(), "c-muted".into()),
                View::List(id) => match st.list(id) {
                    Some(l) => (l.title.clone(), style::color_class(l.color.as_deref())),
                    None => ("Reminders".into(), "c-accent".into()),
                },
            }
        };
        self.content_page.set_title(&title);
        self.title_label.set_label(&title);
        style::apply_class(&self.view_header, &color_class);
        style::apply_class(&self.new_row, &color_class);

        let sections = st.sections(now);
        let shown = sections.iter().flat_map(|s| &s.reminders);
        let count = if st.view == View::Completed && !searching {
            shown.count()
        } else {
            shown
                .filter(|r| !r.completed || st.lingering.contains_key(&r.id))
                .count()
        };
        self.count_label.set_label(&if count > 0 {
            count.to_string()
        } else {
            String::new()
        });

        self.completed_note.set_visible(
            st.loaded
                && !searching
                && (st.view == View::Completed || st.show_completed)
                && !sections.is_empty(),
        );

        let target = st.target_list().cloned();
        let can_add = !searching && st.view != View::Completed && target.is_some();
        self.new_row.set_visible(can_add);
        self.new_row.set_sensitive(st.online);
        if let (Some(list), View::List(_)) = (&target, &st.view) {
            style::apply_class(&self.new_row, &style::color_class(list.color.as_deref()));
        }

        self.stack.set_visible_child_name(if st.loaded {
            "content"
        } else if st.online {
            "loading"
        } else {
            "unreachable"
        });

        let key = RenderKey {
            view: st.view.clone(),
            search: st.search.clone(),
            editable: st.online,
            day: now.date(),
            lists: st
                .data
                .lists
                .iter()
                .map(|l| (l.id.clone(), l.title.clone(), l.color.clone(), l.editable))
                .collect(),
            rows: sections
                .iter()
                .map(|s| {
                    (
                        s.title.clone(),
                        s.reminders.iter().map(RowKey::of).collect(),
                    )
                })
                .collect(),
        };
        if self.rendered.borrow().as_ref() == Some(&key) {
            return;
        }
        *self.rendered.borrow_mut() = Some(key);
        drop(st);
        self.build_rows(&sections, &title, searching);
    }

    fn build_rows(self: &Rc<Self>, sections: &[Section], title: &str, searching: bool) {
        while let Some(child) = self.sections_box.first_child() {
            self.sections_box.remove(&child);
        }
        let mut handles = HashMap::new();
        let st = self.state.borrow();
        let now = dates::now();
        let show_list = searching || st.view.is_smart();

        let weak = Rc::downgrade(self);
        let on_toggle: row::OnToggle = Rc::new(move |id, on| {
            if let Some(win) = weak.upgrade() {
                win.toggle(id, on);
            }
        });

        for section in sections {
            if let Some(t) = &section.title {
                let label = gtk::Label::builder()
                    .label(t)
                    .xalign(0.0)
                    .css_classes(["section-title"])
                    .margin_start(6)
                    .build();
                label.set_accessible_role(gtk::AccessibleRole::Heading);
                if t == "Overdue" {
                    label.add_css_class("overdue");
                }
                if section.color.is_some()
                    || st.view == View::All
                    || searching
                    || st.view == View::Completed
                {
                    style::apply_class(&label, &style::color_class(section.color.as_deref()));
                }
                self.sections_box.append(&label);
            }
            let listbox = gtk::ListBox::builder()
                .css_classes(["reminders"])
                .selection_mode(gtk::SelectionMode::None)
                .build();
            listbox.update_property(&[gtk::accessible::Property::Label(
                section.title.as_deref().unwrap_or(title),
            )]);
            for rem in &section.reminders {
                let list = st.list(&rem.list_id);
                let ctx = RowContext {
                    list,
                    show_list: show_list && section.color.is_none(),
                    editable: st.online && list.is_none_or(|l| l.editable),
                    now,
                };
                let handle = row::build(rem, &ctx, on_toggle.clone());
                listbox.append(&handle.row);
                handles.insert(rem.id.clone(), handle);
            }
            let weak = Rc::downgrade(self);
            listbox.connect_row_activated(move |_, row| {
                if let Some(win) = weak.upgrade() {
                    win.open_editor(row.widget_name().as_str());
                }
            });
            self.sections_box.append(&listbox);
        }

        let empty = sections.is_empty();
        self.sections_box.set_visible(!empty);
        self.empty.set_visible(empty);
        if empty {
            let (icon, heading, body) = if searching {
                (
                    "edit-find-symbolic",
                    "No Results".to_string(),
                    format!("Nothing matches “{}”.", st.search.trim()),
                )
            } else {
                match &st.view {
                    View::Today => (
                        "object-select-symbolic",
                        "Nothing Due Today".into(),
                        "Reminders due today or overdue show up here.".into(),
                    ),
                    View::Scheduled => (
                        "x-office-calendar-symbolic",
                        "Nothing Scheduled".into(),
                        "Reminders with a due date show up here.".into(),
                    ),
                    View::All => (
                        "object-select-symbolic",
                        "No Reminders".into(),
                        "Add one below to get started.".into(),
                    ),
                    View::Completed => (
                        "object-select-symbolic",
                        "No Completed Reminders".into(),
                        "Reminders you tick off in the last 30 days show up here.".into(),
                    ),
                    View::List(_) => (
                        "object-select-symbolic",
                        "All Done".into(),
                        format!("Nothing left in {title}."),
                    ),
                }
            };
            self.empty.set_icon_name(Some(icon));
            self.empty.set_title(&heading);
            self.empty
                .set_description(Some(&glib::markup_escape_text(&body)));
        }
        drop(st);
        *self.rows.borrow_mut() = handles;
    }

    /// After a check is clicked the row already shows its new state, so keep the rows (and their
    /// animation) unless the reminder moved.
    fn render_in_place(self: &Rc<Self>) {
        {
            let mut rendered = self.rendered.borrow_mut();
            if let Some(key) = rendered.as_mut() {
                let st = self.state.borrow();
                let rows: Vec<(Option<String>, Vec<RowKey>)> = st
                    .sections(dates::now())
                    .iter()
                    .map(|s| {
                        (
                            s.title.clone(),
                            s.reminders.iter().map(RowKey::of).collect(),
                        )
                    })
                    .collect();
                let order = |rows: &[(Option<String>, Vec<RowKey>)]| -> Vec<String> {
                    rows.iter()
                        .flat_map(|(_, r)| r.iter().map(|k| k.id.clone()))
                        .collect()
                };
                if order(&rows) == order(&key.rows) {
                    key.rows = rows;
                }
            }
        }
        self.render();
    }

    // ----- Editing

    fn can_edit(&self) -> bool {
        if self.state.borrow().online {
            true
        } else {
            self.toast(OFFLINE_EDIT);
            false
        }
    }

    fn toggle(self: &Rc<Self>, id: &str, completed: bool) {
        if !self.can_edit() {
            *self.rendered.borrow_mut() = None;
            self.render();
            return;
        }
        let (client, original) = {
            let mut st = self.state.borrow_mut();
            let Some(original) = st.reminder(id).cloned() else {
                return;
            };
            let mut rem = original.clone();
            rem.completed = completed;
            rem.completed_at = completed
                .then(|| chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true));
            let keeps_completed =
                st.show_completed || st.view == View::Completed || !st.search.is_empty();
            if completed && !keeps_completed {
                st.lingering.insert(id.to_string(), rem.clone());
            } else {
                st.lingering.remove(id);
            }
            st.upsert(rem);
            (st.client.clone(), original)
        };
        self.render_in_place();

        if completed {
            let weak = Rc::downgrade(self);
            let id = id.to_string();
            glib::timeout_add_local_once(LINGER, move || {
                let Some(win) = weak.upgrade() else { return };
                let still = win
                    .state
                    .borrow()
                    .lingering
                    .get(&id)
                    .is_some_and(|r| r.completed);
                if !still {
                    return;
                }
                let handle = win.rows.borrow().get(&id).cloned();
                let finish = {
                    let win = win.clone();
                    let id = id.clone();
                    move || {
                        win.state.borrow_mut().lingering.remove(&id);
                        win.render();
                    }
                };
                match handle {
                    Some(h) => h.collapse(finish),
                    None => finish(),
                }
            });
        }

        let patch = ReminderPatch {
            completed: Some(completed),
            ..Default::default()
        };
        let id = id.to_string();
        let this = self.clone();
        glib::spawn_future_local(async move {
            let rid = id.clone();
            match bg(move || client.update_reminder(&rid, &patch)).await {
                Ok(rem) => {
                    this.state.borrow_mut().upsert(rem);
                    this.render();
                }
                Err(e) => {
                    {
                        let mut st = this.state.borrow_mut();
                        st.lingering.remove(&id);
                        st.upsert(original);
                    }
                    this.report(&e);
                    this.render();
                }
            }
        });
    }

    fn create(self: &Rc<Self>, title: String) {
        if !self.can_edit() {
            self.new_entry.set_text(&title);
            return;
        }
        let (client, new) = {
            let st = self.state.borrow();
            let Some(list) = st.target_list() else {
                self.toast("There's no list to add to.");
                return;
            };
            let due = matches!(st.view, View::Today | View::Scheduled).then(|| {
                fruity_bridge_client::reminders::Due {
                    date: dates::now().date().format("%Y-%m-%d").to_string(),
                    time: None,
                    time_zone: None,
                }
            });
            (
                st.client.clone(),
                NewReminder {
                    list_id: list.id.clone(),
                    title: title.clone(),
                    due,
                    ..Default::default()
                },
            )
        };
        let this = self.clone();
        glib::spawn_future_local(async move {
            match bg(move || client.create_reminder(&new)).await {
                Ok(rem) => {
                    this.state.borrow_mut().upsert(rem);
                    this.render();
                }
                Err(e) => {
                    if this.new_entry.text().is_empty() {
                        this.new_entry.set_text(&title);
                    }
                    this.report(&e);
                }
            }
        });
    }

    pub fn open_editor(self: &Rc<Self>, id: &str) {
        let st = self.state.borrow();
        let Some(rem) = st.reminder(id).cloned() else {
            return;
        };
        let list_editable = st.list(&rem.list_id).is_none_or(|l| l.editable);
        let read_only = if !st.online {
            Some("Can't reach your Mac — changes are off")
        } else if !list_editable {
            Some("This list is read only")
        } else {
            None
        };
        let lists = st.data.lists.clone();
        drop(st);
        let (w1, w2) = (Rc::downgrade(self), Rc::downgrade(self));
        let (id1, id2) = (id.to_string(), id.to_string());
        editor::present(
            &self.window,
            editor::EditorArgs {
                reminder: &rem,
                lists: &lists,
                read_only,
            },
            move |patch| {
                if let Some(win) = w1.upgrade() {
                    win.save(&id1, patch);
                }
            },
            move || {
                if let Some(win) = w2.upgrade() {
                    win.delete(&id2);
                }
            },
        );
    }

    fn save(self: &Rc<Self>, id: &str, patch: ReminderPatch) {
        if !self.can_edit() {
            return;
        }
        let (client, original) = {
            let mut st = self.state.borrow_mut();
            let Some(original) = st.reminder(id).cloned() else {
                return;
            };
            let mut local = original.clone();
            if let Some(t) = &patch.title {
                local.title = t.clone();
            }
            if let Some(n) = &patch.notes {
                local.notes = Some(n.clone());
            }
            if let Some(d) = &patch.due {
                local.due = d.clone();
            }
            if let Some(p) = patch.priority {
                local.priority = p;
            }
            if let Some(l) = &patch.list_id {
                local.list_id = l.clone();
            }
            if let Some(u) = &patch.url {
                local.url = Some(u.clone());
            }
            st.upsert(local);
            (st.client.clone(), original)
        };
        self.render();
        let this = self.clone();
        let rid = id.to_string();
        glib::spawn_future_local(async move {
            match bg(move || client.update_reminder(&rid, &patch)).await {
                Ok(rem) => {
                    this.state.borrow_mut().upsert(rem);
                    this.render();
                }
                Err(e) => {
                    this.state.borrow_mut().upsert(original);
                    this.report(&e);
                    this.render();
                }
            }
        });
    }

    fn delete(self: &Rc<Self>, id: &str) {
        if !self.can_edit() {
            return;
        }
        let rem = {
            let mut st = self.state.borrow_mut();
            let Some(rem) = st.reminder(id).cloned() else {
                return;
            };
            st.pending_deletes.insert(id.to_string(), rem.clone());
            st.recount();
            rem
        };
        self.render();
        let name = if rem.title.trim().is_empty() {
            "Reminder".to_string()
        } else {
            format!("“{}”", rem.title.trim())
        };
        let toast = adw::Toast::builder()
            .title(format!("{name} deleted"))
            .use_markup(false)
            .button_label("Undo")
            .timeout(5)
            .build();
        let id = id.to_string();
        toast.connect_button_clicked(glib::clone!(
            #[weak(rename_to = win)]
            self,
            #[strong]
            id,
            move |_| {
                win.undo_toasts.borrow_mut().remove(&id);
                win.state.borrow_mut().pending_deletes.remove(&id);
                win.state.borrow_mut().recount();
                win.render();
            }
        ));
        toast.connect_dismissed(glib::clone!(
            #[weak(rename_to = win)]
            self,
            #[strong]
            id,
            move |_| {
                win.undo_toasts.borrow_mut().remove(&id);
                win.commit_delete(&id);
            }
        ));
        self.undo_toasts.borrow_mut().insert(id, toast.clone());
        self.toasts.add_toast(toast);
    }

    fn commit_delete(self: &Rc<Self>, id: &str) {
        if !self.state.borrow().pending_deletes.contains_key(id) {
            return;
        }
        let client = self.state.borrow().client.clone();
        let this = self.clone();
        let rid = id.to_string();
        glib::spawn_future_local(async move {
            let target = rid.clone();
            let result = bg(move || client.delete_reminder(&target)).await;
            {
                let mut st = this.state.borrow_mut();
                st.pending_deletes.remove(&rid);
                match &result {
                    Ok(()) => st.remove(&rid),
                    Err(e) if e.is_not_found() => st.remove(&rid),
                    Err(_) => st.recount(),
                }
            }
            if let Err(e) = result
                && !e.is_not_found()
            {
                this.report(&e);
            }
            this.render();
        });
    }

    /// Called when the window is asked to close. With deletes still waiting on their undo
    /// toast, hides the window and sends them in the background (holding the app open), then
    /// closes it; deletes that fail come back and the window reappears to say so. Returns
    /// whether closing was put off.
    fn flush_deletes_then_close(self: &Rc<Self>) -> bool {
        if self.closing.get() {
            return true;
        }
        let (client, pending): (_, Vec<Reminder>) = {
            let mut st = self.state.borrow_mut();
            let pending = st.pending_deletes.drain().map(|(_, r)| r).collect();
            (st.client.clone().with_write_timeout(FLUSH_TIMEOUT), pending)
        };
        if pending.is_empty() {
            return false;
        }
        self.closing.set(true);
        // Too late to undo; their dismissal finds nothing pending.
        let toasts: Vec<adw::Toast> = self.undo_toasts.borrow_mut().drain().map(|(_, t)| t).collect();
        for t in toasts {
            t.dismiss();
        }
        self.window.set_visible(false);
        let hold = self.window.application().map(|app| app.hold());
        let this = self.clone();
        glib::spawn_future_local(async move {
            let results = bg(move || {
                pending
                    .into_iter()
                    .map(|r| {
                        let result = client.delete_reminder(&r.id);
                        (r, result)
                    })
                    .collect::<Vec<_>>()
            })
            .await;
            this.closing.set(false);
            let mut failed = Vec::new();
            {
                let mut st = this.state.borrow_mut();
                for (r, result) in results {
                    match result {
                        Ok(()) => st.remove(&r.id),
                        Err(e) if e.is_not_found() => st.remove(&r.id),
                        Err(e) => failed.push((r, e)),
                    }
                }
                st.recount();
            }
            if failed.is_empty() {
                // Unless it was reopened meanwhile, finish closing.
                if !this.window.is_visible() {
                    this.window.close();
                }
            } else {
                this.render();
                this.window.present();
                let (r, e) = &failed[0];
                let what = if failed.len() == 1 {
                    format!("“{}”", r.title.trim())
                } else {
                    format!("{} reminders", failed.len())
                };
                let why = if e.is_offline() {
                    "your Mac didn't answer".to_string()
                } else {
                    e.user_message()
                };
                this.toast(&format!("Couldn't delete {what}: {why}"));
            }
            drop(hold);
        });
        true
    }

    // ----- Feedback

    pub fn toast(&self, text: &str) {
        self.toasts.add_toast(
            adw::Toast::builder()
                .title(text)
                .use_markup(false)
                .timeout(4)
                .build(),
        );
    }

    fn report(self: &Rc<Self>, e: &Error) {
        if e.is_offline() {
            self.set_online(false);
            self.toast("Can't reach your Mac, so that change wasn't saved.");
        } else if e.is_conflict() {
            self.toast("Changed on another device");
            self.refresh();
        } else if e.is_not_found() {
            self.toast("That reminder no longer exists.");
            self.refresh();
        } else {
            self.toast(&e.user_message());
        }
    }

    // ----- Small UI helpers

    fn focus_new(self: &Rc<Self>) {
        let can = {
            let st = self.state.borrow();
            st.view != View::Completed && st.target_list().is_some()
        };
        if !can {
            self.select_view(View::All);
        }
        if self.search_bar.is_search_mode() {
            self.search_bar.set_search_mode(false);
        }
        self.split.set_show_content(true);
        if !self.state.borrow().online {
            self.toast(OFFLINE_EDIT);
            return;
        }
        self.new_entry.grab_focus();
    }

    fn start_search(&self) {
        self.split.set_show_content(true);
        self.search_bar.set_search_mode(true);
        self.search_entry.grab_focus();
    }

    pub fn show_preferences(self: &Rc<Self>) {
        let url = self.state.borrow().config.url.clone();
        let weak = Rc::downgrade(self);
        prefs::present(
            &self.window,
            &url,
            prefs::Options {
                description: "Reminders reads and saves through the Fruity Bridge app on your Mac, over Tailscale.",
                permissions: &[
                    prefs::Permission::Reminders,
                    prefs::Permission::NotesRead,
                    prefs::Permission::NotesWrite,
                ],
            },
            move |config| {
                let Some(win) = weak.upgrade() else { return };
                {
                    let mut st = win.state.borrow_mut();
                    st.client = fruity_bridge_client::Client::new(&config);
                    st.config = config;
                }
                win.start_events();
                win.refresh();
            },
        );
    }

    fn show_shortcuts(&self) {
        fruity_bridge_gtk::shortcuts_dialog(
            "Reminders",
            &[
                ("New Reminder", "<Control>n"),
                ("Search All Lists", "<Control>f"),
                ("Show Completed", "<Control><Shift>h"),
                ("Refresh", "<Control>r"),
            ],
        )
        .present(Some(&self.window));
    }

    fn show_about(&self) {
        fruity_bridge_gtk::about_dialog(
            "Reminders",
            crate::APP_ID,
            env!("CARGO_PKG_VERSION"),
            "Your Apple Reminders on Linux, served by your Mac over Tailscale.",
        )
        .present(Some(&self.window));
    }
}
