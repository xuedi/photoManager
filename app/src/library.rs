//! The library as the window sees it: the cache, and a scan running off the main thread.

use std::cell::{Cell, RefCell};
use std::path::Path;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use photomanager_core::browse::{self, TagTree};
use photomanager_core::cache::Cache;
use photomanager_core::changeset::{self, ChangeSet, Wanted};
use photomanager_core::details::Details;
use photomanager_core::filter::{Filter, Listed, Order};
use photomanager_core::geo::Geo;
use photomanager_core::geo::import::Imported;
use photomanager_core::geo::lookup::Candidate;
use photomanager_core::geo::reverse::At;
use photomanager_core::history;
use photomanager_core::journal::{Journal, Kind, Pass, Recorded};
use photomanager_core::metadata::Exiv2;
use photomanager_core::paths::Paths;
use photomanager_core::scan::{self, Mode, Progress, Summary, Thumbnails};
use photomanager_core::scope::Scope;
use photomanager_core::settings::{self, Settings};
use photomanager_core::survey::Survey;
use photomanager_core::thumbs::{Size, Thumbs};
use photomanager_core::tools;
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
    /// The same for the gallery's queries.
    queries: Cell<u64>,
    /// Goes up whenever a scan or a fill-in pass is over, so what was read before can tell it
    /// is stale.
    version: Cell<u64>,
    /// Told whenever the version goes up.
    watchers: Watchers,
}

#[derive(Default)]
struct Watchers(RefCell<Vec<Box<dyn Fn()>>>);

impl std::fmt::Debug for Watchers {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} watchers", self.0.borrow().len())
    }
}

/// What the Tools page shows for a scope: how many photos it names, and how many each tool
/// would change, by key.
#[derive(Debug, Clone, Default)]
pub struct Counted {
    pub photos: usize,
    pub tools: Vec<(String, Result<usize, String>)>,
}

/// What the gallery's sidebars show.
#[derive(Debug, Clone, Default)]
pub struct Sidebars {
    pub places: Vec<browse::Place>,
    pub tags: TagTree,
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
            queries: Cell::new(0),
            version: Cell::new(0),
            watchers: Watchers::default(),
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

