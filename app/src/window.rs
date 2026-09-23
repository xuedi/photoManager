use std::rc::Rc;

use adw::prelude::*;
use adw::subclass::prelude::*;
use gtk::glib;
use gtk::glib::subclass::InitializingObject;
use photomanager_core::filter::{Filter, Gap, Order};
use photomanager_core::scan::Mode;
use photomanager_core::scope::Scope;

use crate::dashboard::Dashboard;
use crate::gallery::Gallery;
use crate::library::Library;
use crate::preview::Preview;
use crate::tools::Tools;

pub const VIEWS: [&str; 4] = ["dashboard", "gallery", "tools", "suggestions"];

mod imp {
    use super::*;

    #[derive(Debug, Default, gtk::CompositeTemplate)]
    #[template(resource = "/org/beijingcode/PhotoManager/window.ui")]
    pub struct Window {
        #[template_child]
        pub stack: TemplateChild<adw::ViewStack>,
        #[template_child]
        pub import_button: TemplateChild<gtk::Button>,
        #[template_child]
        pub dashboard: TemplateChild<Dashboard>,
        #[template_child]
        pub tools: TemplateChild<Tools>,
        #[template_child]
        pub gallery: TemplateChild<Gallery>,
        pub library: std::cell::RefCell<Option<Rc<Library>>>,
        /// What the next tool works on, as the gallery handed it over.
        pub scope: std::cell::RefCell<Option<Scope>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for Window {
        const NAME: &'static str = "PmWindow";
        type Type = super::Window;
        type ParentType = adw::ApplicationWindow;

        fn class_init(klass: &mut Self::Class) {
            Dashboard::ensure_type();
            Gallery::ensure_type();
            Tools::ensure_type();
            klass.bind_template();
        }

        fn instance_init(obj: &InitializingObject<Self>) {
            obj.init_template();
        }
    }

    impl ObjectImpl for Window {
        fn constructed(&self) {
            self.parent_constructed();
            self.obj().setup_actions();
        }
    }

    impl WidgetImpl for Window {}
    impl WindowImpl for Window {}
    impl ApplicationWindowImpl for Window {}
    impl AdwApplicationWindowImpl for Window {}
}

glib::wrapper! {
    pub struct Window(ObjectSubclass<imp::Window>)
        @extends adw::ApplicationWindow, gtk::ApplicationWindow, gtk::Window, gtk::Widget,
        @implements gtk::gio::ActionGroup, gtk::gio::ActionMap, gtk::Accessible, gtk::Buildable,
                    gtk::ConstraintTarget, gtk::Native, gtk::Root, gtk::ShortcutManager;
}

impl Window {
    pub fn new(app: &adw::Application) -> Self {
        glib::Object::builder().property("application", app).build()
    }

    pub fn set_library(&self, library: Option<Rc<Library>>) {
        self.imp().dashboard.set_library(library.clone());
        self.imp().tools.set_library(library.clone());
        self.imp().gallery.set_library(library.clone());
        *self.imp().library.borrow_mut() = library;
    }

    pub fn library(&self) -> Option<Rc<Library>> {
        self.imp().library.borrow().clone()
    }

    pub fn dashboard(&self) -> Dashboard {
        self.imp().dashboard.clone()
    }

    pub fn gallery(&self) -> Gallery {
        self.imp().gallery.clone()
    }

    pub fn tools(&self) -> Tools {
        self.imp().tools.clone()
    }

    pub fn preview(&self) -> Preview {
        self.imp().tools.preview()
    }

    pub fn scan(&self, mode: Mode) {
        self.imp().dashboard.scan(mode);
    }

    pub fn fill_thumbnails(&self) {
        self.imp().dashboard.fill_thumbnails();
    }

    /// Hands a set of photos to the gallery and shows it.
    pub fn show_photos(&self, filter: Filter) {
        self.imp().gallery.show(filter);
        self.show_view("gallery");
    }

    pub fn shown(&self) -> Option<(Filter, Option<i64>)> {
        self.imp().gallery.shown()
    }

