use std::cell::RefCell;
use std::rc::Rc;

use adw::prelude::*;
use adw::subclass::prelude::*;
use gtk::glib;
use gtk::glib::subclass::InitializingObject;
use photomanager_core::scan::Mode;

use crate::library::{Event, Library};

mod imp {
    use super::*;

    #[derive(Debug, Default, gtk::CompositeTemplate)]
    #[template(resource = "/org/beijingcode/PhotoManager/dashboard.ui")]
    pub struct Dashboard {
        #[template_child]
        pub scan_button: TemplateChild<gtk::Button>,
        #[template_child]
        pub fill_button: TemplateChild<gtk::Button>,
        #[template_child]
        pub places_button: TemplateChild<gtk::Button>,
        #[template_child]
        pub cancel_button: TemplateChild<gtk::Button>,
        #[template_child]
        pub progress: TemplateChild<gtk::ProgressBar>,
        #[template_child]
        pub counts: TemplateChild<adw::PreferencesGroup>,
        pub library: RefCell<Option<Rc<Library>>>,
        pub rows: RefCell<Vec<adw::ActionRow>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for Dashboard {
        const NAME: &'static str = "PmDashboard";
        type Type = super::Dashboard;
        type ParentType = adw::Bin;

        fn class_init(klass: &mut Self::Class) {
            klass.bind_template();
        }

        fn instance_init(obj: &InitializingObject<Self>) {
            obj.init_template();
        }
    }

    impl ObjectImpl for Dashboard {}
    impl WidgetImpl for Dashboard {}
    impl BinImpl for Dashboard {}
}

glib::wrapper! {
    pub struct Dashboard(ObjectSubclass<imp::Dashboard>)
        @extends adw::Bin, gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl Default for Dashboard {
    fn default() -> Self {
        glib::Object::builder().build()
    }
}

impl Dashboard {
    pub fn set_library(&self, library: Option<Rc<Library>>) {
        self.imp().scan_button.set_sensitive(library.is_some());
        *self.imp().library.borrow_mut() = library;
        self.show_counts();
    }

    pub fn scan(&self, mode: Mode) {
        let Some(library) = self.imp().library.borrow().clone() else {
            return;
        };
        if library.is_scanning() {
            return;
        }
        self.running(true);
        self.imp().progress.set_fraction(0.0);
        self.imp().progress.set_text(Some("Looking for photos"));

        let dashboard = self.clone();
        library.scan(mode, move |event| dashboard.report(event));
    }

    pub fn fill_thumbnails(&self) {
        let Some(library) = self.imp().library.borrow().clone() else {
            return;
        };
        if library.is_scanning() {
            return;
        }
        self.running(true);
        self.imp().progress.set_fraction(0.0);
        self.imp().progress.set_text(Some("Looking for thumbnails"));

        let dashboard = self.clone();
        library.fill_thumbnails(move |event| dashboard.report(event));
    }

    /// Fetches the place data. Nothing else here touches the network.
    pub fn get_places(&self) {
        let Some(library) = self.imp().library.borrow().clone() else {
            return;
        };
        if library.is_scanning() {
            return;
        }
        self.running(true);
        self.imp().progress.set_fraction(0.0);
        self.imp().progress.set_text(Some("Asking GeoNames"));

        let dashboard = self.clone();
        library.get_places(move |event| dashboard.report(event));
    }

    pub fn cancel(&self) {
        if let Some(library) = self.imp().library.borrow().as_ref() {
            library.cancel_scan();
            self.imp().progress.set_text(Some("Stopping"));
        }
    }

    fn report(&self, event: Event) {
        let progress = &self.imp().progress;
        match event {
            Event::Counted(total) => progress.set_text(Some(&format!("{total} photos to look at"))),
            Event::Done(done, total) => {
                progress.set_fraction(done as f64 / total.max(1) as f64);
                progress.set_text(Some(&format!("{done} of {total}")));
            }
            Event::Finished(summary) => {
                self.running(false);
                self.show_counts();
                tracing::info!(
                    photos = summary.photos,
                    read = summary.read,
                    issues = summary.issues,
                    seconds = summary.seconds,
                    "scan finished"
                );
            }
            Event::Filled(done) => {
                self.running(false);
                self.show_counts();
                tracing::info!(
                    made = done.made,
                    missing = done.missing,
                    failed = done.failed,
                    seconds = done.seconds,
                    "thumbnails filled in"
                );
            }
            Event::Note(line) => progress.set_text(Some(&line)),
            Event::Places(imported) => {
                self.running(false);
                self.show_counts();
                tracing::info!(
                    places = imported.places,
                    names = imported.names,
                    countries = imported.countries,
                    seconds = imported.seconds,
                    "place data imported"
                );
            }
            Event::Failed(why) => {
                self.running(false);
                self.show_counts();
                tracing::error!(why, "scan failed");
            }
        }
    }

    fn running(&self, busy: bool) {
        self.imp().scan_button.set_visible(!busy);
        self.imp().cancel_button.set_visible(busy);
        self.imp().progress.set_visible(busy);
        self.imp().places_button.set_visible(!busy);
        if busy {
            self.imp().fill_button.set_visible(false);
        }
    }

    fn show_counts(&self) {
        let imp = self.imp();
        for row in imp.rows.borrow_mut().drain(..) {
            imp.counts.remove(&row);
        }
        let Some(library) = imp.library.borrow().clone() else {
            return;
        };

        let counts = library.counts();
        if counts.photos == 0 {
            imp.counts.set_visible(false);
            return;
        }
        imp.fill_button.set_visible(counts.thumbnails < counts.photos);
        let mut rows = vec![
            ("Photos".to_string(), counts.photos),
            ("Events".to_string(), counts.events),
            ("Thumbnails".to_string(), counts.thumbnails),
            ("Places".to_string(), counts.places),
        ];
        rows.extend(
            library
                .issue_counts()
                .into_iter()
                .map(|(kind, count)| (capitalised(&kind), count)),
        );

        let dumps = library.dump_date();
        for (title, count) in rows {
            let mut row = adw::ActionRow::builder().title(&title);
            if title == "Places"
                && let Some(date) = dumps.as_deref()
            {
                row = row.subtitle(format!("GeoNames dumps of {date}"));
            }
            let row = row.build();
            let value = gtk::Label::builder().label(count.to_string()).build();
            value.add_css_class("dim-label");
            row.add_suffix(&value);
            imp.counts.add(&row);
            imp.rows.borrow_mut().push(row);
        }
        imp.counts.set_visible(true);
    }
}

fn capitalised(text: &str) -> String {
    let mut characters = text.chars();
    match characters.next() {
        Some(first) => first.to_uppercase().collect::<String>() + characters.as_str(),
        None => String::new(),
    }
}
