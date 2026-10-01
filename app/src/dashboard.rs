use std::cell::{Cell, RefCell};
use std::rc::Rc;

use adw::prelude::*;
use adw::subclass::prelude::*;
use gtk::glib;
use gtk::glib::subclass::InitializingObject;
use photomanager_core::filter::{Filter, Gap, Kind};
use photomanager_core::remedy::Remedy;
use photomanager_core::scan::Mode;
use photomanager_core::survey::{Aligned, Measure, Place, Survey};
use photomanager_core::upkeep::{self, Facts, Job, Status};

use crate::library::{Event, Library};
use crate::upkeep::{Now, Segment};

const SHOW_PHOTOS: &str = "win.show-photos";

mod imp {
    use super::*;

    #[derive(Debug, Default, gtk::CompositeTemplate)]
    #[template(resource = "/org/beijingcode/PhotoManager/dashboard.ui")]
    pub struct Dashboard {
        #[template_child]
        pub toasts: TemplateChild<adw::ToastOverlay>,
        #[template_child]
        pub pages: TemplateChild<gtk::Stack>,
        #[template_child]
        pub empty_slot: TemplateChild<gtk::Box>,
        #[template_child]
        pub filled_slot: TemplateChild<gtk::Box>,
        #[template_child]
        pub controls: TemplateChild<gtk::Box>,
        #[template_child]
        pub bar: TemplateChild<gtk::Box>,
        #[template_child]
        pub busy: TemplateChild<gtk::Box>,
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
        pub suggested: TemplateChild<adw::PreferencesGroup>,
        #[template_child]
        pub suggested_row: TemplateChild<adw::ActionRow>,
        pub library: RefCell<Option<Rc<Library>>>,
        pub survey: RefCell<Option<Rc<Survey>>>,
        /// The rows each group was given, so they can be taken out again.
        pub rows: RefCell<Vec<(adw::PreferencesGroup, gtk::Widget)>>,
        pub place_rows: RefCell<Vec<gtk::Widget>>,
        pub said: RefCell<String>,
        pub segments: RefCell<Vec<Segment>>,
        pub statuses: RefCell<Vec<Status>>,
        /// Which upkeep is the newest asked for, so an older one that arrives late is dropped.
        pub upkeeps: Cell<u64>,
        pub running: Cell<Option<Job>>,
        /// The job that failed last in this session, and why.
        pub failed: RefCell<Option<(Job, String)>>,
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

            for job in Job::ALL {
                let segment = Segment::new(job);
                self.bar.append(segment.widget());
                self.segments.borrow_mut().push(segment);
            }
            self.obj().show_bar();
            // Something may have changed in the library while the dashboard was not looked at.
            self.obj().connect_map(|dashboard| dashboard.show_upkeep());
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
        self.start(Job::Scan, "Looking for photos");

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
        self.start(Job::Thumbnails, "Looking for thumbnails");

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
        self.start(Job::Places, "Asking GeoNames");

        let dashboard = self.clone();
        library.get_places(move |event| dashboard.report(event));
    }

    /// Reads who is in the photos from Immich, with the key from the keyring. Immich is only
    /// read.
    pub fn get_people(&self) {
        let Some(library) = self.imp().library.borrow().clone() else {
            return;
        };
        if library.is_scanning() {
            return;
        }
        if library.immich_address().is_none() {
            self.ask_for_preferences("Set where Immich is and its API key first");
            return;
        }
        self.start(Job::People, "Asking Immich");

        let dashboard = self.clone();
        gtk::glib::spawn_future_local(async move {
            match crate::secrets::immich_key().await {
                Ok(Some(key)) => {
                    let reporter = dashboard.clone();
                    library.get_people(key, move |event| reporter.report(event));
                }
                Ok(None) => {
                    dashboard.stop();
                    dashboard.ask_for_preferences("There is no Immich API key yet");
                }
                Err(why) => dashboard.report(Event::Failed(why)),
            }
        });
    }

    fn ask_for_preferences(&self, text: &str) {
        let toast = adw::Toast::builder()
            .title(text)
            .button_label("Preferences")
            .action_name("app.preferences")
            .build();
        self.say_toast(toast);
    }

    fn say_toast(&self, toast: adw::Toast) {
        *self.imp().said.borrow_mut() = toast.title().map(|title| title.to_string()).unwrap_or_default();
        self.imp().toasts.add_toast(toast);
    }

    /// What the last toast said.
    pub fn said(&self) -> String {
        self.imp().said.borrow().clone()
    }