    /// Makes what the gallery shows, or what is selected in it, what the next tool works on.
    pub fn use_as_scope(&self) {
        let gallery = &self.imp().gallery;
        let Some(scope) = gallery.scope() else {
            return;
        };
        let told = match &scope {
            Scope::Filter(filter) => {
                let count = self.library().and_then(|library| library.count(filter));
                match count {
                    Some(count) => format!("The scope is now {} ({count})", lowercase_first(&filter.title())),
                    None => format!("The scope is now {}", lowercase_first(&filter.title())),
                }
            }
            Scope::Photos { paths, .. } => format!("The scope is now the {} selected photos", paths.len()),
        };
        tracing::info!(scope = scope.title(), "scope set");
        gallery.say(&told);
        *self.imp().scope.borrow_mut() = Some(scope);
    }

    pub fn scope(&self) -> Option<Scope> {
        self.imp().scope.borrow().clone()
    }

    pub fn visible_view(&self) -> String {
        self.imp()
            .stack
            .visible_child_name()
            .map(|name| name.to_string())
            .unwrap_or_default()
    }

    pub fn show_view(&self, name: &str) -> bool {
        let stack = &self.imp().stack;
        if stack.child_by_name(name).is_none() {
            return false;
        }
        stack.set_visible_child_name(name);
        true
    }

