use std::rc::Rc;

use adw::prelude::*;
use adw::subclass::prelude::*;
use gtk::glib;
use gtk::glib::subclass::InitializingObject;
use photomanager_core::filter::Filter;
use photomanager_core::scan::Mode;

use crate::dashboard::Dashboard;
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
        pub gallery: TemplateChild<adw::StatusPage>,
        pub library: std::cell::RefCell<Option<Rc<Library>>>,
        /// The photos last handed to the gallery, and how many there were.
        pub shown: std::cell::RefCell<Option<(Filter, Option<i64>)>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for Window {
        const NAME: &'static str = "PmWindow";
        type Type = super::Window;
        type ParentType = adw::ApplicationWindow;

        fn class_init(klass: &mut Self::Class) {
            Dashboard::ensure_type();
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
        *self.imp().library.borrow_mut() = library;
    }

    pub fn library(&self) -> Option<Rc<Library>> {
        self.imp().library.borrow().clone()
    }

    pub fn dashboard(&self) -> Dashboard {
        self.imp().dashboard.clone()
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
        let count = self.library().and_then(|library| library.count(&filter));
        let gallery = &self.imp().gallery;
        gallery.set_title(&filter.title());
        let noun = match filter.is_of_files() {
            true => ("file", "files"),
            false => ("photo", "photos"),
        };
        gallery.set_description(Some(&match count {
            Some(1) => format!("1 {}", noun.0),
            Some(count) => format!("{count} {}", noun.1),
            None => "The library is busy, try again in a moment.".to_string(),
        }));
        tracing::info!(filter = %filter, count, "photos shown");
        *self.imp().shown.borrow_mut() = Some((filter, count));
        self.show_view("gallery");
    }

    pub fn shown(&self) -> Option<(Filter, Option<i64>)> {
        self.imp().shown.borrow().clone()
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
            .activate(|window: &Window, _, _| window.preview().undo())
            .build();
        self.add_action_entries([
            show_view,
            show_photos,
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
        ]);
    }
}
