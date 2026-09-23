//! What a tool would change, on screen: a summary, a table a person can trim, the exact diff of
//! one photo on request, and the apply that is the only way anything here reaches a photo.
//!
//! The table is virtualised - a `GtkColumnView` over a list model - because a change set over the
//! whole library is one row per photo and a widget per row would take seconds to build. It is the
//! one place in the application with a list model, so the row item is kept to the strings the
//! columns show and bound by hand in the factory, not through properties.

use std::cell::{Cell, OnceCell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;

use adw::prelude::*;
use adw::subclass::prelude::*;
use gtk::glib::subclass::InitializingObject;
use gtk::{gio, glib, pango};

use photomanager_core::changeset::{ChangeSet, Counts};
use photomanager_core::journal::Kind;
use photomanager_core::write::{Assignment, Engine, Summary};

use crate::library::{Event, Library};

/// The exact change to one photo, as the engine answered it.
type Exact = Result<Vec<Assignment>, String>;

mod item {
    use super::*;

    mod imp {
        use super::*;

        #[derive(Debug, Default)]
        pub struct Item {
            pub index: Cell<u32>,
            pub photo: RefCell<String>,
            pub change: RefCell<String>,
            pub state: RefCell<String>,
            pub selected: Cell<bool>,
            pub selectable: Cell<bool>,
        }

        #[glib::object_subclass]
        impl ObjectSubclass for Item {
            const NAME: &'static str = "PmPreviewItem";
            type Type = super::Item;
        }

        impl ObjectImpl for Item {}
    }

    glib::wrapper! {
        pub struct Item(ObjectSubclass<imp::Item>);
    }

    impl Item {
        pub fn of(index: usize, row: &photomanager_core::changeset::Row) -> Item {
            let item: Item = glib::Object::builder().build();
            let held = item.imp();
            held.index.set(index as u32);
            *held.photo.borrow_mut() = row.rel_path.clone();
            *held.change.borrow_mut() = row.tells();
            *held.state.borrow_mut() = row.verdict.tells();
            held.selected.set(row.selected());
            held.selectable.set(row.would_change());
            item
        }

        pub fn index(&self) -> u32 {
            self.imp().index.get()
        }

        pub fn photo(&self) -> String {
            self.imp().photo.borrow().clone()
        }

        pub fn change(&self) -> String {
            self.imp().change.borrow().clone()
        }

        pub fn state(&self) -> String {
            self.imp().state.borrow().clone()
        }

        pub fn selected(&self) -> bool {
            self.imp().selected.get()
        }

        pub fn selectable(&self) -> bool {
            self.imp().selectable.get()
        }

        pub fn set_selected(&self, selected: bool) {
            self.imp().selected.set(selected);
        }
    }
}

pub use item::Item;

mod imp {
    use super::*;

    #[derive(Debug, Default, gtk::CompositeTemplate)]
    #[template(resource = "/org/beijingcode/PhotoManager/preview.ui")]
    pub struct Preview {
        #[template_child]
        pub toasts: TemplateChild<adw::ToastOverlay>,
        #[template_child]
        pub title: TemplateChild<adw::WindowTitle>,
        #[template_child]
        pub apply_button: TemplateChild<gtk::Button>,
        #[template_child]
        pub cancel_button: TemplateChild<gtk::Button>,
        #[template_child]
        pub select_all_button: TemplateChild<gtk::Button>,
        #[template_child]
        pub select_none_button: TemplateChild<gtk::Button>,
        #[template_child]
        pub summary: TemplateChild<adw::PreferencesGroup>,
        #[template_child]
        pub traffic: TemplateChild<gtk::Label>,
        #[template_child]
        pub progress: TemplateChild<gtk::ProgressBar>,
        #[template_child]
        pub table: TemplateChild<gtk::ColumnView>,
        #[template_child]
        pub detail: TemplateChild<adw::PreferencesGroup>,

        pub library: RefCell<Option<Rc<Library>>>,
        pub set: RefCell<Option<ChangeSet>>,
        pub store: OnceCell<gio::ListStore>,
        /// The engine the row details are asked through, started when the first row is opened.
        pub engine: RefCell<Option<Engine>>,
        /// The exact diffs already fetched, so a row is only ever asked about once.
        pub exact: RefCell<HashMap<usize, Exact>>,
        pub asked: Cell<usize>,
        /// A checkbox set from the model is not a person clicking it.
        pub binding: Cell<bool>,
        pub summary_rows: RefCell<Vec<adw::ActionRow>>,
        pub detail_rows: RefCell<Vec<adw::ActionRow>>,
        pub applied: RefCell<Option<(Kind, Summary)>>,
        pub toast: RefCell<String>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for Preview {
        const NAME: &'static str = "PmPreview";
        type Type = super::Preview;
        type ParentType = adw::Bin;

        fn class_init(klass: &mut Self::Class) {
            klass.bind_template();
        }

        fn instance_init(obj: &InitializingObject<Self>) {
            obj.init_template();
        }
    }

    impl ObjectImpl for Preview {
        fn constructed(&self) {
            self.parent_constructed();
            self.obj().build_table();
            self.obj().show_summary();
        }
    }

    impl WidgetImpl for Preview {}
    impl BinImpl for Preview {}
}

glib::wrapper! {
    pub struct Preview(ObjectSubclass<imp::Preview>)
        @extends adw::Bin, gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl Default for Preview {
    fn default() -> Self {
        glib::Object::builder().build()
    }
}

impl Preview {
    pub fn set_library(&self, library: Option<Rc<Library>>) {
        *self.imp().library.borrow_mut() = library;
    }

    /// Puts a change set on screen. Nothing has been written at this point and nothing will be
    /// until the apply button is pressed.
    pub fn show(&self, set: ChangeSet) {
        let imp = self.imp();
        imp.title.set_title(&set.title);
        imp.exact.borrow_mut().clear();
        *imp.applied.borrow_mut() = None;
        imp.binding.set(true);
        let store = self.store();
        store.remove_all();
        for (index, row) in set.rows.iter().enumerate() {
            store.append(&Item::of(index, row));
        }
        imp.binding.set(false);
        *imp.set.borrow_mut() = Some(set);
        self.hide_detail();
        self.show_summary();
    }

    pub fn title(&self) -> Option<String> {
        self.imp().set.borrow().as_ref().map(|set| set.title.clone())
    }

    pub fn counts(&self) -> Option<Counts> {
        self.imp().set.borrow().as_ref().map(ChangeSet::counts)
    }

    /// How many rows have had their exact diff fetched: one ExifTool read each.
    pub fn asked(&self) -> usize {
        self.imp().asked.get()
    }

    pub fn applied(&self) -> Option<(Kind, Summary)> {
        self.imp().applied.borrow().clone()
    }

    pub fn toast(&self) -> String {
        self.imp().toast.borrow().clone()
    }

    pub fn is_busy(&self) -> bool {
        self.imp().cancel_button.is_visible()
    }

    pub fn select(&self, index: usize, selected: bool) {
        let imp = self.imp();
        if imp.binding.get() {
            return;
        }
        let changed = match imp.set.borrow_mut().as_mut() {
            Some(set) => set.select(index, selected),
            None => false,
        };
        if !changed {
            return;
        }
        if let Some(item) = self.store().item(index as u32).and_downcast::<Item>() {
            item.set_selected(selected);
        }
        self.show_summary();
    }

    pub fn select_all(&self) {
        self.select_every(true);
    }

    pub fn select_none(&self) {
        self.select_every(false);
    }

    fn select_every(&self, selected: bool) {
        let imp = self.imp();
        match imp.set.borrow_mut().as_mut() {
            Some(set) if selected => set.select_all(),
            Some(set) => set.select_none(),
            None => return,
        }
        imp.binding.set(true);
        if let Some(set) = imp.set.borrow().as_ref() {
            for (index, row) in set.rows.iter().enumerate() {
                if let Some(item) = self.store().item(index as u32).and_downcast::<Item>() {
                    item.set_selected(row.selected());
                }
            }
        }
        imp.binding.set(false);
        self.store()
            .items_changed(0, self.store().n_items(), self.store().n_items());
        self.show_summary();
    }

    /// The exact tag-level diff of one row, asked of the engine once and kept.
    pub fn details(&self, index: usize) {
        let imp = self.imp();
        if let Some(known) = imp.exact.borrow().get(&index).cloned() {
            self.show_detail(index, &known);
            return;
        }
        let Some(library) = imp.library.borrow().clone() else {
            return;
        };
        if imp.engine.borrow().is_none() {
            match library.engine() {
                Ok(engine) => *imp.engine.borrow_mut() = Some(engine),
                Err(why) => {
                    self.show_detail(index, &Err(why));
                    return;
                }
            }
        }

        let found = {
            let mut engine = imp.engine.borrow_mut();
            let engine = engine.as_mut().expect("an engine");
            match imp.set.borrow().as_ref() {
                Some(set) => set.exact(index, engine),
                None => return,
            }
        };
        imp.asked.set(imp.asked.get() + 1);
        imp.exact.borrow_mut().insert(index, found.clone());
        self.show_detail(index, &found);
    }

    /// Asks before the first write of all, then writes.
    pub fn apply(&self) {
        let Some(library) = self.imp().library.borrow().clone() else {
            return;
        };
        if library.is_busy() || self.is_busy() {
            return;
        }
        let preview = self.downgrade();
        crate::confirm::before_first_write(self, &library, move || {
            if let Some(preview) = preview.upgrade() {
                preview.write();
            }
        });
    }

    pub fn cancel(&self) {
        if let Some(library) = self.imp().library.borrow().as_ref() {
            library.cancel();
            self.imp().progress.set_text(Some("Stopping"));
        }
    }

    /// Takes the last applied change set back, after its own confirmation.
    pub fn undo(&self) {
        let Some(library) = self.imp().library.borrow().clone() else {
            return;
        };
        if library.is_busy() || self.is_busy() {
            return;
        }
        let Some(pass) = library.undoable() else {
            self.say("There is nothing to take back.", false);
            return;
        };
        let preview = self.downgrade();
        crate::confirm::before_undo(self, &pass, move || {
            let Some(preview) = preview.upgrade() else {
                return;
            };
            let Some(library) = preview.imp().library.borrow().clone() else {
                return;
            };
            if library.is_busy() {
                preview.say("Something else is running, so nothing was taken back.", false);
                return;
            }
            preview.running(true, "Putting it back");
            library.undo_last(move |event| preview.report(event));
        });
    }

    fn write(&self) {
        let Some(library) = self.imp().library.borrow().clone() else {
            return;
        };
        let Some(set) = self.imp().set.borrow().clone() else {
            return;
        };
        if library.is_busy() {
            self.say("Something else is running, so nothing was written.", false);
            return;
        }
        self.running(true, "Writing");
        let preview = self.clone();
        library.apply(&set, move |event| preview.report(event));
    }

    fn report(&self, event: Event) {
        match event {
            Event::Done(done, total) => {
                let progress = &self.imp().progress;
                progress.set_fraction(done as f64 / total.max(1) as f64);
                progress.set_text(Some(&format!("{done} of {total}")));
            }
            Event::Applied(kind, summary) => {
                self.running(false, "");
                if let Some(set) = self.imp().set.borrow_mut().as_mut() {
                    match kind {
                        Kind::Write => set.settle(&summary),
                        Kind::Undo => set.unsettle(&summary),
                    }
                }
                self.refill();
                let told = told(kind, &summary);
                *self.imp().applied.borrow_mut() = Some((kind, summary));
                self.say(&told, kind == Kind::Write);
                self.show_summary();
                self.read_the_photos_back();
            }
            Event::Failed(why) => {
                self.running(false, "");
                self.say(&format!("Did not work: {why}"), false);
                tracing::error!(why, "the change set was not applied");
            }
            _ => {}
        }
    }

    /// The rows now carry what became of them, so the table says so too.
    fn refill(&self) {
        let imp = self.imp();
        imp.binding.set(true);
        let store = self.store();
        store.remove_all();
        if let Some(set) = imp.set.borrow().as_ref() {
            for (index, row) in set.rows.iter().enumerate() {
                store.append(&Item::of(index, row));
            }
        }
        imp.binding.set(false);
        imp.exact.borrow_mut().clear();
        self.hide_detail();
    }

    /// A written photo is forgotten by the engine so that nothing stale is believed about it, so
    /// after a pass the library is read again - the photos are only read, never touched.
    fn read_the_photos_back(&self) {
        if let Some(window) = self.root().and_downcast::<crate::window::Window>() {
            window.scan(photomanager_core::scan::Mode::Reconcile);
        }
    }

    fn say(&self, text: &str, undoable: bool) {
        *self.imp().toast.borrow_mut() = text.to_string();
        let toast = adw::Toast::new(text);
        if undoable {
            toast.set_button_label(Some("Undo"));
            toast.set_action_name(Some("win.undo-last"));
        }
        self.imp().toasts.add_toast(toast);
    }

    fn running(&self, busy: bool, note: &str) {
        let imp = self.imp();
        imp.cancel_button.set_visible(busy);
        imp.apply_button.set_visible(!busy);
        imp.progress.set_visible(busy);
        imp.select_all_button.set_sensitive(!busy);
        imp.select_none_button.set_sensitive(!busy);
        if busy {
            imp.progress.set_fraction(0.0);
            imp.progress.set_text(Some(note));
        }
    }

    fn show_summary(&self) {
        let imp = self.imp();
        for row in imp.summary_rows.borrow_mut().drain(..) {
            imp.summary.remove(&row);
        }
        let Some(counts) = self.counts() else {
            imp.summary.set_visible(false);
            imp.title.set_subtitle("Nothing to look at yet");
            imp.traffic.set_label("");
            imp.apply_button.set_sensitive(false);
            return;
        };
        imp.title.set_subtitle(&match counts.written {
            0 => format!("{} of {} photos selected", counts.selected, counts.photos),
            written => format!("{written} of {} photos changed", counts.photos),
        });

        // What is zero is left out: a summary of six rows that all say nothing is no summary.
        let mut rows = vec![("Photos", counts.photos), ("Would change", counts.change)];
        for (title, count) in [
            ("Written", counts.written),
            ("Failed", counts.failed),
            ("Already right", counts.nothing),
            ("Refused", counts.refused),
        ] {
            if count > 0 {
                rows.push((title, count));
            }
        }
        rows.push(("Selected", counts.selected));
        let mut rows: Vec<(String, String)> = rows
            .into_iter()
            .map(|(title, count)| (title.to_string(), count.to_string()))
            .collect();
        rows.push(("Estimated traffic".to_string(), size(counts.traffic)));

        for (title, value) in rows {
            let row = adw::ActionRow::builder().title(&title).build();
            let label = gtk::Label::builder().label(&value).build();
            label.add_css_class("dim-label");
            row.add_suffix(&label);
            imp.summary.add(&row);
            imp.summary_rows.borrow_mut().push(row);
        }
        imp.summary.set_visible(true);

        imp.traffic
            .set_label(&format!("{} would be uploaded again", size(counts.traffic)));
        imp.apply_button.set_sensitive(counts.selected > 0);
    }

    fn show_detail(&self, index: usize, exact: &Exact) {
        let imp = self.imp();
        for row in imp.detail_rows.borrow_mut().drain(..) {
            imp.detail.remove(&row);
        }
        let photo = self
            .store()
            .item(index as u32)
            .and_downcast::<Item>()
            .map(|item| item.photo())
            .unwrap_or_default();
        imp.detail.set_title(&format!("The exact change to {photo}"));

        let rows = match exact {
            Err(why) => vec![adw::ActionRow::builder().title("Refused").subtitle(why).build()],
            Ok(assignments) if assignments.is_empty() => {
                vec![
                    adw::ActionRow::builder()
                        .title("Nothing")
                        .subtitle("the photo already says all of it")
                        .build(),
                ]
            }
            Ok(assignments) => assignments
                .iter()
                .map(|one| adw::ActionRow::builder().title(&one.tag).subtitle(one.tells()).build())
                .collect(),
        };
        for row in rows {
            row.set_subtitle_lines(0);
            imp.detail.add(&row);
            imp.detail_rows.borrow_mut().push(row);
        }
        imp.detail.set_visible(true);
    }

    fn hide_detail(&self) {
        let imp = self.imp();
        for row in imp.detail_rows.borrow_mut().drain(..) {
            imp.detail.remove(&row);
        }
        imp.detail.set_visible(false);
    }

    fn store(&self) -> &gio::ListStore {
        self.imp().store.get_or_init(gio::ListStore::new::<Item>)
    }

    fn build_table(&self) {
        let table = &self.imp().table;
        table.set_model(Some(&gtk::NoSelection::new(Some(self.store().clone()))));

        // The handler belongs to the widget, not to the row it currently shows: a column view
        // recycles its widgets, so connecting on every bind would pile handlers up.
        let picking = gtk::SignalListItemFactory::new();
        picking.connect_setup(glib::clone!(
            #[weak(rename_to = preview)]
            self,
            move |_, item| {
                let item = listed(item);
                let check = gtk::CheckButton::new();
                check.set_halign(gtk::Align::Center);
                check.update_property(&[gtk::accessible::Property::Label("Change this photo")]);
                check.connect_toggled(glib::clone!(
                    #[weak]
                    preview,
                    #[weak]
                    item,
                    move |check| {
                        if let Some(row) = item.item().and_downcast::<Item>() {
                            preview.select(row.index() as usize, check.is_active());
                        }
                    }
                ));
                item.set_child(Some(&check));
            }
        ));
        picking.connect_bind(glib::clone!(
            #[weak(rename_to = preview)]
            self,
            move |_, item| {
                let item = listed(item);
                let (Some(row), Some(check)) = (
                    item.item().and_downcast::<Item>(),
                    item.child().and_downcast::<gtk::CheckButton>(),
                ) else {
                    return;
                };
                preview.imp().binding.set(true);
                check.set_active(row.selected());
                check.set_sensitive(row.selectable());
                preview.imp().binding.set(false);
            }
        ));
        self.add_column("", &picking, false);

        self.add_text_column("Photo", true, Item::photo);
        self.add_text_column("Change", true, Item::change);
        self.add_text_column("State", false, Item::state);

        let asking = gtk::SignalListItemFactory::new();
        asking.connect_setup(|_, item| {
            let button = gtk::Button::builder()
                .icon_name("view-more-symbolic")
                .tooltip_text("Show the exact change")
                .action_name("win.preview-details")
                .build();
            button.add_css_class("flat");
            button.update_property(&[gtk::accessible::Property::Label("Show the exact change")]);
            listed(item).set_child(Some(&button));
        });
        asking.connect_bind(|_, item| {
            let item = listed(item);
            let (Some(row), Some(button)) = (
                item.item().and_downcast::<Item>(),
                item.child().and_downcast::<gtk::Button>(),
            ) else {
                return;
            };
            button.set_action_target_value(Some(&(row.index() as i32).to_variant()));
        });
        self.add_column("", &asking, false);
    }

    fn add_text_column(&self, title: &str, expand: bool, what: fn(&Item) -> String) {
        let factory = gtk::SignalListItemFactory::new();
        factory.connect_setup(|_, item| {
            let label = gtk::Label::builder()
                .xalign(0.0)
                .ellipsize(pango::EllipsizeMode::Middle)
                .build();
            listed(item).set_child(Some(&label));
        });
        factory.connect_bind(move |_, item| {
            let item = listed(item);
            let (Some(row), Some(label)) = (
                item.item().and_downcast::<Item>(),
                item.child().and_downcast::<gtk::Label>(),
            ) else {
                return;
            };
            let text = what(&row);
            label.set_label(&text);
            label.set_tooltip_text(Some(&text));
        });
        self.add_column(title, &factory, expand);
    }

    fn add_column(&self, title: &str, factory: &gtk::SignalListItemFactory, expand: bool) {
        let column = gtk::ColumnViewColumn::new(Some(title), Some(factory.clone()));
        column.set_expand(expand);
        column.set_resizable(expand);
        self.imp().table.append_column(&column);
    }
}

fn listed(item: &glib::Object) -> gtk::ListItem {
    item.clone().downcast::<gtk::ListItem>().expect("a list item")
}

/// What a finished pass says in a toast.
fn told(kind: Kind, summary: &Summary) -> String {
    let what = match kind {
        Kind::Write => "changed",
        Kind::Undo => "put back",
    };
    let mut parts = vec![format!("{} photos {what}", summary.written)];
    for (count, name) in [
        (summary.skipped, "already right"),
        (summary.refused, "refused"),
        (summary.failed, "failed"),
    ] {
        if count > 0 {
            parts.push(format!("{count} {name}"));
        }
    }
    if summary.cancelled {
        parts.push("stopped early".to_string());
    }
    parts.join(", ")
}

/// Nextcloud uploads the whole file again for every edit, so these are whole files.
pub(crate) fn size(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["bytes", "kB", "MB", "GB"];
    let mut size = bytes as f64;
    let mut unit = 0;
    while size >= 1000.0 && unit + 1 < UNITS.len() {
        size /= 1000.0;
        unit += 1;
    }
    match unit {
        0 => format!("{bytes} {}", UNITS[0]),
        _ => format!("{size:.1} {}", UNITS[unit]),
    }
}