    fn setup_actions(&self) {
        let show_view = gtk::gio::ActionEntry::builder("show-view")
            .parameter_type(Some(glib::VariantTy::STRING))
            .activate(|window: &Window, _, parameter| {
                let Some(name) = parameter.and_then(|value| value.str()) else {
                    return;
                };
                if !window.show_view(name) {
                    tracing::warn!(view = name, "no such view");
                }
            })
            .build();
        let show_photos = gtk::gio::ActionEntry::builder("show-photos")
            .parameter_type(Some(glib::VariantTy::STRING))
            .activate(|window: &Window, _, parameter| {
                let Some(text) = parameter.and_then(|value| value.str()) else {
                    return;
                };
                match text.parse::<Filter>() {
                    Ok(filter) => window.show_photos(filter),
                    Err(why) => tracing::warn!(why, "no such set of photos"),
                }
            })
            .build();
        let sort = gtk::gio::ActionEntry::builder("gallery-sort")
            .parameter_type(Some(glib::VariantTy::STRING))
            .activate(|window: &Window, _, parameter| {
                match parameter.and_then(|value| value.str()).and_then(Order::named) {
                    Some(order) => window.imp().gallery.set_order(order),
                    None => tracing::warn!("no such order"),
                }
            })
            .build();
        let gap = gtk::gio::ActionEntry::builder("gallery-gap")
            .parameter_type(Some(glib::VariantTy::STRING))
            .activate(|window: &Window, _, parameter| {
                let Some(key) = parameter.and_then(|value| value.str()) else {
                    return;
                };
                match (key, Gap::ALL.into_iter().find(|gap| gap.key() == key)) {
                    ("none", _) => window.imp().gallery.set_gap(None),
                    (_, Some(gap)) => window.imp().gallery.set_gap(Some(gap)),
                    _ => tracing::warn!(key, "no such field"),
                }
            })
            .build();
        let place = gtk::gio::ActionEntry::builder("gallery-place")
            .parameter_type(Some(glib::VariantTy::STRING))
            .activate(|window: &Window, _, parameter| {
                if let Some(folder) = parameter.and_then(|value| value.str()) {
                    window.imp().gallery.choose_place(folder);
                }
            })
            .build();
        let tag = gtk::gio::ActionEntry::builder("gallery-tag")
            .parameter_type(Some(glib::VariantTy::STRING))
            .activate(|window: &Window, _, parameter| {
                if let Some(path) = parameter.and_then(|value| value.str()) {
                    window.imp().gallery.choose_tag(path);
                }
            })
            .build();
        let gallery_all = gtk::gio::ActionEntry::builder("gallery-select-all")
            .activate(|window: &Window, _, _| window.imp().gallery.select_all())
            .build();
        let gallery_none = gtk::gio::ActionEntry::builder("gallery-select-none")
            .activate(|window: &Window, _, _| window.imp().gallery.select_none())
            .build();
        let use_as_scope = gtk::gio::ActionEntry::builder("use-as-scope")
            .activate(|window: &Window, _, _| window.use_as_scope())
            .build();
        let scan = gtk::gio::ActionEntry::builder("scan")
            .activate(|window: &Window, _, _| window.imp().dashboard.scan(Mode::Reconcile))
            .build();
        let fill = gtk::gio::ActionEntry::builder("fill-thumbnails")
            .activate(|window: &Window, _, _| window.imp().dashboard.fill_thumbnails())
            .build();
        let places = gtk::gio::ActionEntry::builder("get-places")
            .activate(|window: &Window, _, _| window.imp().dashboard.get_places())
            .build();
        let cancel = gtk::gio::ActionEntry::builder("cancel-scan")
            .activate(|window: &Window, _, _| window.imp().dashboard.cancel())
            .build();
        let select_all = gtk::gio::ActionEntry::builder("preview-select-all")
            .activate(|window: &Window, _, _| window.preview().select_all())
            .build();
        let select_none = gtk::gio::ActionEntry::builder("preview-select-none")
            .activate(|window: &Window, _, _| window.preview().select_none())
            .build();
        let details = gtk::gio::ActionEntry::builder("preview-details")
            .parameter_type(Some(glib::VariantTy::INT32))
            .activate(|window: &Window, _, parameter| {
                let Some(index) = parameter.and_then(|value| value.get::<i32>()) else {
                    return;
                };
                window.preview().details(index.max(0) as usize);
            })
            .build();
        let apply = gtk::gio::ActionEntry::builder("apply-change-set")
            .activate(|window: &Window, _, _| window.preview().apply())
            .build();
        let stop = gtk::gio::ActionEntry::builder("cancel-apply")
            .activate(|window: &Window, _, _| window.preview().cancel())
            .build();
        let undo = gtk::gio::ActionEntry::builder("undo-last")
            .activate(|window: &Window, _, _| {
                let gallery = &window.imp().gallery;
                match window.visible_view() == "gallery" && gallery.photo_open() {
                    true => gallery.photo().undo(),
                    false => window.preview().undo(),
                }
            })
            .build();
        let show_photo = gtk::gio::ActionEntry::builder("show-photo")
            .parameter_type(Some(glib::VariantTy::STRING))
            .activate(|window: &Window, _, parameter| {
                let Some(path) = parameter.and_then(|value| value.str()) else {
                    return;
                };
                window.show_view("gallery");
                if !window.imp().gallery.show_photo(path) {
                    tracing::warn!(photo = path, "the gallery does not show that photo");
                }
            })
            .build();
        let photo = |name: &str, act: fn(&crate::photo::PhotoPage)| {
            gtk::gio::ActionEntry::builder(name)
                .activate(move |window: &Window, _, _| {
                    let gallery = &window.imp().gallery;
                    if gallery.photo_open() {
                        act(&gallery.photo());
                    }
                })
                .build()
        };
        let photo_close = gtk::gio::ActionEntry::builder("photo-close")
            .activate(|window: &Window, _, _| window.imp().gallery.close_photo())
            .build();
        let photo_edit = gtk::gio::ActionEntry::builder("photo-edit")
            .state(false.to_variant())
            .activate(|window: &Window, action, _| {
                let gallery = &window.imp().gallery;
                match gallery.photo_open() {
                    true => gallery.photo().toggle_editing(),
                    false => action.set_state(&false.to_variant()),
                }
            })
            .build();
        self.add_action_entries([
            show_view,
            show_photos,
            sort,
            gap,
            place,
            tag,
            gallery_all,
            gallery_none,
            use_as_scope,
            scan,
            fill,
            places,
            cancel,
            select_all,
            select_none,
            details,
            apply,
            stop,
            undo,
            show_photo,
            photo("photo-next", |page| page.step(1)),
            photo("photo-previous", |page| page.step(-1)),
            photo("photo-first", |page| page.first()),
            photo("photo-last", |page| page.last()),
            photo("photo-panel", |page| page.toggle_panel()),
            photo("photo-open-with", |page| page.open_with()),
            photo("photo-show-map", |page| page.show_map()),
            photo("photo-review", |page| page.review()),
            photo("photo-apply", |page| page.apply()),
            photo_close,
            photo_edit,
        ]);
        for name in ["photo-review", "photo-apply"] {
            if let Some(action) = self.lookup_action(name).and_downcast::<gtk::gio::SimpleAction>() {
                action.set_enabled(false);
            }
        }
    }
}

fn lowercase_first(text: &str) -> String {
    let mut chars = text.chars();
    match chars.next() {
        Some(first) => first.to_lowercase().chain(chars).collect(),
        None => String::new(),
    }
}
