//! One photo, looked at closely: the picture as large as the window allows and everything the
//! cache knows about it. The page walks the gallery's own list, so the arrow keys step through
//! exactly the photos of the filter, in the order the grid shows them.
//!
//! The thumbnail is shown at once, from the gallery's loader, then glycin decodes the file in its
//! sandbox and the full size replaces it. The photos on either side are decoded ahead, anything
//! further away is dropped: a full-size texture is large.
//!
//! Editing is a change set of one photo: the form's change, the engine's exact diff in a dialog,
//! the backup question before the first write of all, the write, the journal and the undo, the
//! same way as for ten thousand photos. An unfinished edit is never dropped without asking.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;

use adw::prelude::*;
use adw::subclass::prelude::*;
use gtk::glib::subclass::InitializingObject;
use gtk::{gdk, gio, glib};

use photomanager_core::changeset::{ChangeSet, Verdict, Wanted};
use photomanager_core::details::Details;
use photomanager_core::filter::Listed;
use photomanager_core::journal::Kind;
use photomanager_core::write::{Change, Engine, Field, Outcome, Summary};

use crate::edit::Form;
use crate::gallery::Photo;
use crate::library::{Event, Library};
use crate::panel::{self, Look, Panel};
use crate::thumbnails::{Loader, Request, Slot};

/// The full size of one photo: on its way, there, or not to be had.
#[derive(Debug, Clone)]
pub enum Full {
    Reading(gio::Cancellable),
    Ready(gdk::Texture),
    Failed(String),
}

/// A change built and diffed, waiting for Apply.
#[derive(Debug)]
pub struct Review {
    set: ChangeSet,
    lines: Vec<String>,
    dialog: adw::AlertDialog,
}

mod imp {
    use super::*;

    #[derive(Debug, Default, gtk::CompositeTemplate)]
    #[template(resource = "/org/beijingcode/PhotoManager/photo.ui")]
    pub struct PhotoPage {
        #[template_child]
        pub toasts: TemplateChild<adw::ToastOverlay>,
        #[template_child]
        pub name: TemplateChild<gtk::Label>,
        #[template_child]
        pub position: TemplateChild<gtk::Label>,
        #[template_child]
        pub edit_button: TemplateChild<gtk::ToggleButton>,
        #[template_child]
        pub banner: TemplateChild<adw::Banner>,
        #[template_child]
        pub split: TemplateChild<adw::OverlaySplitView>,
        #[template_child]
        pub stage: TemplateChild<gtk::Overlay>,
        #[template_child]
        pub picture: TemplateChild<gtk::Picture>,
        #[template_child]
        pub previous_button: TemplateChild<gtk::Button>,
        #[template_child]
        pub next_button: TemplateChild<gtk::Button>,
        #[template_child]
        pub loading: TemplateChild<adw::Spinner>,
        #[template_child]
        pub panel: TemplateChild<gtk::Box>,

        pub library: RefCell<Option<Rc<Library>>>,
        pub thumbs: RefCell<Option<Loader<gdk::Texture>>>,
        /// The gallery's list, in the grid's order.
        pub list: RefCell<Option<gio::ListStore>>,
        pub at: Cell<u32>,
        pub shown: RefCell<Option<Listed>>,
        pub full: RefCell<HashMap<String, Full>>,
        pub slot: Slot,
        pub details: RefCell<Option<Details>>,
        pub sheet: RefCell<Panel>,
        pub form: RefCell<Option<Rc<Form>>>,
        /// What the form adds up to: the change, or why it is not one.
        pub pending: RefCell<Option<Result<Change, String>>>,
        pub review: RefCell<Option<Review>>,
        pub engine: RefCell<Option<Engine>>,
        pub applied: RefCell<Option<(Kind, Summary)>>,
        /// When the photo on screen was asked for, and how long its two pictures took.
        pub asked_at: Cell<Option<std::time::Instant>>,
        pub thumb_took: Cell<Option<std::time::Duration>>,
        pub full_took: Cell<Option<std::time::Duration>>,
        /// Which answer about the details is the newest asked for.
        pub asked: Cell<u64>,
        pub toast: RefCell<String>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for PhotoPage {
        const NAME: &'static str = "PmPhotoPage";
        type Type = super::PhotoPage;
        type ParentType = adw::NavigationPage;

        fn class_init(klass: &mut Self::Class) {
            klass.bind_template();
        }

        fn instance_init(obj: &InitializingObject<Self>) {
            obj.init_template();
        }
    }

