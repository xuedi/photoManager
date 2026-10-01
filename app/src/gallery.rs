//! Finding the photos a tool should work on. The page shows exactly `Filter::photos` of one
//! filter, and each control owns one part of it: the place sidebar the folder, the tag sidebar
//! the tags, the people sidebar the people, the dropdown the gap. Several tags or people are
//! asked for together, and a sidebar lists only the entries that would still show photos. Every part, and whatever else the
//! dashboard handed over, is also a chip above the grid, so what narrows the grid is in sight
//! whichever sidebar is open or when none is. Every change ends in `show`, so a click here and a
//! click on the dashboard end up in the same place.
//!
//! The grid is a `GtkGridView` over a list store of small cell objects, bound by hand in the
//! factory. A cell asks for its picture when it is bound and lets go of it when it is unbound.
//!
//! Enter or a double-click opens a photo on a page pushed over the grid; Back returns to the grid
//! at that photo, with the filter, the order and the selection as they were.

use std::cell::{Cell, OnceCell, RefCell};
use std::rc::Rc;

use adw::prelude::*;
use adw::subclass::prelude::*;
use gtk::glib::subclass::InitializingObject;
use gtk::{gdk, gio, glib, pango};

use photomanager_core::browse::{Following, Person, Place, Tag};
use photomanager_core::filter::{Filter, Gap, Kind, Listed, Order};
use photomanager_core::scope::Scope;

use crate::library::{Library, Sidebars};
use crate::photo::PhotoPage;
use crate::thumbnails::{self, Loader, Request, Slot};

const ORDERS: [Order; 2] = [Order::Date, Order::Name];

/// A sidebar, and the part of the filter it owns.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Side {
    Places,
    Tags,
    People,
}

impl Side {
    const ALL: [Side; 3] = [Side::Places, Side::Tags, Side::People];

    fn name(self) -> &'static str {
        match self {
            Side::Places => "places",
            Side::Tags => "tags",
            Side::People => "people",
        }
    }

    fn title(self) -> &'static str {
        match self {
            Side::Places => "Places",
            Side::Tags => "Tags",
            Side::People => "People",
        }
    }

    /// The rows of this sidebar the filter has chosen, by their keys, in the order chosen.
    fn chosen(self, filter: &Filter) -> Vec<String> {
        let single = |groups: Vec<&[String]>| -> Vec<String> {
            groups
                .into_iter()
                .filter_map(|group| match group {
                    [one] => Some(one.clone()),
                    _ => None,
                })
                .collect()
        };
        match self {
            Side::Places => filter.within.clone().into_iter().collect(),
            Side::Tags => single(filter.tags()),
            Side::People => single(filter.persons()),
        }
    }

    /// How many photos an entry would show with the other parts, when that is known.
    fn counted(self, following: &Following, key: &str) -> i64 {
        let counts = match self {
            Side::Places => &following.places,
            Side::Tags => &following.tags,
            Side::People => &following.people,
        };
        counts.get(key).copied().unwrap_or(0)
    }

    /// Whether any part of the filter is this sidebar's to show.
    fn narrows(self, filter: &Filter) -> bool {
        match self {
            Side::Places => filter.within.is_some(),
            Side::Tags => !filter.tags().is_empty(),
            Side::People => !filter.persons().is_empty(),
        }
    }
}

mod node {
    use super::*;

    mod imp {
        use super::*;

        #[derive(Debug, Default)]
        pub struct Node {
            pub name: RefCell<String>,
            /// The folder of a place, the path of a tag.
            pub key: RefCell<String>,
            pub photos: Cell<i64>,
            pub children: RefCell<Vec<super::Node>>,
        }

        #[glib::object_subclass]
        impl ObjectSubclass for Node {
            const NAME: &'static str = "PmGalleryNode";
            type Type = super::Node;
        }

        impl ObjectImpl for Node {}
    }

    glib::wrapper! {
        /// A row of a sidebar tree: a country, an event or a tag.
        pub struct Node(ObjectSubclass<imp::Node>);
    }

    impl Node {
        fn new(name: &str, key: &str, photos: i64, children: Vec<Node>) -> Node {
            let node: Node = glib::Object::builder().build();
            *node.imp().name.borrow_mut() = name.to_string();
            *node.imp().key.borrow_mut() = key.to_string();
            node.imp().photos.set(photos);
            *node.imp().children.borrow_mut() = children;
            node
        }

        pub fn of_place(place: &Place) -> Node {
            let events = place.events.iter().map(Node::of_place).collect();
            Node::new(&place.name, &place.folder, place.photos, events)
        }

        pub fn of_tag(tag: &Tag) -> Node {
            let children = tag.children.iter().map(Node::of_tag).collect();
            Node::new(&tag.name, &tag.path, tag.photos, children)
        }

        pub fn of_person(person: &Person) -> Node {
            Node::new(&person.name, &person.name, person.photos, Vec::new())
        }

        pub fn name(&self) -> String {
            self.imp().name.borrow().clone()
        }

        pub fn key(&self) -> String {
            self.imp().key.borrow().clone()
        }

        pub fn photos(&self) -> i64 {
            self.imp().photos.get()
        }

        pub fn children(&self) -> Option<gio::ListStore> {
            let children = self.imp().children.borrow();
            if children.is_empty() {
                return None;
            }
            let store = gio::ListStore::new::<Node>();
            store.extend_from_slice(&children);
            Some(store)
        }
    }
}

mod photo {
    use super::*;

    mod imp {
        use super::*;

        #[derive(Debug, Default)]
        pub struct Photo {
            pub listed: OnceCell<Listed>,
        }

        #[glib::object_subclass]
        impl ObjectSubclass for Photo {
            const NAME: &'static str = "PmGalleryPhoto";
            type Type = super::Photo;
        }

        impl ObjectImpl for Photo {}
    }

    glib::wrapper! {
        /// One cell's worth of a photo: only what the cell draws.
        pub struct Photo(ObjectSubclass<imp::Photo>);
    }

    impl Photo {
        pub fn of(listed: Listed) -> Photo {
            let photo: Photo = glib::Object::builder().build();
            let _ = photo.imp().listed.set(listed);
            photo
        }

        pub fn listed(&self) -> &Listed {
            self.imp().listed.get().expect("set when made")
        }
    }
}

mod thumb {
    use super::*;

    mod imp {
        use super::*;

