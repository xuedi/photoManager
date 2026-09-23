//! One photo, looked at closely: the picture as large as the window allows and everything the
//! cache knows about it. The page walks the gallery's own list, so the arrow keys step through
//! exactly the photos of the filter, in the order the grid shows them.
//!
//! The thumbnail is shown at once, from the gallery's loader, then glycin decodes the file in its
//! sandbox and the full size replaces it. The photos on either side are decoded ahead, anything
//! further away is dropped: a full-size texture is large.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;

use adw::prelude::*;
use adw::subclass::prelude::*;
use gtk::glib::subclass::InitializingObject;
use gtk::{gdk, gio, glib};

use photomanager_core::details::Details;
use photomanager_core::filter::Listed;

use crate::gallery::Photo;
use crate::library::Library;
use crate::panel::{self, Look, Panel};
use crate::thumbnails::{Loader, Request, Slot};

/// The full size of one photo: on its way, there, or not to be had.
#[derive(Debug, Clone)]
pub enum Full {
    Reading(gio::Cancellable),
    Ready(gdk::Texture),
    Failed(String),
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
        let to = (i64::from(self.imp().at.get()) + by).clamp(0, count - 1);
        if to as u32 != self.imp().at.get() {
            self.show(to as u32);
        }
    }

    pub fn first(&self) {
        if self.count() > 0 {
            self.show(0);
        }
    }

    pub fn last(&self) {
        if self.count() > 0 {
            self.show(self.count() - 1);
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
        let mut position = format!("{} of {}", at + 1, self.count());
        if let Some(taken) = &listed.taken_at {
            position.push_str(&format!(" - {taken}"));
        }
        imp.position.set_label(&position);
        imp.previous_button.set_visible(at > 0);
        imp.next_button.set_visible(at + 1 < self.count());
        *imp.shown.borrow_mut() = Some(listed.clone());

        self.show_thumbnail(&listed);
        self.read_ahead();
        self.show_full();
        self.read_details();
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

        let page = self.downgrade();
        glib::spawn_future_local(async move {
            let mut loader = glycin::Loader::new(file);
            loader.cancellable(cancellable.clone());
            let read = async {
                let mut image = loader.load().await?;
                let frame = image.next_frame().await?;
                Ok::<gdk::Texture, glycin::Error>(frame.texture())
            }
            .await;
            if let Some(page) = page.upgrade() {
                page.arrived(&path, &cancellable, read.map_err(|error| error.to_string()));
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
        *self.imp().details.borrow_mut() = details;
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
        let always_map = library.setting(panel::ALWAYS_MAP).as_deref() == Some("true");
        let look = Look {
            details: &details,
            nearest: nearest.as_ref(),
            has_places: library.counts().places > 0,
            always_map,
        };
        imp.sheet.borrow_mut().fill(&imp.panel, &look, None);
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
