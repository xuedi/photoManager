//! The tools view: what the edits work on, the edits, and the preview one pushes once its form
//! is filled in. The edits themselves live in `core`; each asks its value in a form of its own.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use adw::prelude::*;
use adw::subclass::prelude::*;
use gtk::glib;
use gtk::glib::subclass::InitializingObject;
use photomanager_core::browse;
use photomanager_core::changeset::{ChangeSet, Wanted};
use photomanager_core::edits::{self, Edit, Value};
use photomanager_core::filter::Filter;
use photomanager_core::scope::Scope;

use crate::library::{Event, Library};
use crate::preview::Preview;

mod imp {
    use super::*;

    #[derive(Debug, Default, gtk::CompositeTemplate)]
    #[template(resource = "/org/beijingcode/PhotoManager/tools.ui")]
    pub struct Tools {
        #[template_child]
        pub toasts: TemplateChild<adw::ToastOverlay>,
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
        pub library: RefCell<Option<Rc<Library>>>,
        /// What the tools work on. The whole library until something else is chosen.
        pub scope: RefCell<Option<Scope>>,
        /// What the gallery last handed over with Use as Scope.
        pub picked: RefCell<Option<Scope>>,
        /// How many photos the scope names, `None` while it is counted.
        pub photos: Cell<Option<usize>>,
        pub toast: RefCell<String>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for Tools {
        const NAME: &'static str = "PmTools";
        type Type = super::Tools;
        type ParentType = adw::Bin;

        fn class_init(klass: &mut Self::Class) {
            Preview::ensure_type();
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
            tools.list_edits();
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
        if let Some(library) = &library {
            let tools = self.downgrade();
            library.connect_changed(move || {
                if let Some(tools) = tools.upgrade() {
                    tools.count_scope();
                }
            });
        }
        *self.imp().library.borrow_mut() = library;
        self.count_scope();
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
        self.count_scope();
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

    /// How many photos the scope names, once counted.
    pub fn scope_photos(&self) -> Option<usize> {
        self.imp().photos.get()
    }

    /// An edit by key: its form, or with its value after a `:`, the preview straight away.
    /// Nothing is written.
    pub fn run(&self, asked: &str) {
        let Some(library) = self.imp().library.borrow().clone() else {
            return;
        };
        let (key, value) = match asked.split_once(':') {
            Some((key, value)) => (key, Some(value)),
            None => (asked, None),
        };
        let Some(edit) = Edit::find(key) else {
            tracing::warn!(edit = key, "no such edit");
            return;
        };
        match value {
            None => crate::forms::present(self, &library, edit, &self.scope()),
            Some(text) => match edit.read(text) {
                Ok(value) => self.preview_edit(edit, value),
                Err(why) => self.say(&format!("{}: {why}", edit.title())),
            },
        }
    }

    /// The change set of an edit with its value over the scope, pushed as the preview.
    pub fn preview_edit(&self, edit: Edit, value: Value) {
        let Some(library) = self.imp().library.borrow().clone() else {
            return;
        };
        tracing::info!(edit = edit.key(), scope = self.scope().title(), "edit previewed");
        let tools = self.downgrade();
        library.run_edit(edit, value, &self.scope(), move |event| {
            let Some(tools) = tools.upgrade() else {
                return;
            };
            match event {
                Event::Previewed(set) => tools.show(set),
                Event::Failed(why) => {
                    tools.say(&format!("{}: {why}", edit.title()));
                    tracing::error!(why, "the edit could not be previewed");
                }
                _ => {}
            }
        });
    }

    /// A change set made by hand rather than by an edit. Nothing is written.
    pub fn preview_change_set(&self, title: &str, wanted: Vec<Wanted>) {
        let Some(library) = self.imp().library.borrow().clone() else {
            return;
        };
        let tools = self.downgrade();
        library.preview(title, wanted, move |event| {
            let Some(tools) = tools.upgrade() else {
                return;
            };
            match event {
                Event::Previewed(set) => tools.show(set),
                Event::Failed(why) => tracing::error!(why, "the preview could not be built"),
                _ => {}
            }
        });
    }

    /// Says something on the page the tools are on.
    pub fn say(&self, text: &str) {
        *self.imp().toast.borrow_mut() = text.to_string();
        self.imp().toasts.add_toast(adw::Toast::new(text));
    }

    pub fn toast(&self) -> String {
        self.imp().toast.borrow().clone()
    }

    pub fn show(&self, set: ChangeSet) {
        tracing::info!(title = set.title, photos = set.rows.len(), "change set previewed");
        self.imp().preview.show(set);
        let nav = &self.imp().nav;
        if nav.visible_page_tag().as_deref() != Some("preview") {
            nav.pop_to_tag("tools");
            nav.push_by_tag("preview");
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

    fn list_edits(&self) {
        let imp = self.imp();
        for edit in edits::ALL {
            let row = adw::ActionRow::builder()
                .title(edit.title())
                .subtitle(edit.does())
                .activatable(true)
                .action_name("win.run-edit")
                .action_target(&edit.key().to_variant())
                .build();
            row.add_suffix(
                &gtk::Image::builder()
                    .icon_name("go-next-symbolic")
                    .accessible_role(gtk::AccessibleRole::Presentation)
                    .build(),
            );
            imp.tools_group.add(&row);
        }
        let none = edits::ALL.is_empty();
        imp.tools_group.set_visible(!none);
        imp.scope_group.set_visible(!none);
        imp.empty.set_visible(none);
    }

    fn show_scope(&self) {
        let imp = self.imp();
        let scope = self.scope();
        imp.scope_row.set_title(&glib::markup_escape_text(&scope_title(&scope)));
        imp.scope_row.set_subtitle(&match imp.photos.get() {
            Some(1) => "1 photo".to_string(),
            Some(photos) => format!("{photos} photos"),
            None => "Counting".to_string(),
        });
    }

    /// Counts the scope again: after it changed, and after every scan.
    fn count_scope(&self) {
        let imp = self.imp();
        let Some(library) = imp.library.borrow().clone() else {
            return;
        };
        imp.photos.set(None);
        self.show_scope();
        let scope = self.scope();
        let tools = self.downgrade();
        library.scope_count(&scope.clone(), move |counted| {
            let Some(tools) = tools.upgrade() else {
                return;
            };
            if tools.scope() != scope {
                return;
            }
            match counted {
                Ok(photos) => tools.imp().photos.set(Some(photos)),
                Err(why) => tracing::error!(why, "the scope could not be counted"),
            }
            tools.show_scope();
        });
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