        #[derive(Debug, Default)]
        pub struct Thumb {
            pub picture: gtk::Picture,
            pub icon: gtk::Image,
            pub slot: Slot,
        }

        #[glib::object_subclass]
        impl ObjectSubclass for Thumb {
            const NAME: &'static str = "PmGalleryThumb";
            type Type = super::Thumb;
            type ParentType = adw::Bin;
        }

        impl ObjectImpl for Thumb {
            fn constructed(&self) {
                self.parent_constructed();
                self.picture.set_content_fit(gtk::ContentFit::Cover);
                self.picture.set_can_shrink(true);
                self.icon.set_pixel_size(48);
                self.icon.set_halign(gtk::Align::Center);
                self.icon.set_valign(gtk::Align::Center);
                self.icon.add_css_class("dim-label");
                let overlay = gtk::Overlay::new();
                overlay.set_child(Some(&self.picture));
                overlay.add_overlay(&self.icon);
                overlay.set_overflow(gtk::Overflow::Hidden);
                overlay.add_css_class("card");
                let obj = self.obj();
                obj.set_child(Some(&overlay));
                obj.set_size_request(160, 160);
                obj.set_margin_top(3);
                obj.set_margin_bottom(3);
                obj.set_margin_start(3);
                obj.set_margin_end(3);
            }
        }

        impl WidgetImpl for Thumb {}
        impl BinImpl for Thumb {}
    }

    glib::wrapper! {
        /// A grid cell: the picture, or an icon until there is one.
        pub struct Thumb(ObjectSubclass<imp::Thumb>)
            @extends adw::Bin, gtk::Widget,
            @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
    }

    impl Default for Thumb {
        fn default() -> Self {
            glib::Object::builder().build()
        }
    }

    impl Thumb {
        pub fn bind(&self, listed: &Listed, request: Option<Request>, loader: &Loader<gdk::Texture>) {
            let imp = self.imp();
            let name = listed.rel_path.rsplit('/').next().unwrap_or(&listed.rel_path);
            self.set_tooltip_text(Some(&listed.rel_path));
            self.update_property(&[gtk::accessible::Property::Label(name)]);
            self.show(None, listed.is_photo);
            let Some(request) = request else {
                loader.release(&imp.slot);
                return;
            };
            let thumb = self.downgrade();
            loader.load(&imp.slot, request, move |texture| {
                if let Some(thumb) = thumb.upgrade() {
                    thumb.show(texture, true);
                }
            });
        }

        pub fn unbind(&self, loader: &Loader<gdk::Texture>) {
            loader.release(&self.imp().slot);
            self.show(None, true);
        }

        pub fn has_picture(&self) -> bool {
            self.imp().picture.paintable().is_some()
        }

        fn show(&self, texture: Option<gdk::Texture>, is_photo: bool) {
            let imp = self.imp();
            imp.icon.set_visible(texture.is_none());
            imp.icon.set_icon_name(Some(match is_photo {
                true => "image-x-generic-symbolic",
                false => "text-x-generic-symbolic",
            }));
            imp.picture.set_paintable(texture.as_ref());
        }
    }
}

use node::Node;
pub(crate) use photo::Photo;
use thumb::Thumb;

mod imp {
    use super::*;

    #[derive(Debug, Default, gtk::CompositeTemplate)]
    #[template(resource = "/org/beijingcode/PhotoManager/gallery.ui")]
    pub struct Gallery {
        #[template_child]
        pub toasts: TemplateChild<adw::ToastOverlay>,
        #[template_child]
        pub nav: TemplateChild<adw::NavigationView>,
        #[template_child]
        pub photo: TemplateChild<PhotoPage>,
        #[template_child]
        pub split: TemplateChild<adw::OverlaySplitView>,
        #[template_child]
        pub browse_by: TemplateChild<adw::ToggleGroup>,
        #[template_child]
        pub sidebars: TemplateChild<gtk::Stack>,
        #[template_child]
        pub places_list: TemplateChild<gtk::ListView>,
        #[template_child]
        pub tags_list: TemplateChild<gtk::ListView>,
        #[template_child]
        pub people_search: TemplateChild<gtk::SearchEntry>,
        #[template_child]
        pub people_list: TemplateChild<gtk::ListView>,
        #[template_child]
        pub people_empty: TemplateChild<adw::StatusPage>,
        #[template_child]
        pub title: TemplateChild<gtk::Label>,
        #[template_child]
        pub count: TemplateChild<gtk::Label>,
        #[template_child]
        pub chips: TemplateChild<adw::WrapBox>,
        #[template_child]
        pub gap: TemplateChild<gtk::DropDown>,
        #[template_child]
        pub sort: TemplateChild<gtk::DropDown>,
        #[template_child]
        pub pages: TemplateChild<gtk::Stack>,
        #[template_child]
        pub grid: TemplateChild<gtk::GridView>,
        #[template_child]
        pub files: TemplateChild<gtk::ListView>,
        #[template_child]
        pub empty: TemplateChild<adw::StatusPage>,
        #[template_child]
        pub selection_bar: TemplateChild<gtk::ActionBar>,
        #[template_child]
        pub selected: TemplateChild<gtk::Label>,

        pub library: RefCell<Option<Rc<Library>>>,
        pub loader: RefCell<Option<Loader<gdk::Texture>>>,
        pub filter: RefCell<Option<Filter>>,
        pub count_of: Cell<Option<i64>>,
        pub order: Cell<Order>,
        pub photos: OnceCell<gio::ListStore>,
        pub selection: OnceCell<gtk::MultiSelection>,
        pub file_paths: OnceCell<gtk::StringList>,
        pub places: OnceCell<gio::ListStore>,
        pub tags: OnceCell<gio::ListStore>,
        pub people: OnceCell<gio::ListStore>,
        pub people_found: OnceCell<gtk::CustomFilter>,
        /// Each sidebar's filter of the entries that would still show photos.
        pub reachable: RefCell<Vec<gtk::CustomFilter>>,
        /// The chosen people first, then the most photos.
        pub people_order: OnceCell<gtk::CustomSorter>,
        pub place_tree: OnceCell<gtk::TreeListModel>,
        pub tag_tree: OnceCell<gtk::TreeListModel>,
        /// The dot on each sidebar's tab that says it narrows the grid.
        pub dots: RefCell<Vec<(Side, gtk::Image)>>,
        /// Every sidebar row made, so its mark and count can follow the filter.
        pub rows: RefCell<Vec<(glib::WeakRef<gtk::ListItem>, Side)>>,
        /// What each entry would show with the other parts of the filter on screen.
        pub following: RefCell<Option<Following>>,
        /// The last recount asked for; an older one that comes back later is dropped.
        pub recount: Cell<u64>,
        pub recounting: Cell<bool>,
        /// The library version the sidebars were read at.
        pub seen: Cell<Option<u64>>,
        pub loading: Cell<bool>,
        /// A control set from the filter is not a person changing it.
        pub updating: Cell<bool>,
        /// Each chip above the grid, with what it says.
        pub chip_buttons: RefCell<Vec<(gtk::Button, String)>>,
        pub toast: RefCell<String>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for Gallery {
        const NAME: &'static str = "PmGallery";
        type Type = super::Gallery;
        type ParentType = adw::BreakpointBin;

        fn class_init(klass: &mut Self::Class) {
            PhotoPage::ensure_type();
            klass.bind_template();
        }

        fn instance_init(obj: &InitializingObject<Self>) {
            obj.init_template();
        }
    }

