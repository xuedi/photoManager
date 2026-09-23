use std::cell::RefCell;
use std::rc::Rc;

use adw::prelude::*;
use adw::subclass::prelude::*;
use gtk::glib;
use gtk::glib::subclass::InitializingObject;
use photomanager_core::filter::{Filter, Gap};
use photomanager_core::scan::Mode;
use photomanager_core::survey::{Measure, Place, Survey};

use crate::library::{Event, Library};

const SHOW_PHOTOS: &str = "win.show-photos";

mod imp {
    use super::*;

    #[derive(Debug, Default, gtk::CompositeTemplate)]
    #[template(resource = "/org/beijingcode/PhotoManager/dashboard.ui")]
    pub struct Dashboard {
        #[template_child]
        pub pages: TemplateChild<gtk::Stack>,
        #[template_child]
        pub empty_slot: TemplateChild<gtk::Box>,
        #[template_child]
        pub filled_slot: TemplateChild<gtk::Box>,
        #[template_child]
        pub controls: TemplateChild<gtk::Box>,
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
        pub glance: TemplateChild<gtk::FlowBox>,
        #[template_child]
        pub coverage: TemplateChild<adw::PreferencesGroup>,
        #[template_child]
        pub places: TemplateChild<adw::PreferencesGroup>,
        #[template_child]
        pub field: TemplateChild<gtk::DropDown>,
        #[template_child]
        pub tidy: TemplateChild<adw::PreferencesGroup>,
        #[template_child]
        pub data: TemplateChild<adw::PreferencesGroup>,
        pub library: RefCell<Option<Rc<Library>>>,
        pub survey: RefCell<Option<Rc<Survey>>>,
        /// The rows each group was given, so they can be taken out again.
        pub rows: RefCell<Vec<(adw::PreferencesGroup, gtk::Widget)>>,
        pub place_rows: RefCell<Vec<gtk::Widget>>,
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

