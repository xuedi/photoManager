//! The tools view: what the tools work on, the tools with what each would change right now, and
//! the preview one pushes when it is opened. The tools themselves live in `core`; this lists
//! whatever is there and knows none of them by name.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use adw::prelude::*;
use adw::subclass::prelude::*;
use gtk::glib;
use gtk::glib::subclass::InitializingObject;
use photomanager_core::browse;
use photomanager_core::changeset::{ChangeSet, Wanted};
use photomanager_core::filter::Filter;
use photomanager_core::scope::Scope;
use photomanager_core::tools;

use crate::history::History;
use crate::library::{Counted, Event, Library};
use crate::preview::Preview;
use crate::questions::Questions;

/// A tool's row, and the label that says what it would change.
#[derive(Debug)]
pub struct Listed {
    key: &'static str,
    count: gtk::Label,
}

mod imp {
    use super::*;

    #[derive(Debug, Default, gtk::CompositeTemplate)]
    #[template(resource = "/org/beijingcode/PhotoManager/tools.ui")]
    pub struct Tools {
        #[template_child]
        pub nav: TemplateChild<adw::NavigationView>,
        #[template_child]
        pub scope_group: TemplateChild<adw::PreferencesGroup>,
        #[template_child]
        pub scope_row: TemplateChild<adw::ActionRow>,
        #[template_child]
        pub tools_group: TemplateChild<adw::PreferencesGroup>,
        #[template_child]
        pub empty: TemplateChild<adw::StatusPage>,
        #[template_child]
        pub preview: TemplateChild<Preview>,
        #[template_child]
        pub history: TemplateChild<History>,
        #[template_child]
        pub questions: TemplateChild<Questions>,
        pub library: RefCell<Option<Rc<Library>>>,
        /// What the tools work on. The whole library until something else is chosen.
        pub scope: RefCell<Option<Scope>>,
        /// What the gallery last handed over with Use as Scope.
        pub picked: RefCell<Option<Scope>>,
        pub listed: RefCell<Vec<Listed>>,
        /// The newest count, `None` while it is being counted.
        pub counted: RefCell<Option<Counted>>,
        /// Which count is the newest asked for, so an older one that arrives late is dropped.
        pub asked: Cell<u64>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for Tools {
        const NAME: &'static str = "PmTools";
        type Type = super::Tools;
        type ParentType = adw::Bin;

        fn class_init(klass: &mut Self::Class) {
            Preview::ensure_type();
            History::ensure_type();
            Questions::ensure_type();
            klass.bind_template();
        }

        fn instance_init(obj: &InitializingObject<Self>) {
            obj.init_template();
        }
    }

    impl ObjectImpl for Tools {
        fn constructed(&self) {
            self.parent_constructed();
            let tools = self.obj();
            tools.list_tools();
            tools.show_scope();
            self.scope_row.connect_activated(glib::clone!(
                #[weak]
                tools,
                move |_| tools.choose_scope()
            ));
        }
    }

