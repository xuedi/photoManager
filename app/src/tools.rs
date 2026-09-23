//! The tools view: the list of tools, and the preview a tool pushes when it has decided what it
//! would change. The list itself is still empty - the tools come later - but the way from a tool
//! to a photo runs through here and nowhere else.

use std::rc::Rc;

use adw::prelude::*;
use adw::subclass::prelude::*;
use gtk::glib;
use gtk::glib::subclass::InitializingObject;
use photomanager_core::changeset::{ChangeSet, Wanted};

use crate::library::{Event, Library};
use crate::preview::Preview;

mod imp {
    use super::*;
    use std::cell::RefCell;

    #[derive(Debug, Default, gtk::CompositeTemplate)]
    #[template(resource = "/org/beijingcode/PhotoManager/tools.ui")]
    pub struct Tools {
        #[template_child]
        pub nav: TemplateChild<adw::NavigationView>,
        #[template_child]
        pub tools_box: TemplateChild<gtk::Box>,
        #[template_child]
        pub preview: TemplateChild<Preview>,
        pub library: RefCell<Option<Rc<Library>>>,
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
            #[cfg(feature = "devtools")]
            self.obj().add_development_buttons();
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
        *self.imp().library.borrow_mut() = library;
    }

    pub fn preview(&self) -> Preview {
        self.imp().preview.clone()
    }

    /// What a tool asks for: work out what would change, then show it. Nothing is written.
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
        self.imp().nav.push_by_tag("preview");
    }

    pub fn showing(&self) -> String {
        self.imp()
            .nav
            .visible_page()
            .and_then(|page| page.tag())
            .map(|tag| tag.to_string())
            .unwrap_or_default()
    }

    /// A handful of photos and a rating, so the preview and the apply can be driven before the
    /// first real tool exists. Development builds only.
    #[cfg(feature = "devtools")]
    fn add_development_buttons(&self) {
        let demo = gtk::Button::builder()
            .label("Demo Change Set")
            .action_name("win.preview-demo")
            .build();
        demo.add_css_class("pill");
        demo.update_property(&[gtk::accessible::Property::Label("Demo Change Set")]);

        let undo = gtk::Button::builder()
            .label("Undo the Last Change")
            .action_name("win.undo-last")
            .build();
        undo.add_css_class("pill");
        undo.update_property(&[gtk::accessible::Property::Label("Undo the Last Change")]);

        let row = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(12)
            .halign(gtk::Align::Center)
            .build();
        row.append(&demo);
        row.append(&undo);
        self.imp().tools_box.append(&row);
    }
}