    impl ObjectImpl for PhotoPage {
        fn constructed(&self) {
            self.parent_constructed();
            self.obj().build_keys();
        }
    }

    impl WidgetImpl for PhotoPage {}
    impl NavigationPageImpl for PhotoPage {}
}

glib::wrapper! {
    pub struct PhotoPage(ObjectSubclass<imp::PhotoPage>)
        @extends adw::NavigationPage, gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl Default for PhotoPage {
    fn default() -> Self {
        glib::Object::builder().build()
    }
}

impl PhotoPage {
    pub fn set_library(&self, library: Option<Rc<Library>>, thumbs: Option<Loader<gdk::Texture>>) {
        if let Some(library) = &library {
            let page = self.downgrade();
            library.connect_changed(move || {
                if let Some(page) = page.upgrade() {
                    page.reread();
                }
            });
        }
        *self.imp().library.borrow_mut() = library;
        *self.imp().thumbs.borrow_mut() = thumbs;
    }

    /// Walks this list from now on. A list that changes underneath keeps the photo on screen
    /// where it can.
    pub fn set_list(&self, list: &gio::ListStore) {
        if self.imp().list.borrow().is_some() {
            return;
        }
        list.connect_items_changed(glib::clone!(
            #[weak(rename_to = page)]
            self,
            move |_, _, _, _| page.relocate()
        ));
        *self.imp().list.borrow_mut() = Some(list.clone());
    }

    /// Shows the photo at this position of the list.
    pub fn open(&self, at: u32) {
        self.show(at);
        self.imp().stage.grab_focus();
    }

    /// Lets go of every picture: the page is closed.
    pub fn close_down(&self) {
        let imp = self.imp();
        for (_, full) in imp.full.borrow_mut().drain() {
            if let Full::Reading(cancellable) = full {
                cancellable.cancel();
            }
        }
        if let Some(thumbs) = imp.thumbs.borrow().as_ref() {
            thumbs.release(&imp.slot);
        }
        imp.picture.set_paintable(None::<&gdk::Paintable>);
        *imp.shown.borrow_mut() = None;
        *imp.details.borrow_mut() = None;
        imp.sheet.borrow_mut().clear(&imp.panel);
    }

    /// Steps through the list; past either end it stays where it is.
    pub fn step(&self, by: i64) {
        let count = i64::from(self.count());
        if count == 0 {
            return;
        }
        let to = (i64::from(self.imp().at.get()) + by).clamp(0, count - 1) as u32;
        if to != self.imp().at.get() {
            self.leave(move |page| page.show(to));
        }
    }

    pub fn first(&self) {
        if self.count() > 0 {
            self.leave(|page| page.show(0));
        }
    }

    pub fn last(&self) {
        if self.count() > 0 {
            self.leave(|page| page.show(page.count() - 1));
        }
    }

    /// Goes on with `then` once no unfinished edit is left: at once when nothing was changed,
    /// after asking when something was.
    pub fn leave(&self, then: impl FnOnce(&PhotoPage) + 'static) {
        if !self.has_changes() {
            self.stop_editing();
            then(self);
            return;
        }
        let dialog = adw::AlertDialog::new(
            Some("Discard the Change?"),
            Some("What was changed in the form has not been written, and would be lost."),
        );
        dialog.add_responses(&[("keep", "Keep Editing"), ("discard", "Discard")]);
        dialog.set_response_appearance("discard", adw::ResponseAppearance::Destructive);
        dialog.set_default_response(Some("keep"));
        dialog.set_close_response("keep");
        let then = RefCell::new(Some(then));
        dialog.connect_response(
            None,
            glib::clone!(
                #[weak(rename_to = page)]
                self,
                move |_: &adw::AlertDialog, response: &str| {
                    if response != "discard" {
                        return;
                    }
                    tracing::info!(photo = page.path(), "an unfinished edit was discarded");
                    page.stop_editing();
                    if let Some(then) = then.borrow_mut().take() {
                        then(&page);
                    }
                }
            ),
        );
        dialog.present(Some(self));
    }

    pub fn editing(&self) -> bool {
        self.imp().form.borrow().is_some()
    }

    /// Whether the form says anything the photo does not.
    pub fn has_changes(&self) -> bool {
        let imp = self.imp();
        match (imp.form.borrow().as_ref(), imp.details.borrow().as_ref()) {
            (Some(form), Some(details)) => form.edited() != details.edited(),
            _ => false,
        }
    }

    /// Starts editing, or stops: asking first when the form holds a change.
    pub fn toggle_editing(&self) {
        match self.editing() {
            true => self.leave(|_| {}),
            false => self.start_editing(),
        }
        self.show_editing();
    }

    fn start_editing(&self) {
        let imp = self.imp();
        let (Some(library), Some(details)) = (imp.library.borrow().clone(), imp.details.borrow().clone()) else {
            return;
        };
        let page = self.downgrade();
        let form = Form::new(&details, library.clone(), move || {
            if let Some(page) = page.upgrade() {
                page.form_changed();
            }
        });
        let known = Rc::downgrade(&form);
        library.sidebars(move |read| {
            if let (Some(form), Ok(sidebars)) = (known.upgrade(), read) {
                form.set_known_tags(tag_paths(&sidebars.tags.tree()));
            }
        });
        *imp.form.borrow_mut() = Some(form);
        tracing::info!(photo = self.path(), "editing started");
        self.fill_panel();
        self.form_changed();
    }

    fn stop_editing(&self) {
        let imp = self.imp();
        if imp.form.borrow_mut().take().is_none() {
            return;
        }
        *imp.pending.borrow_mut() = None;
        self.enable("photo-review", false);
        self.fill_panel();
        self.show_editing();
    }

    fn show_editing(&self) {
        let editing = self.editing();
        self.set_can_pop(!editing);
        if let Some(action) = self.action("photo-edit") {
            action.set_state(&editing.to_variant());
        }
    }

    fn form_changed(&self) {
        let imp = self.imp();
        let pending = match (imp.form.borrow().as_ref(), imp.details.borrow().as_ref()) {
            (Some(form), Some(details)) => Some(details.change_to(&form.edited())),
            _ => None,
        };
        if let Some(form) = imp.form.borrow().as_ref() {
            form.show_problem(
                pending
                    .as_ref()
                    .and_then(|pending| pending.as_ref().err())
                    .map(String::as_str),
            );
        }
        let ready = matches!(&pending, Some(Ok(change)) if !change.is_empty());
        *imp.pending.borrow_mut() = pending;
        self.enable("photo-review", ready);
    }

    /// Types into a field of the form by its title.
    pub fn set_form(&self, title: &str, text: &str) -> bool {
        self.imp()
            .form
            .borrow()
            .as_ref()
            .is_some_and(|form| form.set_text(title, text))
    }

    pub fn form_add_tag(&self, tag: &str) {
        let form = self.imp().form.borrow().clone();
        if let Some(form) = form {
            form.add_tag(tag);
        }
    }

    pub fn form_remove_tag(&self, tag: &str) {
        let form = self.imp().form.borrow().clone();
        if let Some(form) = form {
            form.remove_tag(tag);
        }
    }

    pub fn form_rating(&self, rating: Option<i64>) {
        if let Some(form) = self.imp().form.borrow().as_ref() {
            form.set_rating(rating);
        }
    }

    /// The fields the form would change, or why it cannot.
    pub fn pending(&self) -> Option<Result<Vec<&'static str>, String>> {
        self.imp().pending.borrow().as_ref().map(|pending| {
            pending
                .as_ref()
                .map(|change| change.fields.iter().map(field_name).collect())
                .map_err(Clone::clone)
        })
    }

    pub fn can_review(&self) -> bool {
        self.action("photo-review").is_some_and(|action| action.is_enabled())
    }

    /// What the review dialog lists: one line per tag the engine would set.
    pub fn review_lines(&self) -> Option<Vec<String>> {
        self.imp().review.borrow().as_ref().map(|review| review.lines.clone())
    }

    pub fn applied(&self) -> Option<(Kind, Summary)> {
        self.imp().applied.borrow().clone()
    }

    /// Builds the change set of this one photo and shows the engine's exact diff of it.
    pub fn review(&self) {
        let imp = self.imp();
        let (Some(library), Some(path)) = (imp.library.borrow().clone(), self.path()) else {
            return;
        };
        let Some(Ok(change)) = imp.pending.borrow().clone() else {
            return;
        };
        if change.is_empty() {
            return;
        }
        if library.is_busy() {
            self.say("Something else is running. Try again when it is done.", false);
            return;
        }
        let title = format!("Edit {}", self.title());
        let page = self.downgrade();
        library.preview(&title, vec![Wanted::new(path, change)], move |event| {
            let Some(page) = page.upgrade() else {
                return;
            };
            match event {
                Event::Previewed(set) => page.reviewed(set),
                Event::Failed(why) => page.say(&format!("Did not work: {why}"), false),
                _ => {}
            }
        });
    }

    fn reviewed(&self, set: ChangeSet) {
        let imp = self.imp();
        let Some(row) = set.rows.first() else {
            return;
        };
        match &row.verdict {
            Verdict::Refused(why) => return self.say(&format!("This photo cannot be changed: {why}"), false),
            Verdict::Nothing => return self.say("The photo already says all of that.", false),
            _ => {}
        }
        let Some(library) = imp.library.borrow().clone() else {
            return;
        };
        if imp.engine.borrow().is_none() {
            match library.engine() {
                Ok(engine) => *imp.engine.borrow_mut() = Some(engine),
                Err(why) => return self.say(&format!("Did not work: {why}"), false),
            }
        }
        let exact = {
            let mut engine = imp.engine.borrow_mut();
            set.exact(0, engine.as_mut().expect("started above"))
        };
        let assignments = match exact {
            Ok(assignments) => assignments,
            Err(why) => return self.say(&format!("This photo cannot be changed: {why}"), false),
        };
        if assignments.is_empty() {
            return self.say("The photo already says all of that.", false);
        }
        let lines: Vec<String> = assignments
            .iter()
            .map(|one| match &one.then {
                None => format!(
                    "{}: {} -> removed",
                    one.tag,
                    photomanager_core::write::change::shown(one.now.as_ref())
                ),
                Some(_) => format!("{}: {}", one.tag, one.tells()),
            })
            .collect();

        let dialog = adw::AlertDialog::new(
            Some("Change This Photo?"),
            Some(&format!(
                "Only these fields of {} are written, never the picture. The whole file, {}, goes up to Nextcloud again.",
                self.title(),
                crate::preview::size(row.size)
            )),
        );
        let list = gtk::ListBox::new();
        list.add_css_class("boxed-list");
        list.set_selection_mode(gtk::SelectionMode::None);
        for one in &assignments {
            let subtitle = match &one.then {
                None => format!(
                    "{} -> removed",
                    photomanager_core::write::change::shown(one.now.as_ref())
                ),
                Some(_) => one.tells(),
            };
            let row = adw::ActionRow::builder()
                .title(&one.tag)
                .subtitle(subtitle)
                .use_markup(false)
                .subtitle_lines(0)
                .build();
            row.add_css_class("property");
            list.append(&row);
        }
        let scrolled = gtk::ScrolledWindow::builder()
            .child(&list)
            .hscrollbar_policy(gtk::PolicyType::Never)
            .propagate_natural_height(true)
            .max_content_height(360)
            .build();
        dialog.set_extra_child(Some(&scrolled));
        dialog.add_responses(&[("cancel", "Cancel"), ("apply", "Apply")]);
        dialog.set_response_appearance("apply", adw::ResponseAppearance::Suggested);
        dialog.set_default_response(Some("apply"));
        dialog.set_close_response("cancel");
        dialog.connect_response(
            None,
            glib::clone!(
                #[weak(rename_to = page)]
                self,
                move |_: &adw::AlertDialog, response: &str| match response {
                    "apply" => page.apply(),
                    _ => {
                        page.imp().review.borrow_mut().take();
                        page.enable("photo-apply", false);
                    }
                }
            ),
        );
        *imp.review.borrow_mut() = Some(Review {
            set,
            lines,
            dialog: dialog.clone(),
        });
        self.enable("photo-apply", true);
        dialog.present(Some(self));
    }

    /// Closes the review without writing anything.
    pub fn cancel_review(&self) {
        let review = self.imp().review.borrow_mut().take();
        if let Some(review) = review {
            review.dialog.force_close();
        }
        self.enable("photo-apply", false);
    }

    /// Writes the reviewed change, after the backup question if nothing was ever written.
    pub fn apply(&self) {
        let imp = self.imp();
        let Some(review) = imp.review.borrow_mut().take() else {
            return;
        };
        self.enable("photo-apply", false);
        review.dialog.force_close();
        let Some(library) = imp.library.borrow().clone() else {
            return;
        };
        let set = review.set;
        let page = self.downgrade();
        let written = library.clone();
        crate::confirm::before_first_write(self, &library, move || {
            let Some(page) = page.upgrade() else {
                return;
            };
            if written.is_busy() {
                page.say("Something else is running, so nothing was written.", false);
                return;
            }
            let reported = page.downgrade();
            written.apply(&set, move |event| {
                if let Some(page) = reported.upgrade() {
                    page.report(event);
                }
            });
        });
    }

    /// Takes the last applied change back, after asking.
    pub fn undo(&self) {
        let Some(library) = self.imp().library.borrow().clone() else {
            return;
        };
        if library.is_busy() {
            return;
        }
        let Some(pass) = library.undoable() else {
            self.say("There is nothing to take back.", false);
            return;
        };
        let page = self.downgrade();
        crate::confirm::before_undo(self, &pass, move || {
            let Some(page) = page.upgrade() else {
                return;
            };
            let Some(library) = page.imp().library.borrow().clone() else {
                return;
            };
            if library.is_busy() {
                page.say("Something else is running, so nothing was taken back.", false);
                return;
            }
            let reported = page.downgrade();
            library.undo_last(move |event| {
                if let Some(page) = reported.upgrade() {
                    page.report(event);
                }
            });
        });
    }

    fn report(&self, event: Event) {
        match event {
            Event::Applied(kind, summary) => {
                let path = self.path().unwrap_or_default();
                let outcome = summary
                    .outcomes
                    .iter()
                    .find(|(rel_path, _)| *rel_path == path)
                    .map(|(_, outcome)| outcome.clone());
                let told = match (kind, outcome) {
                    (Kind::Undo, _) if summary.written == 1 => "1 photo put back".to_string(),
                    (Kind::Undo, _) => format!("{} photos put back", summary.written),
                    (Kind::Write, Some(Outcome::Written)) => format!("{} changed", self.title()),
                    (Kind::Write, Some(Outcome::Skipped)) => "The photo already says all of that.".to_string(),
                    (Kind::Write, Some(Outcome::Refused(why))) => format!("Not changed: {why}"),
                    (Kind::Write, Some(Outcome::Failed(why))) => format!("Did not work: {why}"),
                    (Kind::Write, None) => "Nothing was written.".to_string(),
                };
                let undoable = kind == Kind::Write && summary.written > 0;
                tracing::info!(kind = kind.as_str(), written = summary.written, "one photo applied");
                *self.imp().applied.borrow_mut() = Some((kind, summary));
                self.say(&told, undoable);
                if undoable {
                    self.stop_editing();
                }
                if let Some(window) = self.root().and_downcast::<crate::window::Window>() {
                    window.scan(photomanager_core::scan::Mode::Reconcile);
                }
            }
            Event::Failed(why) => {
                self.say(&format!("Did not work: {why}"), false);
                tracing::error!(why, "the edit was not applied");
            }
            _ => {}
        }
    }

    fn action(&self, name: &str) -> Option<gio::SimpleAction> {
        self.root()
            .and_downcast::<gtk::ApplicationWindow>()?
            .lookup_action(name)
            .and_downcast::<gio::SimpleAction>()
    }

    fn enable(&self, name: &str, enabled: bool) {
        if let Some(action) = self.action(name) {
            action.set_enabled(enabled);
        }
    }

    pub fn toggle_panel(&self) {
        let split = &self.imp().split;
        split.set_show_sidebar(!split.shows_sidebar());
    }

    /// Hands the file to the image viewer the system picks.
    pub fn open_with(&self) {
        let Some(file) = self.file() else {
            return;
        };
        let launcher = gtk::FileLauncher::new(Some(&gio::File::for_path(&file)));
        let window = self.root().and_downcast::<gtk::Window>();
        launcher.launch(window.as_ref(), None::<&gio::Cancellable>, move |done| {
            if let Err(error) = done {
                tracing::warn!(%error, file = %file.display(), "the photo could not be opened elsewhere");
            }
        });
    }

    /// Where in the list the photo on screen is.
    pub fn position(&self) -> u32 {
        self.imp().at.get()
    }

    pub fn count(&self) -> u32 {
        self.imp()
            .list
            .borrow()
            .as_ref()
            .map(|list| list.n_items())
            .unwrap_or(0)
    }

    pub fn path(&self) -> Option<String> {
        self.imp().shown.borrow().as_ref().map(|listed| listed.rel_path.clone())
    }

    /// The size of the full picture on screen, once it has arrived.
    pub fn full_size(&self) -> Option<(i32, i32)> {
        let path = self.path()?;
        match self.imp().full.borrow().get(&path)? {
            Full::Ready(texture) => Some((texture.width(), texture.height())),
            _ => None,
        }
    }

    /// Why the full picture on screen could not be read.
    pub fn full_failed(&self) -> Option<String> {
        let path = self.path()?;
        match self.imp().full.borrow().get(&path)? {
            Full::Failed(why) => Some(why.clone()),
            _ => None,
        }
    }

    /// Notes how long the photo on screen took, the first time it shows.
    fn took(&self, which: &Cell<Option<std::time::Duration>>) {
        if which.get().is_none()
            && let Some(asked) = self.imp().asked_at.get()
        {
            which.set(Some(asked.elapsed()));
        }
    }

    /// How long the photo on screen took to show its thumbnail and its full size, in ms.
    pub fn timings(&self) -> (Option<u128>, Option<u128>) {
        let imp = self.imp();
        (
            imp.thumb_took.get().map(|took| took.as_millis()),
            imp.full_took.get().map(|took| took.as_millis()),
        )
    }

    /// How many full pictures are kept or on their way.
    pub fn held(&self) -> usize {
        self.imp().full.borrow().len()
    }

    pub fn shows_picture(&self) -> bool {
        self.imp().picture.paintable().is_some()
    }

    pub fn shows_panel(&self) -> bool {
        self.imp().split.shows_sidebar()
    }

    pub fn details(&self) -> Option<Details> {
        self.imp().details.borrow().clone()
    }

    /// Every title and subtitle the panel shows.
    pub fn panel_texts(&self) -> Vec<String> {
        self.imp().sheet.borrow().texts()
    }

    pub fn filter_raw(&self, query: &str) {
        self.imp().sheet.borrow().filter_raw(query);
    }

    pub fn raw_shown(&self) -> usize {
        self.imp().sheet.borrow().raw_shown()
    }

    pub fn shows_map(&self) -> bool {
        self.imp().sheet.borrow().shows_map()
    }

    /// Shows the map around the photo's coordinates: the one thing here that uses the network.
    pub fn show_map(&self) {
        let Some((lat, lon)) = self.details().and_then(|details| details.gps) else {
            return;
        };
        self.imp().sheet.borrow_mut().show_map(lat, lon);
    }

    pub fn toast(&self) -> String {
        self.imp().toast.borrow().clone()
    }

    pub fn say(&self, text: &str, undoable: bool) {
        *self.imp().toast.borrow_mut() = text.to_string();
        let toast = adw::Toast::new(text);
        if undoable {
            toast.set_button_label(Some("Undo"));
            toast.set_action_name(Some("win.undo-last"));
        }
        self.imp().toasts.add_toast(toast);
    }

    fn file(&self) -> Option<std::path::PathBuf> {
        let library = self.imp().library.borrow().clone()?;
        Some(library.paths().library().join(self.path()?))
    }

    fn listed_at(&self, at: u32) -> Option<Listed> {
        let list = self.imp().list.borrow().clone()?;
        list.item(at)
            .and_downcast::<Photo>()
            .map(|photo| photo.listed().clone())
    }

    fn show(&self, at: u32) {
        let imp = self.imp();
        let Some(listed) = self.listed_at(at) else {
            return;
        };
        imp.at.set(at);
        let name = listed
            .rel_path
            .rsplit('/')
            .next()
            .unwrap_or(&listed.rel_path)
            .to_string();
        self.set_title(&name);
        imp.name.set_label(&name);
        imp.name.set_tooltip_text(Some(&listed.rel_path));
        self.show_position(listed.taken_at.as_deref());
        imp.previous_button.set_visible(at > 0);
        imp.next_button.set_visible(at + 1 < self.count());
        *imp.shown.borrow_mut() = Some(listed.clone());
        imp.asked_at.set(Some(std::time::Instant::now()));
        imp.thumb_took.set(None);
        imp.full_took.set(None);

        self.show_thumbnail(&listed);
        self.read_ahead();
        self.show_full();
        self.read_details();
    }

    fn show_position(&self, taken: Option<&str>) {
        let mut position = format!("{} of {}", self.imp().at.get() + 1, self.count());
        if let Some(taken) = taken {
            position.push_str(&format!(" - {taken}"));
        }
        self.imp().position.set_label(&position);
    }

    /// The list moved under the page: find the photo again, or show what is now in its place.
    fn relocate(&self) {
        let Some(path) = self.path() else {
            return;
        };
        let count = self.count();
        let found = (0..count).find(|at| self.listed_at(*at).is_some_and(|listed| listed.rel_path == path));
        match (found, count) {
            (_, 0) => self.close_down(),
            (Some(at), _) => {
                self.imp().at.set(at);
                self.show(at);
            }
            (None, _) => self.show(self.imp().at.get().min(count - 1)),
        }
    }

    fn show_thumbnail(&self, listed: &Listed) {
        let imp = self.imp();
        let ready = matches!(imp.full.borrow().get(&listed.rel_path), Some(Full::Ready(_)));
        let (Some(thumbs), Some(library)) = (imp.thumbs.borrow().clone(), imp.library.borrow().clone()) else {
            return;
        };
        if ready {
            thumbs.release(&imp.slot);
            return;
        }
        imp.picture.set_paintable(None::<&gdk::Paintable>);
        let request = Request {
            key: listed.content_id.clone().unwrap_or_else(|| listed.rel_path.clone()),
            content_id: listed.content_id.clone(),
            file: library.paths().library().join(&listed.rel_path),
            orientation: listed.orientation,
        };
        let page = self.downgrade();
        let path = listed.rel_path.clone();
        thumbs.load(&imp.slot, request, move |texture| {
            let Some(page) = page.upgrade() else {
                return;
            };
            let full = matches!(page.imp().full.borrow().get(&path), Some(Full::Ready(_)));
            if page.path().as_deref() == Some(path.as_str()) && !full {
                page.imp().picture.set_paintable(texture.as_ref());
                page.took(&page.imp().thumb_took);
            }
        });
    }

    /// Puts the full picture on screen when it is there, and says so when it will not be.
    fn show_full(&self) {
        let imp = self.imp();
        let Some(path) = self.path() else {
            return;
        };
        let full = imp.full.borrow().get(&path).cloned();
        imp.loading.set_visible(matches!(full, Some(Full::Reading(_))));
        match full {
            Some(Full::Ready(texture)) => {
                imp.picture.set_paintable(Some(&texture));
                imp.banner.set_revealed(false);
                self.took(&imp.full_took);
            }
            Some(Full::Failed(_)) => {
                imp.banner
                    .set_title("The full size could not be read. This is the thumbnail.");
                imp.banner.set_revealed(true);
            }
            _ => imp.banner.set_revealed(false),
        }
    }

    /// The photo on screen and one on each side; everything else is let go.
    fn read_ahead(&self) {
        let at = self.imp().at.get();
        let wanted: Vec<String> = [Some(at), at.checked_sub(1), Some(at + 1)]
            .into_iter()
            .flatten()
            .filter_map(|at| self.listed_at(at))
            .map(|listed| listed.rel_path)
            .collect();
        self.imp().full.borrow_mut().retain(|path, full| {
            let keep = wanted.contains(path);
            if let (false, Full::Reading(cancellable)) = (keep, &full) {
                cancellable.cancel();
            }
            keep
        });
        for path in wanted {
            if !self.imp().full.borrow().contains_key(&path) {
                self.decode(path);
            }
        }
    }

    fn decode(&self, path: String) {
        let Some(library) = self.imp().library.borrow().clone() else {
            return;
        };
        let file = gio::File::for_path(library.paths().library().join(&path));
        let cancellable = gio::Cancellable::new();
        self.imp()
            .full
            .borrow_mut()
            .insert(path.clone(), Full::Reading(cancellable.clone()));

        // glycin turns the picture by its orientation inside the future, so the future runs on a
        // thread of its own and only the finished frame comes back to the main loop.
        let (sender, receiver) = async_channel::bounded(1);
        let asked = cancellable.clone();
        std::thread::spawn(move || {
            let read = glib::MainContext::new().block_on(async {
                let mut loader = glycin::Loader::new(file);
                loader.cancellable(asked);
                let mut image = loader.load().await?;
                image.next_frame().await
            });
            let _ = sender.send_blocking(read.map_err(|error| error.to_string()));
        });
        let page = self.downgrade();
        glib::spawn_future_local(async move {
            let Ok(read) = receiver.recv().await else {
                return;
            };
            if let Some(page) = page.upgrade() {
                page.arrived(&path, &cancellable, read.map(|frame| frame.texture()));
            }
        });
    }

    fn arrived(&self, path: &str, cancellable: &gio::Cancellable, read: Result<gdk::Texture, String>) {
        let imp = self.imp();
        {
            let mut full = imp.full.borrow_mut();
            let asked = matches!(full.get(path), Some(Full::Reading(asked)) if asked == cancellable);
            if !asked {
                return;
            }
            let arrived = match read {
                Ok(texture) => Full::Ready(texture),
                Err(why) => {
                    tracing::warn!(photo = path, why, "the full size could not be read");
                    Full::Failed(why)
                }
            };
            full.insert(path.to_string(), arrived);
        }
        if self.path().as_deref() == Some(path) {
            self.show_full();
        }
    }

    /// The photo was read again: its details may have changed.
    fn reread(&self) {
        if self.path().is_some() {
            self.read_details();
        }
    }

    fn read_details(&self) {
        let (Some(library), Some(path)) = (self.imp().library.borrow().clone(), self.path()) else {
            return;
        };
        let asked = self.imp().asked.get() + 1;
        self.imp().asked.set(asked);
        let page = self.downgrade();
        library.details(&path, move |read| {
            let Some(page) = page.upgrade() else {
                return;
            };
            if page.imp().asked.get() != asked {
                return;
            }
            match read {
                Ok(details) => page.show_details(details),
                Err(why) => tracing::error!(why, "the details could not be read"),
            }
        });
    }

    fn show_details(&self, details: Option<Details>) {
        if let Some(details) = &details {
            self.show_position(details.taken_at.as_deref());
        }
        *self.imp().details.borrow_mut() = details;
        if self.editing() {
            self.form_changed();
        }
        self.fill_panel();
    }

    fn fill_panel(&self) {
        let imp = self.imp();
        let Some(library) = imp.library.borrow().clone() else {
            return;
        };
        let Some(details) = imp.details.borrow().clone() else {
            imp.sheet.borrow_mut().fill_unknown(&imp.panel);
            return;
        };
        let nearest = details.gps.and_then(|(lat, lon)| library.nearest(lat, lon));
        let always_map = library.setting(panel::ALWAYS_MAP).as_deref() == Some("true") && !self.editing();
        let look = Look {
            details: &details,
            nearest: nearest.as_ref(),
            has_places: library.counts().places > 0,
            always_map,
        };
        let form = imp.form.borrow().as_ref().map(|form| form.widget());
        imp.sheet.borrow_mut().fill(&imp.panel, &look, form.as_ref());
        if always_map {
            self.show_map();
        }
        let always = imp.sheet.borrow().always_switch();
        if let Some(always) = always {
            always.connect_active_notify(glib::clone!(
                #[weak(rename_to = page)]
                self,
                move |switch| page.set_always_map(switch.is_active())
            ));
        }
    }

    fn set_always_map(&self, always: bool) {
        if let Some(library) = self.imp().library.borrow().as_ref() {
            library.put_setting(panel::ALWAYS_MAP, if always { "true" } else { "false" });
        }
        tracing::info!(always, "the map setting changed");
        if always {
            self.show_map();
        }
    }

    fn build_keys(&self) {
        let keys = gtk::ShortcutController::new();
        for (trigger, action) in [
            ("Left", "win.photo-previous"),
            ("Page_Up", "win.photo-previous"),
            ("Right", "win.photo-next"),
            ("Page_Down", "win.photo-next"),
            ("Home", "win.photo-first"),
            ("End", "win.photo-last"),
            ("Escape", "win.photo-close"),
            ("F9", "win.photo-panel"),
        ] {
            keys.add_shortcut(gtk::Shortcut::new(
                gtk::ShortcutTrigger::parse_string(trigger),
                Some(gtk::NamedAction::new(action)),
            ));
        }
        self.add_controller(keys);
    }
}

/// Every tag of the tree, parents before their children.
fn tag_paths(tags: &[photomanager_core::browse::Tag]) -> Vec<String> {
    let mut all = Vec::new();
    for tag in tags {
        all.push(tag.path.clone());
        all.extend(tag_paths(&tag.children));
    }
    all
}

fn field_name(field: &Field) -> &'static str {
    match field {
        Field::Tags(_) => "tags",
        Field::Rating(_) => "rating",
        Field::Gps(_) => "location",
        Field::Place(_) => "place",
        Field::Taken(_) => "date",
        Field::Faces(_) => "people",
        Field::DropLabel => "label",
        Field::DropCatalogSets => "catalog sets",
    }
}