    impl WidgetImpl for Tools {}
    impl BinImpl for Tools {}
}

glib::wrapper! {
    pub struct Tools(ObjectSubclass<imp::Tools>)
        @extends adw::Bin, gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl Default for Tools {
    fn default() -> Self {
        glib::Object::builder().build()
    }
}

impl Tools {
    pub fn set_library(&self, library: Option<Rc<Library>>) {
        self.imp().preview.set_library(library.clone());
        self.imp().history.set_library(library.clone());
        self.imp().questions.set_library(library.clone());
        if let Some(library) = &library {
            let tools = self.downgrade();
            library.connect_changed(move || {
                if let Some(tools) = tools.upgrade() {
                    tools.recount();
                    tools.ask_again();
                }
            });
        }
        *self.imp().library.borrow_mut() = library;
        self.recount();
    }

    pub fn preview(&self) -> Preview {
        self.imp().preview.clone()
    }

    pub fn scope(&self) -> Scope {
        self.imp()
            .scope
            .borrow()
            .clone()
            .unwrap_or_else(|| Scope::Filter(Filter::all()))
    }

    pub fn set_scope(&self, scope: Scope) {
        tracing::info!(scope = scope.title(), "scope set");
        *self.imp().scope.borrow_mut() = Some(scope);
        self.show_scope();
        self.recount();
        self.ask_again();
    }

    /// What the gallery hands over becomes the scope, and stays on offer in the scope dialog.
    pub fn pick(&self, scope: Scope) {
        *self.imp().picked.borrow_mut() = Some(scope.clone());
        self.set_scope(scope);
    }

    pub fn picked(&self) -> Option<Scope> {
        self.imp().picked.borrow().clone()
    }

    /// A scope in written form: `all`, `picked` for what the gallery handed over, or a folder.
    pub fn choose(&self, written: &str) -> bool {
        let scope = match written {
            "all" => Scope::Filter(Filter::all()),
            "picked" => match self.picked() {
                Some(scope) => scope,
                None => return false,
            },
            folder => Scope::Filter(Filter::all().within(folder)),
        };
        self.set_scope(scope);
        true
    }

    /// How many photos the scope names and what each tool would change, once counted.
    pub fn counted(&self) -> Option<Counted> {
        self.imp().counted.borrow().clone()
    }

    /// A tool by key, with its settings as text after a `:` if there are any; without, the ones
    /// it was last given. A tool that asks shows its questions first; any other builds its change
    /// set for the scope and shows it. Nothing is written.
    pub fn run(&self, asked: &str) {
        let Some(library) = self.imp().library.borrow().clone() else {
            return;
        };
        let (key, settings) = match asked.split_once(':') {
            Some((key, settings)) => (key, Some(settings.to_string())),
            None => (asked, None),
        };
        let Some(tool) = tools::find(key) else {
            tracing::warn!(tool = key, "no such tool");
            return;
        };
        let settings = match settings {
            Some(settings) => {
                library.remember_tool_settings(key, &settings);
                Some(settings)
            }
            None => library.tool_settings(key),
        };
        tracing::info!(tool = key, scope = self.scope().title(), "tool opened");
        if tool.asks() {
            self.imp().questions.open(key, &self.scope());
            let nav = &self.imp().nav;
            nav.pop_to_tag("tools");
            nav.push_by_tag("questions");
            return;
        }
        let tools = self.downgrade();
        library.run_tool(key, settings, &self.scope(), move |event| {
            let Some(tools) = tools.upgrade() else {
                return;
            };
            match event {
                Event::Previewed(set) => tools.show(set),
                Event::Failed(why) => tracing::error!(why, "the tool could not be run"),
                _ => {}
            }
        });
    }

    pub fn questions(&self) -> Questions {
        self.imp().questions.clone()
    }

    /// One answer to one question of the tool whose questions are shown.
    pub fn answer(&self, key: &str, question: &str, answer: &str) {
        let questions = &self.imp().questions;
        if questions.key().as_deref() != Some(key) {
            tracing::warn!(tool = key, "its questions are not the ones shown");
            return;
        }
        if questions.answer(question, answer) {
            self.recount();
        }
    }

    pub fn answer_exact(&self, key: &str) {
        let questions = &self.imp().questions;
        if questions.key().as_deref() != Some(key) {
            tracing::warn!(tool = key, "its questions are not the ones shown");
            return;
        }
        if questions.answer_exact() > 0 {
            self.recount();
        }
    }

    /// The change set of the tool whose questions are shown, with the answers so far.
    pub fn preview_answers(&self) {
        let imp = self.imp();
        let (Some(library), Some(key)) = (imp.library.borrow().clone(), imp.questions.key()) else {
            return;
        };
        tracing::info!(tool = key, scope = self.scope().title(), "answers previewed");
        let tools = self.downgrade();
        library.run_tool(&key, imp.questions.settings(), &self.scope(), move |event| {
            let Some(tools) = tools.upgrade() else {
                return;
            };
            match event {
                Event::Previewed(set) => tools.show(set),
                Event::Failed(why) => tracing::error!(why, "the answers could not be previewed"),
                _ => {}
            }
        });
    }

    /// The questions on screen are asked again, after a scan or for another scope.
    fn ask_again(&self) {
        let questions = &self.imp().questions;
        if questions.key().is_some() {
            questions.ask(&self.scope());
        }
    }

    /// A change set made by hand rather than by a tool. Nothing is written.
    pub fn preview_change_set(&self, title: &str, wanted: Vec<Wanted>) {
        let Some(library) = self.imp().library.borrow().clone() else {
            return;
        };
        let tools = self.clone();
        library.preview(title, wanted, move |event| match event {
            Event::Previewed(set) => tools.show(set),
            Event::Failed(why) => tracing::error!(why, "the preview could not be built"),
            _ => {}
        });
    }

    pub fn show(&self, set: ChangeSet) {
        tracing::info!(title = set.title, photos = set.rows.len(), "change set previewed");
        self.imp().preview.show(set);
        let nav = &self.imp().nav;
        if nav.visible_page_tag().as_deref() != Some("preview") {
            nav.push_by_tag("preview");
        }
    }

    pub fn history(&self) -> History {
        self.imp().history.clone()
    }

    pub fn show_history(&self) {
        self.imp().history.refresh();
        let nav = &self.imp().nav;
        match self.showing().as_str() {
            "history" => {}
            "pass" => {
                nav.pop_to_tag("history");
            }
            _ => {
                nav.pop_to_tag("tools");
                nav.push_by_tag("history");
            }
        }
    }

    /// Opens one pass of the history on a page of its own.
    pub fn show_pass(&self, batch: i64) {
        let Some(page) = self.imp().history.details(batch) else {
            tracing::warn!(batch, "no such pass");
            return;
        };
        if self.showing() != "history" {
            self.show_history();
        }
        self.imp().nav.push(&page);
    }

    pub fn take_back(&self, batch: i64) {
        self.imp().history.take_back(batch);
    }

    /// Leaves a pass's page for the list, where what a take-back came to is shown.
    pub fn back_to_history(&self) {
        if self.showing() == "pass" {
            self.imp().nav.pop_to_tag("history");
        }
    }

    pub fn showing(&self) -> String {
        self.imp()
            .nav
            .visible_page()
            .and_then(|page| page.tag())
            .map(|tag| tag.to_string())
            .unwrap_or_default()
    }

    fn list_tools(&self) {
        let imp = self.imp();
        for tool in tools::ALL {
            let count = gtk::Label::new(None);
            count.add_css_class("dim-label");
            let row = adw::ActionRow::builder()
                .title(tool.title())
                .subtitle(tool.fixes())
                .activatable(true)
                .action_name("win.run-tool")
                .action_target(&tool.key().to_variant())
                .build();
            row.add_suffix(&count);
            row.add_suffix(
                &gtk::Image::builder()
                    .icon_name("go-next-symbolic")
                    .accessible_role(gtk::AccessibleRole::Presentation)
                    .build(),
            );
            imp.tools_group.add(&row);
            imp.listed.borrow_mut().push(Listed { key: tool.key(), count });
        }
        let none = tools::ALL.is_empty();
        imp.tools_group.set_visible(!none);
        imp.scope_group.set_visible(!none);
        imp.empty.set_visible(none);
    }

    fn show_scope(&self) {
        let imp = self.imp();
        let scope = self.scope();
        imp.scope_row.set_title(&glib::markup_escape_text(&scope_title(&scope)));
        let photos = imp.counted.borrow().as_ref().map(|counted| counted.photos);
        imp.scope_row.set_subtitle(&match photos {
            Some(1) => "1 photo".to_string(),
            Some(photos) => format!("{photos} photos"),
            None => "Counting".to_string(),
        });
    }

    /// Counts again: after the scope changed, and after every scan.
    fn recount(&self) {
        let imp = self.imp();
        let Some(library) = imp.library.borrow().clone() else {
            return;
        };
        let asked = imp.asked.get() + 1;
        imp.asked.set(asked);
        *imp.counted.borrow_mut() = None;
        self.show_counts();

        let tools = self.downgrade();
        library.count_tools(&self.scope(), move |counted| {
            let Some(tools) = tools.upgrade() else {
                return;
            };
            if tools.imp().asked.get() != asked {
                return;
            }
            match counted {
                Ok(counted) => *tools.imp().counted.borrow_mut() = Some(counted),
                Err(why) => tracing::error!(why, "the tools could not be counted"),
            }
            tools.show_counts();
        });
    }

    fn show_counts(&self) {
        let imp = self.imp();
        let counted = imp.counted.borrow();
        for listed in imp.listed.borrow().iter() {
            let found = counted
                .as_ref()
                .and_then(|counted| counted.tools.iter().find(|(key, _)| key == listed.key))
                .map(|(_, count)| count);
            let waiting = counted
                .as_ref()
                .and_then(|counted| counted.waiting.iter().find(|(key, _)| key == listed.key))
                .map(|(_, waiting)| *waiting)
                .unwrap_or_default();
            let tool = tools::find(listed.key);
            listed.count.set_label(&match found {
                None => "Counting".to_string(),
                Some(Ok(0)) if waiting > 0 => tool.map(|tool| tool.waiting(waiting)).unwrap_or_default(),
                Some(Ok(0)) => "Nothing to do here".to_string(),
                Some(Ok(1)) => "1 photo would change".to_string(),
                Some(Ok(count)) => format!("{count} photos would change"),
                Some(Err(_)) => "Could not be counted".to_string(),
            });
        }
        drop(counted);
        self.show_scope();
    }

    /// The scope dialog: the whole library, what the gallery handed over, or a country or an
    /// event from the place tree, with a search over the names.
    fn choose_scope(&self) {
        let Some(library) = self.imp().library.borrow().clone() else {
            return;
        };
        let tools = self.downgrade();
        library.sidebars(move |found| {
            let Some(tools) = tools.upgrade() else {
                return;
            };
            match found {
                Ok(sidebars) => tools.present_scopes(sidebars.places),
                Err(why) => tracing::error!(why, "the places could not be read"),
            }
        });
    }

    fn present_scopes(&self, places: Vec<browse::Place>) {
        let list = gtk::ListBox::builder()
            .selection_mode(gtk::SelectionMode::None)
            .valign(gtk::Align::Start)
            .build();
        list.add_css_class("boxed-list");

        let dialog = adw::Dialog::builder()
            .title("Scope")
            .content_width(420)
            .content_height(560)
            .build();
        // Each row with the words a search finds it by.
        let searched: Rc<RefCell<Vec<(gtk::ListBoxRow, String)>>> = Rc::default();
        let choice = |icon: &str, title: &str, subtitle: &str, written: &str, words: &str| {
            let row = adw::ActionRow::builder()
                .title(glib::markup_escape_text(title))
                .subtitle(glib::markup_escape_text(subtitle))
                .activatable(true)
                .build();
            row.add_prefix(
                &gtk::Image::builder()
                    .icon_name(icon)
                    .accessible_role(gtk::AccessibleRole::Presentation)
                    .build(),
            );
            let written = written.to_string();
            row.connect_activated(glib::clone!(
                #[weak]
                dialog,
                move |row| {
                    if let Err(error) = WidgetExt::activate_action(row, "win.tools-scope", Some(&written.to_variant()))
                    {
                        tracing::error!(%error, "the scope could not be set");
                    }
                    dialog.close();
                }
            ));
            list.append(&row);
            searched.borrow_mut().push((row.upcast(), words.to_lowercase()));
        };

        choice(
            "view-grid-symbolic",
            "Whole Library",
            "Every photo",
            "all",
            "whole library",
        );
        if let Some(picked) = self.picked() {
            choice(
                "image-x-generic-symbolic",
                &picked.title(),
                "Handed over from the gallery",
                "picked",
                "gallery",
            );
        }
        for country in &places {
            choice(
                "mark-location-symbolic",
                &country.name,
                &photos(country.photos),
                &country.folder,
                &country.name,
            );
            for event in &country.events {
                choice(
                    "folder-symbolic",
                    &event.name,
                    &format!("{} - {}", country.name, photos(event.photos)),
                    &event.folder,
                    &format!("{} {}", country.name, event.name),
                );
            }
        }

        let wanted: Rc<RefCell<String>> = Rc::default();
        list.set_filter_func(glib::clone!(
            #[strong]
            wanted,
            move |row| {
                let wanted = wanted.borrow();
                wanted.is_empty()
                    || searched
                        .borrow()
                        .iter()
                        .any(|(each, words)| each == row && words.contains(wanted.as_str()))
            }
        ));
        let search = gtk::SearchEntry::builder()
            .placeholder_text("Search Places")
            .hexpand(true)
            .build();
        search.update_property(&[gtk::accessible::Property::Label("Search Places")]);
        search.connect_search_changed(glib::clone!(
            #[weak]
            list,
            move |search| {
                *wanted.borrow_mut() = search.text().to_lowercase();
                list.invalidate_filter();
            }
        ));

        let content = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(12)
            .margin_top(12)
            .margin_bottom(12)
            .margin_start(12)
            .margin_end(12)
            .build();
        content.append(&search);
        content.append(&list);
        let scrolled = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vexpand(true)
            .child(&content)
            .build();
        let view = adw::ToolbarView::new();
        view.add_top_bar(&adw::HeaderBar::new());
        view.set_content(Some(&scrolled));
        dialog.set_child(Some(&view));
        dialog.present(Some(self));
    }
}

/// What the scope row calls a scope: a place by its own name, anything else by its title.
fn scope_title(scope: &Scope) -> String {
    match scope {
        Scope::Filter(filter) if filter.kinds().is_empty() => match &filter.within {
            None => "Whole Library".to_string(),
            Some(folder) => folder.rsplit('/').next().unwrap_or(folder).to_string(),
        },
        other => other.title(),
    }
}

fn photos(count: i64) -> String {
    match count {
        1 => "1 photo".to_string(),
        count => format!("{count} photos"),
    }
}