    impl ObjectImpl for Dashboard {
        fn constructed(&self) {
            self.parent_constructed();
            let titles: Vec<&str> = Gap::ALL.iter().map(|gap| gap.title()).collect();
            self.field.set_model(Some(&gtk::StringList::new(&titles)));
            let dashboard = self.obj().clone();
            self.field.connect_selected_notify(move |_| dashboard.show_places());
        }
    }

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
        self.refresh();
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
            library.cancel();
            self.imp().progress.set_text(Some("Stopping"));
        }
    }

    /// The field the gaps by country and event are shown for.
    pub fn field(&self) -> Gap {
        Gap::ALL
            .get(self.imp().field.selected() as usize)
            .copied()
            .unwrap_or(Gap::Gps)
    }

    pub fn set_field(&self, gap: Gap) {
        let at = Gap::ALL.iter().position(|each| *each == gap).unwrap_or(0);
        self.imp().field.set_selected(at as u32);
    }

    /// The countries as listed, in order: the ones with the most missing first.
    pub fn listed_places(&self) -> Vec<String> {
        self.imp()
            .place_rows
            .borrow()
            .iter()
            .filter_map(|row| row.downcast_ref::<adw::PreferencesRow>())
            .map(|row| row.title().to_string())
            .collect()
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
                self.refresh();
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
                self.show_data();
                tracing::info!(
                    made = done.made,
                    missing = done.missing,
                    failed = done.failed,
                    seconds = done.seconds,
                    "thumbnails filled in"
                );
            }
            Event::Note(line) => progress.set_text(Some(&line)),
            Event::Previewed(_) | Event::Applied(_, _) => {}
            Event::Places(imported) => {
                self.running(false);
                self.show_data();
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
                self.refresh();
                self.imp().progress.set_visible(true);
                self.imp().progress.set_fraction(0.0);
                self.imp().progress.set_text(Some(&format!("Did not work: {why}")));
                tracing::error!(why, "the last thing asked for failed");
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

    /// Shows what is known right away and asks for a new survey, which fills the page when it
    /// arrives.
    fn refresh(&self) {
        let Some(library) = self.imp().library.borrow().clone() else {
            self.show_page(false);
            return;
        };
        self.show_page(library.counts().photos > 0);
        self.show_data();

        let dashboard = self.clone();
        library.resurvey(move |taken| match taken {
            Ok(survey) => dashboard.show(survey),
            Err(why) => tracing::error!(why, "the library could not be surveyed"),
        });
    }

    fn show_page(&self, filled: bool) {
        let imp = self.imp();
        let (page, slot) = match filled {
            true => ("filled", &imp.filled_slot),
            false => ("empty", &imp.empty_slot),
        };
        let controls = imp.controls.get();
        if controls.parent().as_ref() != Some(slot.upcast_ref()) {
            if let Some(parent) = controls.parent().and_downcast::<gtk::Box>() {
                parent.remove(&controls);
            }
            slot.append(&controls);
        }
        imp.pages.set_visible_child_name(page);
    }

    fn show(&self, survey: Rc<Survey>) {
        self.show_page(survey.photos > 0);
        *self.imp().survey.borrow_mut() = Some(survey.clone());
        self.show_glance(&survey);
        self.show_coverage(&survey);
        self.show_places();
        self.show_tidy(&survey);
    }

    fn show_glance(&self, survey: &Survey) {
        let glance = &self.imp().glance;
        glance.remove_all();

        let years = match (survey.first.as_deref(), survey.last.as_deref()) {
            (Some(first), Some(last)) => (
                format!("{} - {}", &first[..4.min(first.len())], &last[..4.min(last.len())]),
                format!("from {first} to {last}"),
            ),
            _ => ("none".to_string(), "no photo carries a date".to_string()),
        };
        let camera = match survey.cameras.iter().find(|(model, _)| model.is_some()) {
            Some((Some(model), _)) => format!("cameras, most photos from {model}"),
            _ => "cameras".to_string(),
        };
        let types = survey
            .file_types
            .iter()
            .map(|(extension, count)| match extension.is_empty() {
                true => format!("none {count}"),
                false => format!("{extension} {count}"),
            })
            .collect::<Vec<_>>()
            .join(", ");

        for (value, caption) in [
            (survey.photos.to_string(), "photos".to_string()),
            (survey.events.to_string(), "events".to_string()),
            (gigabytes(survey.bytes), "on disk".to_string()),
            years,
            (survey.camera_count().to_string(), camera),
            (survey.file_types.len().to_string(), format!("file types: {types}")),
        ] {
            glance.append(&tile(&value, &caption));
        }
    }

    fn show_coverage(&self, survey: &Survey) {
        let group = self.imp().coverage.get();
        self.clear(&group);
        for (gap, measure) in &survey.coverage {
            let subtitle = match gap {
                Gap::DateOffFolder => format!(
                    "disagrees on {} of {} dated photos in a dated folder",
                    measure.missing, measure.of
                ),
                _ => format!("missing on {} of {}", measure.missing, measure.of),
            };
            let row = adw::ActionRow::builder().title(gap.title()).subtitle(subtitle).build();
            let bar = gtk::LevelBar::builder()
                .value(measure.present())
                .valign(gtk::Align::Center)
                .width_request(96)
                .build();
            bar.update_property(&[gtk::accessible::Property::Label(&format!("{} coverage", gap.title()))]);
            row.add_suffix(&bar);
            clickable(&row, measure.missing, &Filter::missing(*gap));
            self.keep(&group, row.upcast());
        }
    }

    /// The countries, ordered by what they miss of the chosen field, their events inside.
    fn show_places(&self) {
        let imp = self.imp();
        for row in imp.place_rows.borrow_mut().drain(..) {
            imp.places.remove(&row);
        }
        let Some(survey) = imp.survey.borrow().clone() else {
            return;
        };
        let gap = self.field();

        for country in by_gap(&survey.countries, gap) {
            let measure = country.gap(gap);
            let events: Vec<&Place> = by_gap(&country.events, gap)
                .into_iter()
                .filter(|event| event.gap(gap).missing > 0)
                .collect();
            let row: gtk::Widget = match events.is_empty() {
                true => {
                    let row = adw::ActionRow::builder()
                        .title(glib::markup_escape_text(&country.name))
                        .subtitle(so_far(gap, measure))
                        .build();
                    clickable(&row, measure.missing, &country.filter(gap));
                    row.upcast()
                }
                false => {
                    let row = adw::ExpanderRow::builder()
                        .title(glib::markup_escape_text(&country.name))
                        .subtitle(so_far(gap, measure))
                        .build();
                    let show = gtk::Button::builder()
                        .icon_name("go-next-symbolic")
                        .tooltip_text("Show These Photos")
                        .valign(gtk::Align::Center)
                        .action_name(SHOW_PHOTOS)
                        .action_target(&country.filter(gap).to_string().to_variant())
                        .build();
                    show.add_css_class("flat");
                    show.update_property(&[gtk::accessible::Property::Label(&format!(
                        "Show the photos of {}",
                        country.name
                    ))]);
                    row.add_suffix(&show);
                    for event in events {
                        let inner = adw::ActionRow::builder()
                            .title(glib::markup_escape_text(&event.name))
                            .subtitle(so_far(gap, event.gap(gap)))
                            .build();
                        clickable(&inner, event.gap(gap).missing, &event.filter(gap));
                        row.add_row(&inner);
                    }
                    row.upcast()
                }
            };
            imp.places.add(&row);
            imp.place_rows.borrow_mut().push(row);
        }
    }

    fn show_tidy(&self, survey: &Survey) {
        let group = self.imp().tidy.get();
        self.clear(&group);
        group.set_visible(!survey.tidy.is_empty());
        for finding in &survey.tidy {
            let row = adw::ActionRow::builder()
                .title(glib::markup_escape_text(&finding.title))
                .subtitle(glib::markup_escape_text(&finding.detail))
                .build();
            let count = gtk::Label::builder().label(finding.count.to_string()).build();
            count.add_css_class("dim-label");
            row.add_suffix(&count);
            clickable(&row, finding.count, &finding.filter);
            self.keep(&group, row.upcast());
        }
    }

    /// What is not about the photos but about what the application keeps next to them.
    fn show_data(&self) {
        let imp = self.imp();
        let group = imp.data.get();
        self.clear(&group);
        let Some(library) = imp.library.borrow().clone() else {
            return;
        };
        let counts = library.counts();
        imp.fill_button
            .set_visible(!library.is_scanning() && counts.photos > 0 && counts.thumbnails < counts.photos);

        let places = library
            .dump_date()
            .filter(|_| counts.places > 0)
            .map(|date| format!("from the GeoNames dumps of {date}"))
            .unwrap_or_else(|| "not fetched yet".to_string());
        for (title, value, subtitle) in [
            ("Thumbnails", counts.thumbnails, "made while scanning".to_string()),
            ("Places", counts.places, places),
        ] {
            let row = adw::ActionRow::builder().title(title).subtitle(subtitle).build();
            let label = gtk::Label::builder().label(value.to_string()).build();
            label.add_css_class("dim-label");
            row.add_suffix(&label);
            self.keep(&group, row.upcast());
        }
    }

    fn keep(&self, group: &adw::PreferencesGroup, row: gtk::Widget) {
        group.add(&row);
        self.imp().rows.borrow_mut().push((group.clone(), row));
    }

    fn clear(&self, group: &adw::PreferencesGroup) {
        self.imp().rows.borrow_mut().retain(|(owner, row)| {
            if owner == group {
                group.remove(row);
                return false;
            }
            true
        });
    }
}

