use std::rc::Rc;

use adw::prelude::*;
use adw::subclass::prelude::*;
use gtk::glib;
use gtk::glib::subclass::InitializingObject;
use photomanager_core::scan::Mode;

use crate::dashboard::Dashboard;
use crate::library::Library;

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
    }

    #[glib::object_subclass]
    impl ObjectSubclass for Window {
        const NAME: &'static str = "PmWindow";
        type Type = super::Window;
        type ParentType = adw::ApplicationWindow;

        fn class_init(klass: &mut Self::Class) {
            Dashboard::ensure_type();
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
        self.imp().dashboard.set_library(library);
    }

    pub fn scan(&self, mode: Mode) {
        self.imp().dashboard.scan(mode);
    }

    pub fn fill_thumbnails(&self) {
        self.imp().dashboard.fill_thumbnails();
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
        let scan = gtk::gio::ActionEntry::builder("scan")
            .activate(|window: &Window, _, _| window.imp().dashboard.scan(Mode::Reconcile))
            .build();
        let fill = gtk::gio::ActionEntry::builder("fill-thumbnails")
            .activate(|window: &Window, _, _| window.imp().dashboard.fill_thumbnails())
            .build();
        let cancel = gtk::gio::ActionEntry::builder("cancel-scan")
            .activate(|window: &Window, _, _| window.imp().dashboard.cancel())
            .build();
        self.add_action_entries([show_view, scan, fill, cancel]);
    }
}