    /// The photos of a filter in the order asked for, read off the main thread through a
    /// read-only look at the cache. Only the newest query asked for is answered.
    pub fn query<F: FnOnce(Result<Vec<Listed>, String>) + 'static>(
        self: &Rc<Self>,
        filter: &Filter,
        order: Order,
        done: F,
    ) {
        let asked = self.queries.get() + 1;
        self.queries.set(asked);
        let this = self.clone();
        let filter = filter.clone();
        self.read_off_thread(
            move |cache| filter.photos(cache, order),
            move |found| {
                if this.queries.get() == asked {
                    done(found);
                }
            },
        );
    }

    /// Every tool's change set for the scope, counted off the main thread through a read-only look
    /// at the cache. The numbers are the ones the preview will show.
    pub fn count_tools<F: FnOnce(Result<Counted, String>) + 'static>(&self, scope: &Scope, done: F) {
        let scope = scope.clone();
        self.read_off_thread(
            move |cache| {
                Ok(Counted {
                    photos: scope.paths(cache)?.len(),
                    tools: tools::ALL
                        .iter()
                        .map(|tool| (tool.key().to_string(), tools::count(*tool, cache, &scope, None)))
                        .collect(),
                })
            },
            done,
        );
    }

    /// The countries, the events and the tags, with their counts.
    pub fn sidebars<F: FnOnce(Result<Sidebars, String>) + 'static>(&self, done: F) {
        self.read_off_thread(
            |cache| {
                Ok(Sidebars {
                    places: browse::places(cache)?,
                    tags: TagTree::take(cache)?,
                })
            },
            done,
        );
    }

    pub fn version(&self) -> u64 {
        self.version.get()
    }

    /// Calls `changed` every time a scan or a fill-in pass is over.
    pub fn connect_changed(&self, changed: impl Fn() + 'static) {
        self.watchers.0.borrow_mut().push(Box::new(changed));
    }

    fn moved_on(&self) {
        self.version.set(self.version.get() + 1);
        for watcher in self.watchers.0.borrow().iter() {
            watcher();
        }
    }

    /// Everything the cache knows about one photo, read off the main thread.
    pub fn details<F: FnOnce(Result<Option<Details>, String>) + 'static>(&self, rel_path: &str, done: F) {
        let rel_path = rel_path.to_string();
        self.read_off_thread(move |cache| Details::of(cache, &rel_path), done);
    }

    fn read_off_thread<T: Default + Send + 'static>(
        &self,
        read: impl FnOnce(&Cache) -> photomanager_core::cache::Result<T> + Send + 'static,
        done: impl FnOnce(Result<T, String>) + 'static,
    ) {
        let (sender, receiver) = async_channel::bounded(1);
        let file = self.paths.cache_db();
        std::thread::spawn(move || {
            let read = match Cache::read_only(&file) {
                Ok(Some(cache)) => read(&cache),
                Ok(None) => Ok(T::default()),
                Err(error) => Err(error),
            };
            let _ = sender.send_blocking(read.map_err(|error| error.to_string()));
        });
        gtk::glib::spawn_future_local(async move {
            if let Ok(read) = receiver.recv().await {
                done(read);
            }
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

    /// What is at a point, from the local place data: no network. `None` when there is no place
    /// data yet or it is busy.
    pub fn nearest(&self, lat: f64, lon: f64) -> Option<At> {
        let geo = self.geo.borrow();
        let geo = geo.as_ref().filter(|geo| geo.is_filled())?;
        geo.at(lat, lon)
            .map_err(|error| tracing::warn!(%error, "the place data could not be asked"))
            .ok()
    }

    /// The places a name could mean, best first, from the local place data.
    pub fn find_place(&self, text: &str) -> Vec<Candidate> {
        let geo = self.geo.borrow();
        let Some(geo) = geo.as_ref().filter(|geo| geo.is_filled()) else {
            return Vec::new();
        };
        geo.find(text, None)
            .map(|found| found.candidates)
            .map_err(|error| tracing::warn!(%error, "the place data could not be asked"))
            .unwrap_or_default()
    }

    pub fn setting(&self, name: &str) -> Option<String> {
        self.settings.borrow().as_ref()?.get(name).ok().flatten()
    }

    pub fn put_setting(&self, name: &str, value: &str) {
        let mut settings = self.settings.borrow_mut();
        let Some(settings) = settings.as_mut() else {
            return;
        };
        if let Err(error) = settings.put(name, value) {
            tracing::error!(%error, name, "the setting could not be kept");
        }
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

    /// The first photos the cache knows, in path order.
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

    /// The photos a scope names, or none while the cache is out.
    pub fn scope_paths(&self, scope: &photomanager_core::scope::Scope) -> Vec<String> {
        let cache = self.cache.borrow();
        cache
            .as_ref()
            .and_then(|cache| scope.paths(cache).ok())
            .unwrap_or_default()
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

    /// Passes from the journal, newest first. `None` while a pass is being written, which has the
    /// journal out.
    pub fn history(&self, skip: i64, limit: i64) -> Option<Vec<history::Pass>> {
        let journal = self.journal.borrow();
        history::passes(journal.as_ref()?, skip, limit)
            .map_err(|error| tracing::error!(%error, "the history could not be read"))
            .ok()
    }

    pub fn pass(&self, batch: i64) -> Option<(history::Pass, Vec<Recorded>)> {
        let journal = self.journal.borrow();
        let journal = journal.as_ref()?;
        history::pass(journal, batch)
            .and_then(|pass| Ok((pass, history::photos(journal, batch)?)))
            .map_err(|error| tracing::error!(%error, batch, "the pass could not be read"))
            .ok()
    }

    /// An engine of its own, for the one photo a preview row is asked about.
    pub fn engine(&self) -> Result<Engine, String> {
        Engine::new(self.paths.library()).map_err(|error| error.to_string())
    }

    /// Builds a change set off the main thread. The cache alone: no photo is opened, nothing is
    /// written.
    pub fn preview<F: Fn(Event) + 'static>(self: &Rc<Self>, title: &str, wanted: Vec<Wanted>, report: F) {
        let title = title.to_string();
        self.build(
            move |cache| ChangeSet::build(cache, &title, &wanted).map_err(|error| error.to_string()),
            report,
        );
    }

    /// A tool's change set for the scope, with its settings as text or its own defaults.
    pub fn run_tool<F: Fn(Event) + 'static>(
        self: &Rc<Self>,
        key: &str,
        settings: Option<String>,
        scope: &Scope,
        report: F,
    ) {
        let Some(tool) = tools::find(key) else {
            report(Event::Failed(format!("there is no tool {key}")));
            return;
        };
        let scope = scope.clone();
        self.build(move |cache| tool.change_set(cache, &scope, settings.as_deref()), report);
    }

    fn build<F: Fn(Event) + 'static>(
        self: &Rc<Self>,
        make: impl FnOnce(&Cache) -> Result<ChangeSet, String> + Send + 'static,
        report: F,
    ) {
        if self.is_busy() {
            report(Event::Failed("something else is running".to_string()));
            return;
        }
        let Some(cache) = self.cache.borrow_mut().take() else {
            report(Event::Failed("the cache is busy".to_string()));
            return;
        };
        self.working.set(true);

        let (sender, receiver) = async_channel::unbounded();
        std::thread::spawn(move || {
            let built = make(&cache);
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
        self.write(Job::Apply(set.clone()), report);
    }

    /// Puts the last applied change set back.
    pub fn undo_last<F: Fn(Event) + 'static>(self: &Rc<Self>, report: F) {
        self.write(Job::UndoLast, report);
    }

    /// Puts any pass back that can still be taken back.
    pub fn take_back<F: Fn(Event) + 'static>(self: &Rc<Self>, batch: i64, report: F) {
        self.write(Job::TakeBack(batch), report);
    }

    fn write<F: Fn(Event) + 'static>(self: &Rc<Self>, job: Job, report: F) {
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
                Ok(mut engine) => match &job {
                    Job::Apply(set) => changeset::apply(set, &mut engine, &mut journal, &mut cache, &told, &cancel),
                    Job::UndoLast => changeset::undo_last(&mut engine, &mut journal, &mut cache, &told, &cancel),
                    Job::TakeBack(batch) => {
                        changeset::take_back(&mut engine, &mut journal, &mut cache, *batch, &told, &cancel)
                    }
                },
                Err(error) => Err(error),
            };
            let kind = match job {
                Job::Apply(_) => Kind::Write,
                Job::UndoLast | Job::TakeBack(_) => Kind::Undo,
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
                        this.moved_on();
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
        self.moved_on();
    }
}

/// What a write pass is asked to do.
enum Job {
    Apply(ChangeSet),
    UndoLast,
    TakeBack(i64),
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