/// A row that shows its photos when activated, if it has any.
fn clickable(row: &adw::ActionRow, count: i64, filter: &Filter) {
    if count == 0 {
        return;
    }
    row.set_activatable(true);
    row.set_action_name(Some(SHOW_PHOTOS));
    row.set_action_target_value(Some(&filter.to_string().to_variant()));
    row.add_suffix(&gtk::Image::from_icon_name("go-next-symbolic"));
}

fn by_gap(places: &[Place], gap: Gap) -> Vec<&Place> {
    let mut sorted: Vec<&Place> = places.iter().collect();
    sorted.sort_by(|a, b| {
        b.gap(gap)
            .missing
            .cmp(&a.gap(gap).missing)
            .then_with(|| a.name.cmp(&b.name))
    });
    sorted
}

fn so_far(gap: Gap, measure: Measure) -> String {
    match (gap, measure.missing) {
        (Gap::DateOffFolder, 0) if measure.of == 0 => "no dated photo in a dated folder".to_string(),
        (_, 0) => "nothing missing".to_string(),
        (Gap::DateOffFolder, missing) => format!("{missing} of {} disagree with the folder", measure.of),
        (_, missing) => format!("{missing} of {} {}", measure.of, gap.lacking()),
    }
}

fn tile(value: &str, caption: &str) -> gtk::Widget {
    let card = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(4)
        .build();
    card.add_css_class("card");
    let inner = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(4)
        .margin_top(12)
        .margin_bottom(12)
        .margin_start(12)
        .margin_end(12)
        .build();
    let big = gtk::Label::builder().label(value).xalign(0.0).build();
    big.add_css_class("title-2");
    let small = gtk::Label::builder()
        .label(caption)
        .xalign(0.0)
        .wrap(true)
        .wrap_mode(gtk::pango::WrapMode::WordChar)
        .build();
    small.add_css_class("dim-label");
    small.add_css_class("caption");
    inner.append(&big);
    inner.append(&small);
    card.append(&inner);
    card.update_property(&[gtk::accessible::Property::Label(&format!("{value} {caption}"))]);
    card.upcast()
}

fn gigabytes(bytes: i64) -> String {
    let bytes = bytes as f64;
    match bytes {
        b if b >= 100_000_000.0 => format!("{:.1} GB", b / 1_000_000_000.0),
        b if b >= 100_000.0 => format!("{:.1} MB", b / 1_000_000.0),
        b => format!("{:.0} kB", b / 1_000.0),
    }
}
