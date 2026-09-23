//! Finding the photos a tool should work on. The page shows exactly `Filter::photos` of one
//! filter, and each control owns one part of it: the place sidebar the folder, the tag sidebar
//! the tag, the dropdown the gap. Whatever else the dashboard handed over stays as a chip. Every
//! change ends in `show`, so a click here and a click on the dashboard end up in the same place.
//!
//! The grid is a `GtkGridView` over a list store of small cell objects, bound by hand in the
//! factory. A cell asks for its picture when it is bound and lets go of it when it is unbound.

use std::cell::{Cell, OnceCell, RefCell};
use std::rc::Rc;

use adw::prelude::*;
use adw::subclass::prelude::*;
use gtk::glib::subclass::InitializingObject;
use gtk::{gdk, gio, glib, pango};

use photomanager_core::browse::{Place, Tag};
use photomanager_core::filter::{Filter, Gap, Kind, Listed, Order};
use photomanager_core::scope::Scope;

use crate::library::{Library, Sidebars};
use crate::thumbnails::{self, Loader, Request, Slot};

const ORDERS: [Order; 2] = [Order::Date, Order::Name];

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
use photo::Photo;
use thumb::Thumb;

mod imp {
    use super::*;

    #[derive(Debug, Default, gtk::CompositeTemplate)]
    #[template(resource = "/org/beijingcode/PhotoManager/gallery.ui")]
    pub struct Gallery {
        #[template_child]
        pub toasts: TemplateChild<adw::ToastOverlay>,
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
        pub title: TemplateChild<gtk::Label>,
        #[template_child]
        pub count: TemplateChild<gtk::Label>,
        #[template_child]
        pub chips: TemplateChild<gtk::Box>,
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
        pub place_tree: OnceCell<gtk::TreeListModel>,
        pub tag_tree: OnceCell<gtk::TreeListModel>,
        /// Every sidebar row made, so its mark can follow the filter.
        pub rows: RefCell<Vec<(glib::WeakRef<gtk::ListItem>, bool)>>,
        /// The library version the sidebars were read at.
        pub seen: Cell<Option<u64>>,
        pub loading: Cell<bool>,
        /// A control set from the filter is not a person changing it.
        pub updating: Cell<bool>,
        pub chip_buttons: RefCell<Vec<gtk::Button>>,
        pub toast: RefCell<String>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for Gallery {
        const NAME: &'static str = "PmGallery";
        type Type = super::Gallery;
        type ParentType = adw::BreakpointBin;

        fn class_init(klass: &mut Self::Class) {
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
            gallery.build_files();
            let places = gallery.build_tree(&self.places_list, gallery.places_store(), true);
            let _ = self.place_tree.set(places);
            let tags = gallery.build_tree(&self.tags_list, gallery.tags_store(), false);
            let _ = self.tag_tree.set(tags);
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

    /// Narrows to a tag and everything below it; the one already chosen widens back.
    pub fn choose_tag(&self, path: &str) {
        let filter = self.filter();
        let filter = match chosen_tag(&filter).as_deref() == Some(path) {
            true => filter.with_tag(None),
            false => filter.with_tag(Some(path)),
        };
        self.show(filter);
    }

    /// Takes a part the dashboard handed over back out.
    pub fn remove_part(&self, kind: &Kind) {
        let filter = self.filter().without(kind);
        self.show(filter);
    }

    /// Whether the photos asked for last are still on their way.
    pub fn is_loading(&self) -> bool {
        self.imp().loading.get()
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

    /// The parts shown as chips, by title.
    pub fn chips(&self) -> Vec<String> {
        self.extras().iter().map(Kind::title).collect()
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

    /// Whatever the controls do not own.
    fn extras(&self) -> Vec<Kind> {
        let filter = self.filter();
        filter
            .kinds()
            .iter()
            .filter(|kind| match kind {
                Kind::Missing(_) => false,
                Kind::Tagged(paths) => paths.len() > 1,
                _ => true,
            })
            .cloned()
            .collect()
    }

    fn is_stale(&self) -> bool {
        let library = self.imp().library.borrow().clone();
        library.is_some_and(|library| self.imp().seen.get() != Some(library.version()))
    }

    fn requery(&self) {
        let imp = self.imp();
        let Some(library) = imp.library.borrow().clone() else {
            self.fill(Vec::new());
            return;
        };
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
        let store = self.places_store();
        store.splice(0, store.n_items(), &places);
        let store = self.tags_store();
        store.splice(0, store.n_items(), &tags);
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

        for chip in imp.chip_buttons.borrow_mut().drain(..) {
            imp.chips.remove(&chip);
        }
        for kind in self.extras() {
            let title = kind.title();
            let chip = gtk::Button::builder()
                .child(
                    &adw::ButtonContent::builder()
                        .icon_name("window-close-symbolic")
                        .label(&title)
                        .can_shrink(true)
                        .build(),
                )
                .tooltip_text("Show without this")
                .valign(gtk::Align::Center)
                .build();
            chip.update_property(&[gtk::accessible::Property::Label(&format!("Remove {title}"))]);
            chip.connect_clicked(glib::clone!(
                #[weak(rename_to = gallery)]
                self,
                move |_| gallery.remove_part(&kind)
            ));
            imp.chips.append(&chip);
            imp.chip_buttons.borrow_mut().push(chip);
        }
        self.show_header();
        self.mark_rows();
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

    /// Sets each sidebar row's mark to whether it is the chosen place or tag, and opens the
    /// rows above the chosen one so it can be seen.
    fn mark_rows(&self) {
        let filter = self.filter();
        let place = filter.within.clone();
        let tag = chosen_tag(&filter);
        let imp = self.imp();
        for (tree, chosen) in [(imp.place_tree.get(), &place), (imp.tag_tree.get(), &tag)] {
            if let (Some(tree), Some(chosen)) = (tree, chosen) {
                let roots = (0..tree.model().n_items()).filter_map(|at| tree.child_row(at));
                reveal(roots.collect(), chosen);
            }
        }
        self.imp().rows.borrow_mut().retain(|(item, is_place)| {
            let Some(item) = item.upgrade() else {
                return false;
            };
            let chosen = match is_place {
                true => place.as_ref(),
                false => tag.as_ref(),
            };
            mark(&item, chosen);
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

    fn build_tree(&self, list: &gtk::ListView, roots: &gio::ListStore, is_place: bool) -> gtk::TreeListModel {
        let tree = gtk::TreeListModel::new(roots.clone(), false, false, |item| {
            item.downcast_ref::<Node>()
                .and_then(Node::children)
                .map(|store| store.upcast())
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
                gallery.imp().rows.borrow_mut().push((item.downgrade(), is_place));
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
                if let Some((name, _, count)) = parts {
                    name.set_label(&node.name());
                    name.set_tooltip_text(Some(&node.key()));
                    count.set_label(&node.photos().to_string());
                }
                let filter = gallery.filter();
                let chosen = match is_place {
                    true => filter.within.clone(),
                    false => chosen_tag(&filter),
                };
                mark(&item, chosen.as_ref());
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
                match is_place {
                    true => gallery.choose_place(&node.key()),
                    false => gallery.choose_tag(&node.key()),
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
}

/// The one tag the tag sidebar owns, when the filter has exactly one.
fn chosen_tag(filter: &Filter) -> Option<String> {
    match filter.tags() {
        Some([one]) => Some(one.clone()),
        _ => None,
    }
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

/// Shows the check on the row that is the chosen place or tag, and says so.
fn mark(item: &gtk::ListItem, chosen: Option<&String>) {
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
    let is_chosen = chosen == Some(&node.key());
    if let Some((_, check, _)) = row_parts(&expander) {
        check.set_visible(is_chosen);
    }
    let mut label = format!("{}, {} photos", node.name(), node.photos());
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