    impl ObjectImpl for Gallery {
        fn constructed(&self) {
            self.parent_constructed();
            let gallery = self.obj();
            gallery.build_controls();
            gallery.build_grid();
            gallery.build_photo();
            gallery.build_files();
            let places = gallery.build_tree(&self.places_list, gallery.places_store().upcast_ref(), Side::Places);
            let _ = self.place_tree.set(places);
            let tags = gallery.build_tree(&self.tags_list, gallery.tags_store().upcast_ref(), Side::Tags);
            let _ = self.tag_tree.set(tags);
            let found = gallery.build_people();
            gallery.build_tree(&self.people_list, found.upcast_ref(), Side::People);
            gallery.build_tabs();
        }
    }

    impl WidgetImpl for Gallery {
        fn map(&self) {
            self.parent_map();
            let gallery = self.obj();
            let shown = self.filter.borrow().is_some();
            match shown {
                false => gallery.show(Filter::all()),
                true if gallery.is_stale() => gallery.requery(),
                true => {}
            }
        }
    }

    impl BreakpointBinImpl for Gallery {}
}

glib::wrapper! {
    pub struct Gallery(ObjectSubclass<imp::Gallery>)
        @extends adw::BreakpointBin, gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl Default for Gallery {
    fn default() -> Self {
        glib::Object::builder().build()
    }
}

impl Gallery {
    pub fn set_library(&self, library: Option<Rc<Library>>) {
        let imp = self.imp();
        *imp.loader.borrow_mut() = library
            .as_ref()
            .map(|library| thumbnails::textures(library.thumbs().clone()));
        imp.photo.set_library(library.clone(), imp.loader.borrow().clone());
        *imp.library.borrow_mut() = library;
        imp.seen.set(None);
        if imp.filter.borrow().is_some() {
            self.requery();
        }
    }

    /// Shows exactly the photos of the filter, and sets every control to its part of it.
    pub fn show(&self, filter: Filter) {
        let imp = self.imp();
        let count = imp.library.borrow().as_ref().and_then(|library| library.count(&filter));
        tracing::info!(filter = %filter, count, "photos shown");
        *imp.filter.borrow_mut() = Some(filter);
        imp.count_of.set(count);
        self.selection().unselect_all();
        self.show_controls();
        self.requery();
    }

    /// The filter on screen and how many it names.
    pub fn shown(&self) -> Option<(Filter, Option<i64>)> {
        let filter = self.imp().filter.borrow().clone()?;
        Some((filter, self.imp().count_of.get()))
    }

    pub fn order(&self) -> Order {
        self.imp().order.get()
    }

    pub fn set_order(&self, order: Order) {
        if self.order() == order {
            return;
        }
        self.imp().order.set(order);
        self.show_controls();
        self.requery();
    }

    pub fn set_gap(&self, gap: Option<Gap>) {
        let filter = self.filter().with_gap(gap);
        self.show(filter);
    }

    /// Narrows to a country or an event; the one already chosen widens back.
    pub fn choose_place(&self, folder: &str) {
        let filter = self.filter();
        let filter = match filter.within.as_deref() == Some(folder) {
            true => filter.anywhere(),
            false => filter.within(folder),
        };
        self.show(filter);
    }

    /// Narrows to a tag and everything below it, together with the tags chosen; a chosen one
    /// again widens back.
    pub fn choose_tag(&self, path: &str) {
        self.show(self.filter().toggle_tag(path));
    }

    /// Narrows to the photos naming a person, together with the people chosen; a chosen one
    /// again widens back.
    pub fn choose_person(&self, name: &str) {
        self.show(self.filter().toggle_person(name));
    }

    /// Takes one part of the filter out, and nothing else.
    pub fn remove_part(&self, part: &Part) {
        let filter = self.filter();
        let filter = match part {
            Part::Within => filter.anywhere(),
            Part::Kind(kind) => filter.without(kind),
        };
        self.show(filter);
    }

    /// Back to the whole library, in the same order.
    pub fn clear_filter(&self) {
        self.show(Filter::all());
    }

    /// Narrows the people list to the names holding this text.
    pub fn search_people(&self, text: &str) {
        self.imp().people_search.set_text(text);
        self.refilter_people();
    }

    /// The people the list shows, after the search.
    pub fn people_listed(&self) -> Vec<String> {
        let list = self.imp().people_list.model();
        let Some(list) = list else {
            return Vec::new();
        };
        (0..list.n_items())
            .filter_map(|at| list.item(at).and_downcast::<gtk::TreeListRow>())
            .filter_map(|row| row.item().and_downcast::<Node>())
            .map(|node| node.name())
            .collect()
    }

    /// The sidebars whose tab carries a dot, by name.
    pub fn dots(&self) -> Vec<String> {
        self.imp()
            .dots
            .borrow()
            .iter()
            .filter(|(_, dot)| dot.get_visible())
            .map(|(side, _)| side.name().to_string())
            .collect()
    }

    /// Opens a sidebar by its name.
    pub fn browse_by(&self, name: &str) {
        self.imp().browse_by.set_active_name(Some(name));
    }

    /// Whether the photos asked for last are still on their way.
    pub fn is_loading(&self) -> bool {
        self.imp().loading.get()
    }

    /// Whether the sidebar counts for the filter on screen are still on their way.
    pub fn is_recounting(&self) -> bool {
        self.imp().recounting.get()
    }

    /// What the sidebar shows for an entry: its count, and whether it is listed at all.
    pub fn count_shown(&self, side: &str, key: &str) -> Option<(i64, bool)> {
        let side = Side::ALL.into_iter().find(|each| each.name() == side)?;
        let count = side.counted(self.imp().following.borrow().as_ref()?, key);
        Some((count, self.reachable(side, key)))
    }

    /// Whether an entry is listed: the counts are not in yet, it would show photos, or it or one
    /// below it is chosen.
    fn reachable(&self, side: Side, key: &str) -> bool {
        let following = self.imp().following.borrow();
        let Some(following) = following.as_ref() else {
            return true;
        };
        let below = format!("{key}/");
        side.counted(following, key) > 0
            || side
                .chosen(&self.filter())
                .iter()
                .any(|chosen| chosen == key || chosen.starts_with(&below))
    }

    /// `grid`, `files` or `empty`.
    pub fn page(&self) -> String {
        self.imp()
            .pages
            .visible_child_name()
            .map(|name| name.to_string())
            .unwrap_or_default()
    }

    /// The paths in the grid, in its order.
    pub fn listed(&self) -> Vec<String> {
        self.photos_store()
            .iter::<Photo>()
            .filter_map(Result::ok)
            .map(|photo| photo.listed().rel_path.clone())
            .collect()
    }

    /// What the chips above the grid say, in their order.
    pub fn chips(&self) -> Vec<String> {
        self.imp()
            .chip_buttons
            .borrow()
            .iter()
            .map(|(_, said)| said.clone())
            .collect()
    }

    /// Clicks the chip that says this, as a person would.
    pub fn close_chip(&self, said: &str) -> bool {
        let chip = self
            .imp()
            .chip_buttons
            .borrow()
            .iter()
            .find(|(_, text)| text == said)
            .map(|(button, _)| button.clone());
        chip.map(|chip| chip.emit_clicked()).is_some()
    }

    /// How many cells show a picture right now.
    pub fn pictures(&self) -> usize {
        descendants(self.imp().grid.upcast_ref())
            .into_iter()
            .filter_map(|widget| widget.downcast::<Thumb>().ok())
            .filter(Thumb::has_picture)
            .count()
    }

    pub fn kept(&self) -> usize {
        self.imp()
            .loader
            .borrow()
            .as_ref()
            .map(Loader::kept)
            .unwrap_or_default()
    }

    pub fn select(&self, position: u32, selected: bool) {
        match selected {
            true => self.selection().select_item(position, false),
            false => self.selection().unselect_item(position),
        };
    }

    pub fn select_all(&self) {
        self.selection().select_all();
    }

    pub fn select_none(&self) {
        self.selection().unselect_all();
    }

    pub fn selected(&self) -> u64 {
        self.selection().selection().size()
    }

    /// What a tool would work on: the whole filter when everything or nothing is selected, the
    /// selected photos in grid order otherwise.
    pub fn scope(&self) -> Option<Scope> {
        let filter = self.imp().filter.borrow().clone()?;
        let total = u64::from(self.photos_store().n_items());
        let selected = self.selected();
        if selected == 0 || selected == total {
            return Some(Scope::Filter(filter));
        }
        let store = self.photos_store();
        let set = self.selection().selection();
        let paths: Vec<String> = (0..store.n_items())
            .filter(|at| set.contains(*at))
            .filter_map(|at| store.item(at).and_downcast::<Photo>())
            .map(|photo| photo.listed().rel_path.clone())
            .collect();
        Some(Scope::Photos {
            title: format!(
                "{} photos picked from {}",
                paths.len(),
                lowercase_first(&filter.title())
            ),
            paths,
        })
    }

    pub fn photo(&self) -> PhotoPage {
        self.imp().photo.clone()
    }

    /// Whether a photo is open on its own page.
    pub fn photo_open(&self) -> bool {
        self.imp()
            .nav
            .visible_page()
            .is_some_and(|page| page.tag().as_deref() == Some("photo"))
    }

    /// Opens the photo at this position of the grid.
    pub fn open(&self, at: u32) {
        let imp = self.imp();
        if at >= self.photos_store().n_items() {
            return;
        }
        if !self.photo_open() {
            imp.nav.push_by_tag("photo");
        }
        imp.photo.open(at);
        tracing::info!(photo = imp.photo.path(), "photo opened");
    }

    /// Opens a photo by its path, when the grid holds it.
    pub fn show_photo(&self, rel_path: &str) -> bool {
        match self.listed().iter().position(|path| path == rel_path) {
            Some(at) => {
                self.open(at as u32);
                true
            }
            None => false,
        }
    }

    /// Back to the grid, once no unfinished edit is left.
    pub fn close_photo(&self) {
        if !self.photo_open() {
            return;
        }
        let gallery = self.downgrade();
        self.imp().photo.leave(move |_| {
            if let Some(gallery) = gallery.upgrade() {
                gallery.imp().nav.pop();
            }
        });
    }

    pub fn say(&self, text: &str) {
        *self.imp().toast.borrow_mut() = text.to_string();
        self.imp().toasts.add_toast(adw::Toast::new(text));
    }

    pub fn toast(&self) -> String {
        self.imp().toast.borrow().clone()
    }

    fn filter(&self) -> Filter {
        self.imp().filter.borrow().clone().unwrap_or_default()
    }

    /// Every part of the filter, each as its chip says it: the parts in their order, then the
    /// folder.
    fn parts(&self) -> Vec<(Part, String, String)> {
        let filter = self.filter();
        let mut parts: Vec<(Part, String, String)> = filter
            .kinds()
            .iter()
            .map(|kind| {
                let (said, tells) = match kind {
                    Kind::Missing(gap) => (gap.lacking().to_string(), format!("Photos {}", gap.lacking())),
                    Kind::Tagged(paths) if paths.len() == 1 => (format!("tag: {}", paths[0]), kind.title()),
                    Kind::Person(names) if names.len() == 1 => (names[0].clone(), kind.title()),
                    _ => (kind.title(), kind.title()),
                };
                (Part::Kind(kind.clone()), said, tells)
            })
            .collect();
        if let Some(folder) = &filter.within {
            let name = folder.rsplit('/').next().unwrap_or(folder).to_string();
            parts.push((Part::Within, name, format!("In {folder}")));
        }
        parts
    }

    fn is_stale(&self) -> bool {
        let library = self.imp().library.borrow().clone();
        library.is_some_and(|library| self.imp().seen.get() != Some(library.version()))
    }

    /// Counts every sidebar again for the filter on screen, off the main thread.
    fn recount(&self, library: &Library) {
        let imp = self.imp();
        let asked = imp.recount.get() + 1;
        imp.recount.set(asked);
        imp.recounting.set(true);
        let gallery = self.downgrade();
        library.following(&self.filter(), move |counted| {
            let Some(gallery) = gallery.upgrade() else {
                return;
            };
            let imp = gallery.imp();
            if imp.recount.get() != asked {
                return;
            }
            imp.recounting.set(false);
            match counted {
                Ok(following) => *imp.following.borrow_mut() = Some(following),
                Err(why) => tracing::error!(why, "the sidebars could not be counted"),
            }
            gallery.relist();
            gallery.mark_rows();
        });
    }

    fn requery(&self) {
        let imp = self.imp();
        let Some(library) = imp.library.borrow().clone() else {
            self.fill(Vec::new());
            return;
        };
        self.recount(&library);
        if self.is_stale() {
            imp.seen.set(Some(library.version()));
            if let Some(loader) = imp.loader.borrow().as_ref() {
                loader.clear();
            }
            let gallery = self.downgrade();
            library.sidebars(move |read| match (gallery.upgrade(), read) {
                (Some(gallery), Ok(sidebars)) => gallery.fill_sidebars(&sidebars),
                (_, Err(why)) => tracing::error!(why, "the sidebars could not be read"),
                _ => {}
            });
        }
        imp.loading.set(true);
        let gallery = self.downgrade();
        library.query(&self.filter(), self.order(), move |found| {
            let Some(gallery) = gallery.upgrade() else {
                return;
            };
            match found {
                Ok(rows) => gallery.fill(rows),
                Err(why) => {
                    tracing::error!(why, "the photos could not be listed");
                    gallery.fill(Vec::new());
                }
            }
        });
    }

    fn fill(&self, rows: Vec<Listed>) {
        let imp = self.imp();
        self.selection().unselect_all();
        let found = rows.len();
        let all_files = !rows.is_empty() && rows.iter().all(|row| !row.is_photo);
        let paths = self.file_paths();
        paths.splice(0, paths.n_items(), &[]);
        let photos: Vec<Photo> = match all_files {
            true => {
                let names: Vec<&str> = rows.iter().map(|row| row.rel_path.as_str()).collect();
                paths.splice(0, 0, &names);
                Vec::new()
            }
            false => rows.into_iter().map(Photo::of).collect(),
        };
        let store = self.photos_store();
        store.splice(0, store.n_items(), &photos);
        if store.n_items() > 0 {
            imp.grid.scroll_to(0, gtk::ListScrollFlags::NONE, None);
        }
        imp.pages.set_visible_child_name(match (found, all_files) {
            (0, _) => "empty",
            (_, true) => "files",
            _ => "grid",
        });
        if imp.count_of.get().is_none() && imp.filter.borrow().is_some() {
            imp.count_of.set(Some(found as i64));
        }
        imp.loading.set(false);
        self.show_header();
        self.show_selection();
    }

    fn fill_sidebars(&self, sidebars: &Sidebars) {
        let places: Vec<Node> = sidebars.places.iter().map(Node::of_place).collect();
        let tags: Vec<Node> = sidebars.tags.tree().iter().map(Node::of_tag).collect();
        let people: Vec<Node> = sidebars.people.iter().map(Node::of_person).collect();
        let store = self.places_store();
        store.splice(0, store.n_items(), &places);
        let store = self.tags_store();
        store.splice(0, store.n_items(), &tags);
        let store = self.people_store();
        store.splice(0, store.n_items(), &people);
        self.show_people_empty();
        self.mark_rows();
    }

    fn show_controls(&self) {
        let imp = self.imp();
        let filter = self.filter();
        imp.updating.set(true);
        let at = filter
            .gap()
            .and_then(|gap| Gap::ALL.iter().position(|each| *each == gap))
            .map(|at| at + 1)
            .unwrap_or(0);
        imp.gap.set_selected(at as u32);
        let at = ORDERS.iter().position(|order| *order == self.order()).unwrap_or(0);
        imp.sort.set_selected(at as u32);
        imp.updating.set(false);

        self.show_chips();
        for (side, dot) in imp.dots.borrow().iter() {
            let narrows = side.narrows(&filter);
            dot.set_visible(narrows);
            // The tab's button is made by the toggle group around the content.
            let mut button = dot.parent();
            while let Some(widget) = button.as_ref().filter(|widget| !widget.is::<gtk::ToggleButton>()) {
                button = widget.parent();
            }
            if let Some(button) = button {
                match narrows {
                    true => button.update_property(&[gtk::accessible::Property::Description("Narrows the photos")]),
                    false => button.reset_property(gtk::AccessibleProperty::Description),
                }
            }
        }
        self.show_header();
        self.mark_rows();
    }

    /// One chip per part of the filter, each closing its part, and Clear All after them; none
    /// for the whole library.
    fn show_chips(&self) {
        let imp = self.imp();
        while let Some(child) = imp.chips.first_child() {
            imp.chips.remove(&child);
        }
        imp.chip_buttons.borrow_mut().clear();
        let parts = self.parts();
        imp.chips.set_visible(!parts.is_empty());
        if parts.is_empty() {
            return;
        }
        for (part, said, tells) in parts {
            let chip = gtk::Button::builder()
                .child(
                    &adw::ButtonContent::builder()
                        .icon_name("window-close-symbolic")
                        .label(&said)
                        .can_shrink(true)
                        .build(),
                )
                .tooltip_text(format!("{tells}. Show without this"))
                .build();
            chip.update_property(&[gtk::accessible::Property::Label(&format!("Remove {said}"))]);
            chip.connect_clicked(glib::clone!(
                #[weak(rename_to = gallery)]
                self,
                move |_| gallery.remove_part(&part)
            ));
            imp.chips.append(&chip);
            imp.chip_buttons.borrow_mut().push((chip, said));
        }
        let clear = gtk::Button::builder()
            .label("Clear All")
            .tooltip_text("Show every photo")
            .build();
        clear.add_css_class("flat");
        clear.connect_clicked(glib::clone!(
            #[weak(rename_to = gallery)]
            self,
            move |_| gallery.clear_filter()
        ));
        imp.chips.append(&clear);
    }

    fn show_header(&self) {
        let imp = self.imp();
        let Some(filter) = imp.filter.borrow().clone() else {
            return;
        };
        imp.title.set_label(&filter.title());
        imp.title.set_tooltip_text(Some(&filter.title()));
        let noun = match filter.is_of_files() {
            true => ("file", "files"),
            false => ("photo", "photos"),
        };
        imp.count.set_label(&match imp.count_of.get() {
            Some(1) => format!("1 {}", noun.0),
            Some(count) => format!("{count} {}", noun.1),
            None => "Counting".to_string(),
        });
    }

    fn show_selection(&self) {
        let imp = self.imp();
        let selected = self.selected();
        imp.selection_bar.set_revealed(selected > 0);
        imp.selected.set_label(&format!("{selected} selected"));
    }

    /// Lists again only the entries that would still show photos, the chosen people first.
    fn relist(&self) {
        let imp = self.imp();
        for reachable in imp.reachable.borrow().iter() {
            reachable.changed(gtk::FilterChange::Different);
        }
        if let Some(order) = imp.people_order.get() {
            order.changed(gtk::SorterChange::Different);
        }
        self.show_people_empty();
    }

    /// Sets each sidebar row's mark to whether it is a chosen place, tag or person, and opens
    /// the rows above the chosen ones so they can be seen.
    fn mark_rows(&self) {
        let filter = self.filter();
        let imp = self.imp();
        for (tree, side) in [(imp.place_tree.get(), Side::Places), (imp.tag_tree.get(), Side::Tags)] {
            let Some(tree) = tree else {
                continue;
            };
            for chosen in side.chosen(&filter) {
                let roots = (0..tree.model().n_items()).filter_map(|at| tree.child_row(at));
                reveal(roots.collect(), &chosen);
            }
        }
        let following = imp.following.borrow();
        imp.rows.borrow_mut().retain(|(item, side)| {
            let Some(item) = item.upgrade() else {
                return false;
            };
            mark(&item, *side, &side.chosen(&filter), following.as_ref());
            true
        });
    }

    fn build_controls(&self) {
        let imp = self.imp();
        let mut gaps = vec!["All Photos"];
        gaps.extend(Gap::ALL.iter().map(|gap| short(*gap)));
        imp.gap.set_model(Some(&gtk::StringList::new(&gaps)));
        imp.sort.set_model(Some(&gtk::StringList::new(&["Date", "Name"])));

        imp.gap.connect_selected_notify(glib::clone!(
            #[weak(rename_to = gallery)]
            self,
            move |dropdown| {
                if gallery.imp().updating.get() {
                    return;
                }
                let gap = (dropdown.selected() as usize)
                    .checked_sub(1)
                    .and_then(|at| Gap::ALL.get(at).copied());
                gallery.set_gap(gap);
            }
        ));
        imp.sort.connect_selected_notify(glib::clone!(
            #[weak(rename_to = gallery)]
            self,
            move |dropdown| {
                if gallery.imp().updating.get() {
                    return;
                }
                let order = ORDERS.get(dropdown.selected() as usize).copied().unwrap_or_default();
                gallery.set_order(order);
            }
        ));
        imp.browse_by.connect_active_name_notify(glib::clone!(
            #[weak(rename_to = gallery)]
            self,
            move |group| {
                if let Some(name) = group.active_name() {
                    gallery.imp().sidebars.set_visible_child_name(&name);
                }
            }
        ));
    }

    fn build_grid(&self) {
        let imp = self.imp();
        let selection = self.selection();
        imp.grid.set_model(Some(selection));
        selection.connect_selection_changed(glib::clone!(
            #[weak(rename_to = gallery)]
            self,
            move |_, _, _| gallery.show_selection()
        ));

        let factory = gtk::SignalListItemFactory::new();
        factory.connect_setup(|_, item| {
            listed(item).set_child(Some(&Thumb::default()));
        });
        factory.connect_bind(glib::clone!(
            #[weak(rename_to = gallery)]
            self,
            move |_, item| {
                let item = listed(item);
                let (Some(photo), Some(thumb)) = (
                    item.item().and_downcast::<Photo>(),
                    item.child().and_downcast::<Thumb>(),
                ) else {
                    return;
                };
                let loader = gallery.imp().loader.borrow();
                let Some(loader) = loader.as_ref() else {
                    return;
                };
                thumb.bind(photo.listed(), gallery.request(photo.listed()), loader);
            }
        ));
        factory.connect_unbind(glib::clone!(
            #[weak(rename_to = gallery)]
            self,
            move |_, item| {
                let Some(thumb) = listed(item).child().and_downcast::<Thumb>() else {
                    return;
                };
                if let Some(loader) = gallery.imp().loader.borrow().as_ref() {
                    thumb.unbind(loader);
                }
            }
        ));
        imp.grid.set_factory(Some(&factory));
    }

    fn build_photo(&self) {
        let imp = self.imp();
        imp.photo.set_list(self.photos_store());
        imp.grid.connect_activate(glib::clone!(
            #[weak(rename_to = gallery)]
            self,
            move |_, at| gallery.open(at)
        ));
        imp.nav.connect_popped(glib::clone!(
            #[weak(rename_to = gallery)]
            self,
            move |_, page| {
                if page.tag().as_deref() != Some("photo") {
                    return;
                }
                let photo = &gallery.imp().photo;
                let at = photo.position();
                photo.close_down();
                if at < gallery.photos_store().n_items() {
                    gallery.imp().grid.scroll_to(at, gtk::ListScrollFlags::FOCUS, None);
                }
            }
        ));
    }

    /// Where a cell's picture comes from. A file that is not a photo has none to ask for.
    fn request(&self, listed: &Listed) -> Option<Request> {
        if !listed.is_photo {
            return None;
        }
        let library = self.imp().library.borrow().clone()?;
        Some(Request {
            key: listed.content_id.clone().unwrap_or_else(|| listed.rel_path.clone()),
            content_id: listed.content_id.clone(),
            file: library.paths().library().join(&listed.rel_path),
            orientation: listed.orientation,
        })
    }

    fn build_files(&self) {
        let imp = self.imp();
        imp.files
            .set_model(Some(&gtk::NoSelection::new(Some(self.file_paths().clone()))));
        let factory = gtk::SignalListItemFactory::new();
        factory.connect_setup(|_, item| {
            let label = gtk::Label::builder()
                .xalign(0.0)
                .ellipsize(pango::EllipsizeMode::Middle)
                .build();
            listed(item).set_child(Some(&label));
        });
        factory.connect_bind(|_, item| {
            let item = listed(item);
            let (Some(path), Some(label)) = (
                item.item().and_downcast::<gtk::StringObject>(),
                item.child().and_downcast::<gtk::Label>(),
            ) else {
                return;
            };
            label.set_label(&path.string());
            label.set_tooltip_text(Some(&path.string()));
        });
        imp.files.set_factory(Some(&factory));
    }

    /// The dot on each tab, beside its name.
    fn build_tabs(&self) {
        let imp = self.imp();
        for side in Side::ALL {
            let Some(toggle) = imp.browse_by.toggle_by_name(side.name()) else {
                continue;
            };
            let dot = gtk::Image::from_icon_name("media-record-symbolic");
            dot.set_pixel_size(8);
            dot.add_css_class("accent");
            dot.set_visible(false);
            dot.set_accessible_role(gtk::AccessibleRole::Presentation);
            let label = gtk::Label::new(Some(side.title()));
            let content = gtk::Box::builder().spacing(4).halign(gtk::Align::Center).build();
            content.append(&label);
            content.append(&dot);
            toggle.set_child(Some(&content));
            if let Some(button) = content.parent() {
                button.update_relation(&[gtk::accessible::Relation::LabelledBy(&[label.upcast_ref()])]);
            }
            imp.dots.borrow_mut().push((side, dot));
        }
    }

    /// The people list: those the filter still reaches, narrowed by the search above it, the
    /// chosen ones first in the order chosen, then the most photos.
    fn build_people(&self) -> gtk::SortListModel {
        let imp = self.imp();
        let search = imp.people_search.downgrade();
        let found = gtk::CustomFilter::new(move |item| {
            let text = search
                .upgrade()
                .map(|entry| entry.text().to_lowercase())
                .unwrap_or_default();
            let text = text.trim();
            text.is_empty()
                || item
                    .downcast_ref::<Node>()
                    .is_some_and(|node| node.name().to_lowercase().contains(text))
        });
        let both = gtk::EveryFilter::new();
        both.append(found.clone());
        both.append(self.reachable_filter(Side::People));
        let model = gtk::FilterListModel::new(Some(self.people_store().clone()), Some(both));
        let gallery = self.downgrade();
        let order = gtk::CustomSorter::new(move |one, other| {
            let (Some(gallery), Some(one), Some(other)) = (
                gallery.upgrade(),
                one.downcast_ref::<Node>(),
                other.downcast_ref::<Node>(),
            ) else {
                return gtk::Ordering::Equal;
            };
            let chosen = Side::People.chosen(&gallery.filter());
            let at = |node: &Node| chosen.iter().position(|name| *name == node.key()).unwrap_or(usize::MAX);
            let following = gallery.imp().following.borrow();
            let photos = |node: &Node| {
                following
                    .as_ref()
                    .map_or(node.photos(), |following| Side::People.counted(following, &node.key()))
            };
            at(one)
                .cmp(&at(other))
                .then(photos(other).cmp(&photos(one)))
                .then(one.name().cmp(&other.name()))
                .into()
        });
        let sorted = gtk::SortListModel::new(Some(model), Some(order.clone()));
        imp.people_search.connect_search_changed(glib::clone!(
            #[weak(rename_to = gallery)]
            self,
            move |_| gallery.refilter_people()
        ));
        let _ = imp.people_found.set(found);
        let _ = imp.people_order.set(order);
        sorted
    }

    /// A filter of the entries of a side that would still show photos, refiltered as the counts
    /// come in.
    fn reachable_filter(&self, side: Side) -> gtk::CustomFilter {
        let gallery = self.downgrade();
        let reachable = gtk::CustomFilter::new(move |item| {
            let (Some(gallery), Some(node)) = (gallery.upgrade(), item.downcast_ref::<Node>()) else {
                return true;
            };
            gallery.reachable(side, &node.key())
        });
        self.imp().reachable.borrow_mut().push(reachable.clone());
        reachable
    }

    fn refilter_people(&self) {
        if let Some(found) = self.imp().people_found.get() {
            found.changed(gtk::FilterChange::Different);
        }
        self.show_people_empty();
    }

    /// The status page instead of an empty list: no people at all, none the search finds, or
    /// none in the photos the filter shows.
    fn show_people_empty(&self) {
        let imp = self.imp();
        let searched = !imp.people_search.text().trim().is_empty();
        let none = self.people_listed().is_empty();
        imp.people_empty.set_visible(none);
        imp.people_list.set_visible(!none);
        let (title, description) = match (searched, self.people_store().n_items()) {
            (true, _) => ("No Match", "No person has that in their name."),
            (false, 0) => ("No People", "No photo names a person yet."),
            (false, _) => ("No People Here", "None of the photos shown names a person."),
        };
        imp.people_empty.set_title(title);
        imp.people_empty.set_description(Some(description));
    }

    /// A sidebar's tree. Places and tags list only the entries that would still show photos, at
    /// every level; people come filtered already.
    fn build_tree(&self, list: &gtk::ListView, roots: &gio::ListModel, side: Side) -> gtk::TreeListModel {
        let reachable = (side != Side::People).then(|| self.reachable_filter(side));
        let roots: gio::ListModel = match &reachable {
            Some(reachable) => gtk::FilterListModel::new(Some(roots.clone()), Some(reachable.clone())).upcast(),
            None => roots.clone(),
        };
        let tree = gtk::TreeListModel::new(roots, false, false, move |item| {
            let children = item.downcast_ref::<Node>().and_then(Node::children)?;
            Some(match &reachable {
                Some(reachable) => gtk::FilterListModel::new(Some(children), Some(reachable.clone())).upcast(),
                None => children.upcast(),
            })
        });
        list.set_model(Some(&gtk::NoSelection::new(Some(tree.clone()))));

        let factory = gtk::SignalListItemFactory::new();
        factory.connect_setup(glib::clone!(
            #[weak(rename_to = gallery)]
            self,
            move |_, item| {
                let item = listed(item);
                let name = gtk::Label::builder()
                    .xalign(0.0)
                    .hexpand(true)
                    .ellipsize(pango::EllipsizeMode::End)
                    .build();
                let check = gtk::Image::from_icon_name("object-select-symbolic");
                check.set_visible(false);
                let count = gtk::Label::new(None);
                count.add_css_class("dim-label");
                count.add_css_class("numeric");
                let row = gtk::Box::builder().spacing(6).build();
                row.append(&name);
                row.append(&check);
                row.append(&count);
                let expander = gtk::TreeExpander::new();
                expander.set_child(Some(&row));
                item.set_child(Some(&expander));
                gallery.imp().rows.borrow_mut().push((item.downgrade(), side));
            }
        ));
        factory.connect_bind(glib::clone!(
            #[weak(rename_to = gallery)]
            self,
            move |_, item| {
                let item = listed(item);
                let (Some(row), Some(expander)) = (
                    item.item().and_downcast::<gtk::TreeListRow>(),
                    item.child().and_downcast::<gtk::TreeExpander>(),
                ) else {
                    return;
                };
                let Some(node) = row.item().and_downcast::<Node>() else {
                    return;
                };
                expander.set_list_row(Some(&row));
                let parts = row_parts(&expander);
                if let Some((name, _, _)) = parts {
                    name.set_label(&node.name());
                    name.set_tooltip_text(Some(&node.key()));
                }
                let following = gallery.imp().following.borrow();
                mark(&item, side, &side.chosen(&gallery.filter()), following.as_ref());
            }
        ));
        list.set_factory(Some(&factory));

        list.connect_activate(glib::clone!(
            #[weak(rename_to = gallery)]
            self,
            move |list, position| {
                let Some(node) = list
                    .model()
                    .and_then(|model| model.item(position))
                    .and_downcast::<gtk::TreeListRow>()
                    .and_then(|row| row.item())
                    .and_downcast::<Node>()
                else {
                    return;
                };
                match side {
                    Side::Places => gallery.choose_place(&node.key()),
                    Side::Tags => gallery.choose_tag(&node.key()),
                    Side::People => gallery.choose_person(&node.key()),
                }
            }
        ));
        tree
    }

    fn photos_store(&self) -> &gio::ListStore {
        self.imp().photos.get_or_init(gio::ListStore::new::<Photo>)
    }

    fn selection(&self) -> &gtk::MultiSelection {
        self.imp()
            .selection
            .get_or_init(|| gtk::MultiSelection::new(Some(self.photos_store().clone())))
    }

    fn file_paths(&self) -> &gtk::StringList {
        self.imp().file_paths.get_or_init(|| gtk::StringList::new(&[]))
    }

    fn places_store(&self) -> &gio::ListStore {
        self.imp().places.get_or_init(gio::ListStore::new::<Node>)
    }

    fn tags_store(&self) -> &gio::ListStore {
        self.imp().tags.get_or_init(gio::ListStore::new::<Node>)
    }

    fn people_store(&self) -> &gio::ListStore {
        self.imp().people.get_or_init(gio::ListStore::new::<Node>)
    }
}

/// A part of the filter a chip stands for.
#[derive(Debug, Clone, PartialEq)]
pub enum Part {
    Kind(Kind),
    /// The folder it is narrowed to.
    Within,
}

/// Expands every row above `chosen`: `China` for `China/2006-09-00 Besuch Ben`.
fn reveal(rows: Vec<gtk::TreeListRow>, chosen: &str) {
    for row in rows {
        let Some(node) = row.item().and_downcast::<Node>() else {
            continue;
        };
        if !chosen.starts_with(&format!("{}/", node.key())) {
            continue;
        }
        row.set_expanded(true);
        let below = row.children().map(|children| children.n_items()).unwrap_or(0);
        reveal((0..below).filter_map(|at| row.child_row(at)).collect(), chosen);
    }
}

fn row_parts(expander: &gtk::TreeExpander) -> Option<(gtk::Label, gtk::Image, gtk::Label)> {
    let row = expander.child()?;
    let name = row.first_child().and_downcast::<gtk::Label>()?;
    let check = name.next_sibling().and_downcast::<gtk::Image>()?;
    let count = check.next_sibling().and_downcast::<gtk::Label>()?;
    Some((name, check, count))
}

/// Shows the check on a row that is a chosen place, tag or person, and how many photos the row
/// would show with the filter.
fn mark(item: &gtk::ListItem, side: Side, chosen: &[String], following: Option<&Following>) {
    let Some(expander) = item.child().and_downcast::<gtk::TreeExpander>() else {
        return;
    };
    let Some(node) = item
        .item()
        .and_downcast::<gtk::TreeListRow>()
        .and_then(|row| row.item())
        .and_downcast::<Node>()
    else {
        return;
    };
    let is_chosen = chosen.contains(&node.key());
    let photos = following.map_or(node.photos(), |following| side.counted(following, &node.key()));
    if let Some((_, check, count)) = row_parts(&expander) {
        check.set_visible(is_chosen);
        count.set_label(&photos.to_string());
    }
    let mut label = format!("{}, {photos} photos", node.name());
    if is_chosen {
        label.push_str(", chosen");
    }
    item.set_accessible_label(&label);
}

fn listed(item: &glib::Object) -> gtk::ListItem {
    item.clone().downcast::<gtk::ListItem>().expect("a list item")
}

/// A gap in the few words a dropdown has room for.
fn short(gap: Gap) -> &'static str {
    match gap {
        Gap::Gps => "No GPS",
        Gap::Date => "No Date",
        Gap::DateOffFolder => "Date Off Folder",
        Gap::Tags => "No Tags",
        Gap::People => "No People",
        Gap::Location => "No Location Text",
    }
}

fn lowercase_first(text: &str) -> String {
    let mut chars = text.chars();
    match chars.next() {
        Some(first) => first.to_lowercase().chain(chars).collect(),
        None => String::new(),
    }
}

fn descendants(widget: &gtk::Widget) -> Vec<gtk::Widget> {
    let mut found = Vec::new();
    let mut child = widget.first_child();
    while let Some(widget) = child {
        found.push(widget.clone());
        found.extend(descendants(&widget));
        child = widget.next_sibling();
    }
    found
}