    pub fn cancel(&self) {
        if let Some(library) = self.imp().library.borrow().as_ref() {
            library.cancel();
            self.imp().progress.set_text(Some("Stopping"));
        }
    }

    /// The line that opens the suggestions, while there are any.
    pub fn set_suggestions(&self, count: usize) {
        let imp = self.imp();
        imp.suggested.set_visible(count > 0);
        imp.suggested_row.set_title(&match count {
            1 => "1 Suggestion".to_string(),
            count => format!("{count} Suggestions"),
        });
    }

    /// What the suggestions line says, while it is shown.
    pub fn suggestions_line(&self) -> Option<String> {
        let imp = self.imp();
        imp.suggested
            .get_visible()
            .then(|| imp.suggested_row.title().to_string())
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
                self.stop();
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
                self.stop();
                self.show_upkeep();
                tracing::info!(
                    made = done.made,
                    missing = done.missing,
                    failed = done.failed,
                    seconds = done.seconds,
                    "thumbnails filled in"
                );
            }
            Event::Note(line) => progress.set_text(Some(&line)),
            Event::Previewed(_) | Event::Applied(_) | Event::Fixed(_) | Event::Doubted(..) => {}
            Event::Places(imported) => {
                self.stop();
                self.show_upkeep();
                tracing::info!(
                    places = imported.places,
                    names = imported.names,
                    countries = imported.countries,
                    seconds = imported.seconds,
                    "place data imported"
                );
            }
            Event::People(fetched) => {
                self.stop();
                self.show_upkeep();
                self.say_toast(adw::Toast::new(&format!(
                    "{} named persons in {} photos, {} faces",
                    fetched.named, fetched.with_named, fetched.faces
                )));
            }
            Event::Failed(why) => {
                let job = self.imp().running.get();
                self.stop();
                if let Some(job) = job {
                    *self.imp().failed.borrow_mut() = Some((job, why.clone()));
                }
                self.refresh();
                self.say_toast(adw::Toast::new(&format!("Did not work: {why}")));
                tracing::error!(why, "the last thing asked for failed");
            }
        }
    }

    fn start(&self, job: Job, text: &str) {
        let imp = self.imp();
        imp.running.set(Some(job));
        if imp.failed.borrow().as_ref().is_some_and(|(failed, _)| *failed == job) {
            *imp.failed.borrow_mut() = None;
        }
        imp.progress.set_fraction(0.0);
        imp.progress.set_text(Some(text));
        imp.busy.set_visible(true);
        self.show_bar();
    }

    fn stop(&self) {
        self.imp().running.set(None);
        self.imp().busy.set_visible(false);
        self.show_bar();
    }

    /// Asks when each job last ran; the bar follows when the answer is in.
    fn show_upkeep(&self) {
        let imp = self.imp();
        let asked = imp.upkeeps.get() + 1;
        imp.upkeeps.set(asked);
        let Some(library) = imp.library.borrow().clone() else {
            imp.statuses.borrow_mut().clear();
            self.show_bar();
            return;
        };
        let dashboard = self.downgrade();
        library.upkeep(move |found| {
            let Some(dashboard) = dashboard.upgrade() else {
                return;
            };
            if dashboard.imp().upkeeps.get() != asked {
                return;
            }
            match found {
                Ok(statuses) => *dashboard.imp().statuses.borrow_mut() = statuses,
                Err(why) => tracing::error!(why, "the upkeep could not be read"),
            }
            dashboard.show_bar();
        });
    }

    fn show_bar(&self) {
        let imp = self.imp();
        let running = imp.running.get();
        let failed = imp.failed.borrow();
        let statuses = imp.statuses.borrow();
        let nothing = Facts {
            now: photomanager_core::clock::now(),
            ..Facts::default()
        };
        let has_library = imp.library.borrow().is_some();
        for segment in imp.segments.borrow().iter() {
            let status = statuses
                .iter()
                .find(|status| status.job == segment.job)
                .cloned()
                .unwrap_or_else(|| upkeep::status(segment.job, &nothing));
            let now = match (running, failed.as_ref()) {
                (Some(job), _) if job == segment.job => Now::Running,
                (Some(_), _) => Now::Waiting,
                _ if !has_library => Now::Waiting,
                (None, Some((job, why))) if *job == segment.job => Now::Failed(why),
                (None, _) => Now::Idle,
            };
            segment.show(&status, now);
        }
    }

    /// Lays the bar out as a column on a narrow window.
    pub fn set_narrow(&self, narrow: bool) {
        let bar = &self.imp().bar;
        match narrow {
            true => {
                bar.set_orientation(gtk::Orientation::Vertical);
                bar.add_css_class("vertical");
            }
            false => {
                bar.set_orientation(gtk::Orientation::Horizontal);
                bar.remove_css_class("vertical");
            }
        }
    }

    /// Each job of the bar: its name, its state in a word and its caption, as shown.
    pub fn upkeep_shown(&self) -> Vec<(&'static str, String, String)> {
        self.imp()
            .segments
            .borrow()
            .iter()
            .map(|segment| {
                let (word, caption) = segment.shown();
                (segment.job.title(), word, caption)
            })
            .collect()
    }

    /// Shows what is known right away and asks for a new survey, which fills the page when it
    /// arrives.
    fn refresh(&self) {
        let Some(library) = self.imp().library.borrow().clone() else {
            self.show_page(false);
            self.show_upkeep();
            return;
        };
        self.show_page(library.counts().photos > 0);
        self.show_upkeep();

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
                Gap::EventOffFolder => format!(
                    "disagrees on {} of {} photos that name their event",
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
            if let Some(fix) = fix_button(measure.missing, &Filter::missing(*gap), gap.title()) {
                row.add_suffix(&fix);
            }
            activates(&row, measure.missing, &Filter::missing(*gap));
            self.keep(&group, row.upcast());
            if *gap == Gap::Gps && !survey.gps_left.is_empty() {
                self.keep(&group, gps_left_row(survey, measure.missing).upcast());
            }
            if *gap == Gap::Gps && !survey.neighbours.is_empty() {
                self.keep(&group, neighbours_row(survey).upcast());
            }
        }
        self.keep(&group, aligned_row(&survey.aligned).upcast());
    }

    /// The line of the events where a neighbour knows the position, and how many there are.
    pub fn neighbours_line(&self) -> Option<(String, Vec<String>)> {
        let survey = self.imp().survey.borrow().clone()?;
        (!survey.neighbours.is_empty()).then(|| {
            (
                NEIGHBOURS.to_string(),
                survey.neighbours.iter().map(|event| event.event.clone()).collect(),
            )
        })
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
                    if let Some(fix) = fix_button(measure.missing, &country.filter(gap), &country.name) {
                        row.add_suffix(&fix);
                    }
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
                    if let Some(fix) = fix_button(measure.missing, &country.filter(gap), &country.name) {
                        row.add_suffix(&fix);
                    }
                    row.add_suffix(&show);
                    for event in events {
                        let inner = adw::ActionRow::builder()
                            .title(glib::markup_escape_text(&event.name))
                            .subtitle(so_far(gap, event.gap(gap)))
                            .build();
                        if let Some(fix) = fix_button(event.gap(gap).missing, &event.filter(gap), &event.name) {
                            inner.add_suffix(&fix);
                        }
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
            if let Some(fix) = fix_button(finding.count, &finding.filter, &finding.title) {
                row.add_suffix(&fix);
            }
            clickable(&row, finding.count, &finding.filter);
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

const NEIGHBOURS: &str = "Events where a neighbour knows the position";

/// The photos without GPS by what is left to do for them, each part opening its fixes or its
/// tool. The parts add up to the gap.
fn gps_left_row(survey: &Survey, missing: i64) -> adw::ExpanderRow {
    let row = adw::ExpanderRow::builder()
        .title("What Is Left for GPS")
        .subtitle(format!("{missing} photos without GPS, by what places them"))
        .build();
    for (check, count) in &survey.gps_left {
        let filter = Filter::of(Kind::Checked(*check));
        let inner = adw::ActionRow::builder()
            .title(check.title())
            .subtitle(check.detail())
            .build();
        let label = gtk::Label::builder().label(count.to_string()).build();
        label.add_css_class("dim-label");
        inner.add_suffix(&label);
        if let Some(fix) = fix_button(*count, &filter, check.title()) {
            inner.add_suffix(&fix);
        }
        clickable(&inner, *count, &filter);
        row.add_row(&inner);
    }
    row
}

/// How many events sit exactly where the layout puts them, and below it each way the rest falls
/// short, each opening its photos and its fix.
fn aligned_row(aligned: &Aligned) -> adw::ExpanderRow {
    let row = adw::ExpanderRow::builder()
        .title("Aligned with the layout")
        .subtitle(format!(
            "{} of {} events are not, {} photos",
            aligned.off_events, aligned.events, aligned.photos
        ))
        .enable_expansion(!aligned.reasons.is_empty())
        .build();
    if aligned.photos > 0 {
        let show = gtk::Button::builder()
            .icon_name("go-next-symbolic")
            .tooltip_text("Show These Photos")
            .valign(gtk::Align::Center)
            .action_name(SHOW_PHOTOS)
            .action_target(&Aligned::filter().to_string().to_variant())
            .build();
        show.add_css_class("flat");
        show.update_property(&[gtk::accessible::Property::Label(
            "Show the photos not aligned with the layout",
        )]);
        row.add_suffix(&show);
    }
    // An expander row shows its suffixes last added first: the meter, then the arrow.
    let bar = gtk::LevelBar::builder()
        .value(aligned.present())
        .valign(gtk::Align::Center)
        .width_request(96)
        .build();
    bar.update_property(&[gtk::accessible::Property::Label("Aligned with the layout")]);
    row.add_suffix(&bar);
    for finding in &aligned.reasons {
        let inner = adw::ActionRow::builder()
            .title(glib::markup_escape_text(&finding.title))
            .subtitle(glib::markup_escape_text(&finding.detail))
            .build();
        let count = gtk::Label::builder().label(finding.count.to_string()).build();
        count.add_css_class("dim-label");
        inner.add_suffix(&count);
        if let Some(fix) = fix_button(finding.count, &finding.filter, &finding.title) {
            inner.add_suffix(&fix);
        }
        clickable(&inner, finding.count, &finding.filter);
        row.add_row(&inner);
    }
    row
}

/// The events where a photo measured its position and others did not, each opening Position
/// from a Neighbour on it.
fn neighbours_row(survey: &Survey) -> adw::ExpanderRow {
    let events = &survey.neighbours;
    let row = adw::ExpanderRow::builder()
        .title(NEIGHBOURS)
        .subtitle(match events.len() {
            1 => "1 event with a photo that measured where it was".to_string(),
            count => format!("{count} events with a photo that measured where it was"),
        })
        .build();
    let count = gtk::Label::builder().label(events.len().to_string()).build();
    count.add_css_class("dim-label");
    row.add_suffix(&count);
    for event in events {
        let name = event.event.rsplit('/').next().unwrap_or(&event.event);
        let inner = adw::ActionRow::builder()
            .title(glib::markup_escape_text(name))
            .subtitle(format!(
                "{} measured, {} derived, {} without",
                event.measured, event.derived, event.none
            ))
            .activatable(true)
            .action_name("win.neighbour-event")
            .action_target(&event.event.to_variant())
            .tooltip_text("Open Position from a Neighbour on This Event")
            .build();
        inner.add_suffix(&gtk::Image::from_icon_name("go-next-symbolic"));
        row.add_row(&inner);
    }
    row
}

/// A button that opens where these photos are fixed, if there are any and the application fixes
/// them. `what` names the finding for a screen reader.
fn fix_button(count: i64, filter: &Filter, what: &str) -> Option<gtk::Button> {
    if count == 0 {
        return None;
    }
    let remedy = Remedy::of(filter)?;
    let button = gtk::Button::builder()
        .label("Fix")
        .tooltip_text(remedy.tells())
        .valign(gtk::Align::Center)
        .action_name("win.fix")
        .action_target(&filter.to_string().to_variant())
        .build();
    button.add_css_class("flat");
    // A button with a text is named by it; every row's would be "Fix".
    button.reset_relation(gtk::AccessibleRelation::LabelledBy);
    button.update_property(&[gtk::accessible::Property::Label(&format!("Fix {what}"))]);
    Some(button)
}

/// A row that shows its photos when activated, if it has any, with an arrow that says so.
fn clickable(row: &adw::ActionRow, count: i64, filter: &Filter) {
    if activates(row, count, filter) {
        row.add_suffix(&gtk::Image::from_icon_name("go-next-symbolic"));
    }
}

/// A row that shows its photos when activated, if it has any.
fn activates(row: &adw::ActionRow, count: i64, filter: &Filter) -> bool {
    if count == 0 {
        return false;
    }
    row.set_activatable(true);
    row.set_action_name(Some(SHOW_PHOTOS));
    row.set_action_target_value(Some(&filter.to_string().to_variant()));
    true
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
        (Gap::EventOffFolder, 0) if measure.of == 0 => "no photo names its event".to_string(),
        (_, 0) => "nothing missing".to_string(),
        (Gap::DateOffFolder | Gap::EventOffFolder, missing) => {
            format!("{missing} of {} disagree with the folder", measure.of)
        }
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
