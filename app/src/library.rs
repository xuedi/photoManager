//! The library as the window sees it: the cache, and a scan running off the main thread.

use std::cell::{Cell, RefCell};
use std::path::Path;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use photomanager_core::cache::Cache;
use photomanager_core::changeset::{self, ChangeSet, Wanted};
use photomanager_core::filter::Filter;
use photomanager_core::geo::Geo;
use photomanager_core::geo::import::Imported;
use photomanager_core::journal::{Journal, Kind, Pass};
use photomanager_core::metadata::Exiv2;
use photomanager_core::paths::Paths;
use photomanager_core::scan::{self, Mode, Progress, Summary, Thumbnails};
use photomanager_core::settings::{self, Settings};
use photomanager_core::survey::Survey;
use photomanager_core::thumbs::{Size, Thumbs};
use photomanager_core::write::{Engine, Summary as Applied};

#[derive(Debug)]
pub enum Event {
    Counted(usize),
    Done(usize, usize),
    Finished(Summary),
    Filled(Thumbnails),
    /// The place data is in, with what the import found.
    Places(Imported),
    /// A line to show while something long is running.
    Note(String),
    /// A change set is ready to be looked at. Nothing has been written.
    Previewed(ChangeSet),
    /// A change set was applied, or the last one was taken back.
    Applied(Kind, Applied),
    Failed(String),
}

#[derive(Debug, Clone, Copy, Default)]
pub struct Counts {
    pub photos: i64,
    pub events: i64,
    pub issues: i64,
    pub thumbnails: i64,
    pub places: i64,
}

#[derive(Debug)]
pub struct Library {
    paths: Paths,
    thumbs: Thumbs,
    cache: RefCell<Option<Cache>>,
    geo: RefCell<Option<Geo>>,
    journal: RefCell<Option<Journal>>,
    settings: RefCell<Option<Settings>>,
    scanning: Cell<bool>,
    /// The cache is out for something that is not a scan: a preview, an apply, an undo.
    working: Cell<bool>,
    cancel: Arc<AtomicBool>,
    last: RefCell<Option<Summary>>,
    survey: RefCell<Option<Rc<Survey>>>,
    /// Which survey is the newest asked for, so an older one that arrives late is dropped.
    surveys: Cell<u64>,
}

impl Library {
    pub fn open(paths: Paths) -> Result<Rc<Library>, String> {
        let cache = Cache::open(&paths.cache_db()).map_err(|error| error.to_string())?;
        let geo = Geo::open(&paths.geo_db())
            .map_err(|error| tracing::error!(%error, "the place data cannot be opened"))
            .ok();
        let journal = Journal::open(&paths.app_db())
            .map_err(|error| tracing::error!(%error, "the journal cannot be opened, so nothing can be written"))
            .ok();
        let settings = Settings::open(&paths.app_db())
            .map_err(|error| tracing::error!(%error, "the settings cannot be opened"))
            .ok();
        Ok(Rc::new(Library {
            geo: RefCell::new(geo),
            journal: RefCell::new(journal),
            settings: RefCell::new(settings),
            thumbs: Thumbs::new(paths.thumbs_dir()),
            paths,
            cache: RefCell::new(Some(cache)),
            scanning: Cell::new(false),
            working: Cell::new(false),
            cancel: Arc::new(AtomicBool::new(false)),
            last: RefCell::new(None),
            survey: RefCell::new(None),
            surveys: Cell::new(0),
        }))
    }

    pub fn paths(&self) -> &Paths {
        &self.paths
    }

    pub fn thumbs(&self) -> &Thumbs {
        &self.thumbs
    }

    pub fn is_scanning(&self) -> bool {
        self.scanning.get()
    }

    /// Whether anything at all has the cache out. Nothing else may start while it has.
    pub fn is_busy(&self) -> bool {
        self.scanning.get() || self.working.get()
    }

    pub fn last_summary(&self) -> Option<Summary> {
        *self.last.borrow()
    }

    pub fn counts(&self) -> Counts {
        let cache = self.cache.borrow();
        let Some(cache) = cache.as_ref() else {
            return Counts::default();
        };
        Counts {
            photos: cache.photo_count().unwrap_or_default(),
            events: cache.event_count().unwrap_or_default(),
            issues: cache.issue_count().unwrap_or_default(),
            thumbnails: self.thumbs.count(Size::Small) as i64,
            places: self.place_count(),
        }
    }

    /// The last survey that arrived.
    pub fn survey(&self) -> Option<Rc<Survey>> {
        self.survey.borrow().clone()
    }

    /// Looks the library over again, off the main thread and through a read-only look at the
    /// cache, so it can run while the cache itself is out for something else.
    pub fn resurvey<F: Fn(Result<Rc<Survey>, String>) + 'static>(self: &Rc<Self>, done: F) {
        let asked = self.surveys.get() + 1;
        self.surveys.set(asked);

        let (sender, receiver) = async_channel::bounded(1);
        let file = self.paths.cache_db();
        std::thread::spawn(move || {
            let taken = match Cache::read_only(&file) {
                Ok(Some(cache)) => Survey::take(&cache),
                Ok(None) => Ok(Survey::default()),
                Err(error) => Err(error),
            };
            let _ = sender.send_blocking(taken.map_err(|error| error.to_string()));
        });

        let this = self.clone();
        gtk::glib::spawn_future_local(async move {
            let Ok(taken) = receiver.recv().await else {
                return;
            };
            if this.surveys.get() != asked {
                return;
            }
            done(taken.map(|survey| {
                let survey = Rc::new(survey);
                *this.survey.borrow_mut() = Some(survey.clone());
                survey
            }));
        });
    }

    /// How many photos a filter names, or nothing while the cache is out.
    pub fn count(&self, filter: &Filter) -> Option<i64> {
        let cache = self.cache.borrow();
        filter.count(cache.as_ref()?).ok()
    }

    fn place_count(&self) -> i64 {
        self.geo
            .borrow()
            .as_ref()
            .and_then(|geo| geo.counts().ok())
            .map(|counts| counts.places)
            .unwrap_or_default()
    }

    /// When the dumps the place data was built from were last changed.
    pub fn dump_date(&self) -> Option<String> {
        self.geo.borrow().as_ref().and_then(|geo| geo.dump_date())
    }

    /// Downloads the GeoNames dumps and imports them. The only thing here that uses the network,
    /// and only because the button was pressed.
    pub fn get_places<F: Fn(Event) + 'static>(self: &Rc<Self>, report: F) {
        if self.scanning.get() {
            return;
        }
        let Some(mut geo) = self.geo.borrow_mut().take() else {
            report(Event::Failed("the place data is busy".to_string()));
            return;
        };
        self.scanning.set(true);
        self.cancel.store(false, Ordering::Relaxed);

        let (sender, receiver) = async_channel::unbounded();
        let local = self.paths.local_dumps().map(Path::to_path_buf);
        let dumps = local.clone().unwrap_or_else(|| self.paths.dumps_dir());
        let cancel = self.cancel.clone();
        let progress = sender.clone();

        std::thread::spawn(move || {
            let fetched = match local {
                Some(_) => Ok(Vec::new()),
                None => photomanager_core::geo::download::run(
                    &dumps,
                    &|step| {
                        let _ = progress.send_blocking(Message::Note(format!(
                            "Downloading {} ({} of {})",
                            step.file,
                            step.done + 1,
                            step.of
                        )));
                    },
                    &cancel,
                ),
            };
            let outcome = fetched.and_then(|_| {
                photomanager_core::geo::import::run(&mut geo, &dumps, &|step| {
                    let _ = progress.send_blocking(match step {
                        photomanager_core::geo::import::Step::Reading(file) => Message::Note(format!("Reading {file}")),
                        photomanager_core::geo::import::Step::Places(done, total) => Message::Done(done, total),
                    });
                })
            });
            let _ = sender.send_blocking(match outcome {
                Ok(imported) => Message::Places(imported, geo),
                Err(error) => Message::PlacesFailed(error.to_string(), geo),
            });
        });

        let this = self.clone();
        gtk::glib::spawn_future_local(async move {
            while let Ok(message) = receiver.recv().await {
                let event = match message {
                    Message::Note(line) => Event::Note(line),
                    Message::Done(done, total) => Event::Done(done, total),
                    Message::Places(imported, geo) => {
                        this.put_back(geo);
                        Event::Places(imported)
                    }
                    Message::PlacesFailed(why, geo) => {
                        this.put_back(geo);
                        Event::Failed(why)
                    }
                    _ => continue,
                };
                report(event);
            }
        });
    }

    fn put_back(&self, geo: Geo) {
        *self.geo.borrow_mut() = Some(geo);
        self.scanning.set(false);
    }

    /// The first photos the cache knows, in path order. What a tool is pointed at is 3.0's; this
    /// is how a change set gets photos before there is a scope selector.
    pub fn photo_paths(&self, limit: usize) -> Vec<String> {
        let cache = self.cache.borrow();
        let Some(cache) = cache.as_ref() else {
            return Vec::new();
        };
        let mut paths = cache.paths().unwrap_or_default();
        paths.sort();
        paths.truncate(limit);
        paths
    }

    /// Whether the user still has to be asked before anything is ever written to a photo.
    pub fn must_ask(&self) -> bool {
        match (self.journal.borrow().as_ref(), self.settings.borrow().as_ref()) {
            (Some(journal), Some(settings)) => settings::must_ask(journal, settings).unwrap_or(true),
            _ => true,
        }
    }

    /// Records that the user said their photos are backed up, and where they said it is.
    pub fn acknowledge(&self, backup: Option<&Path>) {
        let mut settings = self.settings.borrow_mut();
        let Some(settings) = settings.as_mut() else {
            return;
        };
        if let Err(error) = settings::acknowledge(settings, backup) {
            tracing::error!(%error, "the acknowledgement could not be kept");
        }
    }

    /// The pass the last applied change set left, if it can still be taken back.
    pub fn undoable(&self) -> Option<Pass> {
        self.journal
            .borrow()
            .as_ref()
            .and_then(|journal| changeset::undoable(journal).ok())
            .flatten()
    }

    /// An engine of its own, for the one photo a preview row is asked about.
    pub fn engine(&self) -> Result<Engine, String> {
        Engine::new(self.paths.library()).map_err(|error| error.to_string())
    }

    /// Builds a change set off the main thread. The cache alone: no photo is opened, nothing is
    /// written.
    pub fn preview<F: Fn(Event) + 'static>(self: &Rc<Self>, title: &str, wanted: Vec<Wanted>, report: F) {
        if self.is_busy() {
            return;
        }
        let Some(cache) = self.cache.borrow_mut().take() else {
            report(Event::Failed("the cache is busy".to_string()));
            return;
        };
        self.working.set(true);

        let (sender, receiver) = async_channel::unbounded();
        let title = title.to_string();
        std::thread::spawn(move || {
            let built = ChangeSet::build(&cache, &title, &wanted).map_err(|error| error.to_string());
            let _ = sender.send_blocking(Message::Previewed(built, cache));
        });

        let this = self.clone();
        gtk::glib::spawn_future_local(async move {
            while let Ok(message) = receiver.recv().await {
                let Message::Previewed(built, cache) = message else {
                    continue;
                };
                *this.cache.borrow_mut() = Some(cache);
                this.working.set(false);
                report(match built {
                    Ok(set) => Event::Previewed(set),
                    Err(why) => Event::Failed(why),
                });
            }
        });
    }

    /// Writes the selected rows, as one journal batch.
    pub fn apply<F: Fn(Event) + 'static>(self: &Rc<Self>, set: &ChangeSet, report: F) {
        self.write(Kind::Write, Some(set.clone()), report);
    }

    /// Puts the last applied change set back.
    pub fn undo_last<F: Fn(Event) + 'static>(self: &Rc<Self>, report: F) {
        self.write(Kind::Undo, None, report);
    }

    fn write<F: Fn(Event) + 'static>(self: &Rc<Self>, kind: Kind, set: Option<ChangeSet>, report: F) {
        if self.is_busy() {
            return;
        }
        let Some(mut journal) = self.journal.borrow_mut().take() else {
            report(Event::Failed(
                "the journal is not open, so nothing is written".to_string(),
            ));
            return;
        };
        let Some(mut cache) = self.cache.borrow_mut().take() else {
            *self.journal.borrow_mut() = Some(journal);
            report(Event::Failed("the cache is busy".to_string()));
            return;
        };
        self.working.set(true);
        self.cancel.store(false, Ordering::Relaxed);

        let (sender, receiver) = async_channel::unbounded();
        let library = self.paths.library().to_path_buf();
        let cancel = self.cancel.clone();
        let progress = sender.clone();

        std::thread::spawn(move || {
            let told = |done: usize, total: usize| {
                let _ = progress.send_blocking(Message::Done(done, total));
            };
            let outcome = match Engine::new(&library) {
                Ok(mut engine) => match &set {
                    Some(set) => changeset::apply(set, &mut engine, &mut journal, &mut cache, &told, &cancel),
                    None => changeset::undo_last(&mut engine, &mut journal, &mut cache, &told, &cancel),
                },
                Err(error) => Err(error),
            };
            let _ = sender.send_blocking(Message::Applied(
                kind,
                outcome.map_err(|error| error.to_string()),
                cache,
                journal,
            ));
        });

        let this = self.clone();
        gtk::glib::spawn_future_local(async move {
            while let Ok(message) = receiver.recv().await {
                let event = match message {
                    Message::Done(done, total) => Event::Done(done, total),
                    Message::Applied(kind, outcome, cache, journal) => {
                        *this.cache.borrow_mut() = Some(cache);
                        *this.journal.borrow_mut() = Some(journal);
                        this.working.set(false);
                        match outcome {
                            Ok(summary) => Event::Applied(kind, summary),
                            Err(why) => Event::Failed(why),
                        }
                    }
                    _ => continue,
                };
                report(event);
            }
        });
    }

    pub fn issue_counts(&self) -> Vec<(String, i64)> {
        let cache = self.cache.borrow();
        cache
            .as_ref()
            .and_then(|cache| cache.issue_counts().ok())
            .unwrap_or_default()
    }

    /// Stops whatever is running: a scan between photos, an apply between photos.
    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Relaxed);
    }

    /// Throws the cache away. Only the cache: the photos are not touched.
    pub fn rebuild_cache(&self) -> Result<(), String> {
        if self.scanning.get() {
            return Err("a scan is running".to_string());
        }
        let Some(cache) = self.cache.borrow_mut().take() else {
            return Err("the cache is busy".to_string());
        };
        let fresh = cache.rebuild().map_err(|error| error.to_string())?;
        *self.cache.borrow_mut() = Some(fresh);
        *self.last.borrow_mut() = None;
        Ok(())
    }

    pub fn scan<F: Fn(Event) + 'static>(self: &Rc<Self>, mode: Mode, report: F) {
        if self.scanning.get() {
            return;
        }
        let Some(mut cache) = self.cache.borrow_mut().take() else {
            report(Event::Failed("the cache is busy".to_string()));
            return;
        };
        self.scanning.set(true);
        self.cancel.store(false, Ordering::Relaxed);

        let (sender, receiver) = async_channel::unbounded();
        let library = self.paths.library().to_path_buf();
        let thumbs = self.thumbs.clone();
        let cancel = self.cancel.clone();
        let progress = sender.clone();

        std::thread::spawn(move || {
            let outcome = scan::run(
                &mut cache,
                &library,
                &Exiv2,
                &thumbs,
                mode,
                &|step| {
                    let _ = progress.send_blocking(match step {
                        Progress::Counted(total) => Message::Counted(total),
                        Progress::Done(done, total) => Message::Done(done, total),
                    });
                },
                &cancel,
            );
            let _ = sender.send_blocking(match outcome {
                Ok(summary) => Message::Finished(summary, cache),
                Err(error) => Message::Failed(error.to_string(), cache),
            });
        });

        let this = self.clone();
        gtk::glib::spawn_future_local(async move {
            while let Ok(message) = receiver.recv().await {
                let event = match message {
                    Message::Counted(total) => Event::Counted(total),
                    Message::Done(done, total) => Event::Done(done, total),
                    Message::Finished(summary, cache) => {
                        this.finish(cache, Some(summary));
                        Event::Finished(summary)
                    }
                    Message::Failed(why, cache) => {
                        this.finish(cache, None);
                        Event::Failed(why)
                    }
                    Message::Filled(done) => {
                        this.scanning.set(false);
                        Event::Filled(done)
                    }
                    _ => continue,
                };
                report(event);
            }
        });
    }

    /// Makes the thumbnails the scan could not make, reading only the photos that lack one.
    pub fn fill_thumbnails<F: Fn(Event) + 'static>(self: &Rc<Self>, report: F) {
        if self.scanning.get() {
            return;
        }
        let photos = match self.cache.borrow().as_ref() {
            Some(cache) => cache.pictures().unwrap_or_default(),
            None => return,
        };
        self.scanning.set(true);
        self.cancel.store(false, Ordering::Relaxed);

        let (sender, receiver) = async_channel::unbounded();
        let library = self.paths.library().to_path_buf();
        let thumbs = self.thumbs.clone();
        let cancel = self.cancel.clone();
        let progress = sender.clone();

        std::thread::spawn(move || {
            let done = scan::thumbnails(
                &photos,
                &library,
                &thumbs,
                &|step| {
                    let _ = progress.send_blocking(match step {
                        Progress::Counted(total) => Message::Counted(total),
                        Progress::Done(done, total) => Message::Done(done, total),
                    });
                },
                &cancel,
            );
            let _ = sender.send_blocking(Message::Filled(done));
        });

        let this = self.clone();
        gtk::glib::spawn_future_local(async move {
            while let Ok(message) = receiver.recv().await {
                let event = match message {
                    Message::Counted(total) => Event::Counted(total),
                    Message::Done(done, total) => Event::Done(done, total),
                    Message::Filled(done) => {
                        this.scanning.set(false);
                        Event::Filled(done)
                    }
                    _ => continue,
                };
                report(event);
            }
        });
    }

    fn finish(&self, cache: Cache, summary: Option<Summary>) {
        *self.cache.borrow_mut() = Some(cache);
        *self.last.borrow_mut() = summary;
        self.scanning.set(false);
    }
}

/// What the scanning thread sends back; the cache travels with the last message.
enum Message {
    Counted(usize),
    Done(usize, usize),
    Finished(Summary, Cache),
    Filled(Thumbnails),
    Places(Imported, Geo),
    PlacesFailed(String, Geo),
    Note(String),
    Previewed(std::result::Result<ChangeSet, String>, Cache),
    Applied(Kind, std::result::Result<Applied, String>, Cache, Journal),
    Failed(String, Cache),
}
