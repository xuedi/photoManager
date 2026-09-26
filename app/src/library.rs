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
use photomanager_core::edits::{self, Camera, Edit, Value};
use photomanager_core::filter::{Filter, Listed, Order};
use photomanager_core::fixes::{self, Fix, Pass as FixPass};
use photomanager_core::geo::Geo;
use photomanager_core::geo::import::Imported;
use photomanager_core::geo::lookup::Candidate;
use photomanager_core::geo::reverse::At;
use photomanager_core::immich::{self, Fetched, Snapshot};
use photomanager_core::journal::Journal;
use photomanager_core::layout::{Layout, Placement};
use photomanager_core::metadata::Exiv2;
use photomanager_core::paths::Paths;
use photomanager_core::scan::{self, Mode, Progress, Summary, Thumbnails};
use photomanager_core::scope::Scope;
use photomanager_core::settings::{self, Settings};
use photomanager_core::survey::Survey;
use photomanager_core::thumbs::{Size, Thumbs};
use photomanager_core::tools::Question;
use photomanager_core::write::{Engine, Summary as Applied};

#[derive(Debug)]
pub enum Event {
    Counted(usize),
    Done(usize, usize),
    Finished(Summary),
    Filled(Thumbnails),
    /// The place data is in, with what the import found.
    Places(Imported),
    /// Who is in the photos was fetched from Immich.
    People(Fetched),
    /// A line to show while something long is running.
    Note(String),
    /// A change set is ready to be looked at. Nothing has been written.
    Previewed(ChangeSet),
    /// A change set was applied.
    Applied(Applied),
    /// The ticked fixes were applied, a pass for each finder that had any.
    Fixed(Vec<FixPass>),
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

/// What the gallery's sidebars show.
#[derive(Debug, Clone, Default)]
pub struct Sidebars {
    pub places: Vec<browse::Place>,
    pub tags: TagTree,
}

impl Library {
    pub fn open(paths: Paths) -> Result<Rc<Library>, String> {
        let mut cache = Cache::open(&paths.cache_db()).map_err(|error| error.to_string())?;
        let geo = Geo::open(&paths.geo_db())
            .map_err(|error| tracing::error!(%error, "the place data cannot be opened"))
            .ok();
        let mut journal = Journal::open(&paths.app_db())
            .map_err(|error| tracing::error!(%error, "the journal cannot be opened, so nothing can be written"))
            .ok();
        if let Some(journal) = journal.as_mut() {
            let settled = Engine::new(paths.library()).and_then(|engine| engine.resolve(journal, &mut cache));
            if let Err(error) = settled {
                tracing::error!(%error, "an interrupted move could not be settled");
            }
        }
        let settings = Settings::open(&paths.app_db())
            .map_err(|error| tracing::error!(%error, "the settings cannot be opened"))
            .ok();
        let layout = settings.as_ref().map(settings::layout).unwrap_or_default();
        if let Err(error) = cache.follow_layout(&layout) {
            tracing::error!(%error, "the photos could not be placed in the folder layout");
        }
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

    /// Every fix in the library, found off the main thread through read-only looks at the cache
    /// and the place data. A finder that cannot look is left out, and the log says why.
    pub fn fixes<F: FnOnce(Result<Vec<Fix>, String>) + 'static>(&self, done: F) {
        let geo_db = self.paths.geo_db();
        self.read_off_thread(
            move |cache| {
                let started = std::time::Instant::now();
                let geo = Geo::read_only(&geo_db).ok().flatten();
                let found = fixes::find(cache, geo.as_ref());
                tracing::info!(
                    fixes = found.len(),
                    seconds = started.elapsed().as_secs_f64(),
                    "suggestions found"
                );
                Ok(found)
            },
            done,
        );
    }

    /// How many photos a scope names, counted off the main thread.
    pub fn scope_count<F: FnOnce(Result<usize, String>) + 'static>(&self, scope: &Scope, done: F) {
        let scope = scope.clone();
        self.read_off_thread(move |cache| Ok(scope.paths(cache)?.len()), done);
    }

    /// The cameras of the scope, for Shift Dates.
    pub fn cameras(&self, scope: &Scope) -> Result<Vec<Camera>, String> {
        let cache = self.cache.borrow();
        let cache = cache.as_ref().ok_or("the cache is busy")?;
        edits::cameras(cache, scope).map_err(|error| error.to_string())
    }

    /// Where Move Event starts for the scope's one event.
    pub fn move_proposal(&self, scope: &Scope) -> Result<Question, String> {
        let cache = self.cache.borrow();
        let cache = cache.as_ref().ok_or("the cache is busy")?;
        let geo = self.geo.borrow();
        edits::proposal(cache, geo.as_ref(), scope)
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

    /// The folder layout the user chose, or the default.
    pub fn layout(&self) -> Layout {
        self.settings
            .borrow()
            .as_ref()
            .map(settings::layout)
            .unwrap_or_default()
    }

    /// Keeps a new folder layout and places every photo in it again. Nothing in the library
    /// moves; what is off the new layout is for Folder Migration.
    pub fn set_layout(&self, layout: &Layout) -> Result<(), String> {
        layout.check()?;
        if self.scanning.get() || self.working.get() {
            return Err("wait until the library is not busy".to_string());
        }
        {
            let mut cache = self.cache.borrow_mut();
            let cache = cache.as_mut().ok_or("the cache is busy")?;
            cache.follow_layout(layout).map_err(|error| error.to_string())?;
        }
        self.put_setting(settings::FOLDER_LAYOUT, &layout.to_string());
        *self.survey.borrow_mut() = None;
        self.moved_on();
        Ok(())
    }

    /// How many events a layout would find out of place, of how many.
    pub fn events_off(&self, layout: &Layout) -> Option<(usize, usize)> {
        let cache = self.cache.borrow();
        let geo = self.geo.borrow();
        photomanager_core::tools::folders::events_off(cache.as_ref()?, geo.as_ref(), layout)
            .map_err(|error| tracing::warn!(%error, "the events off a layout could not be counted"))
            .ok()
    }

    /// The top-level tags a tag level can be made of.
    pub fn tag_roots(&self) -> Vec<String> {
        self.cache
            .borrow()
            .as_ref()
            .and_then(|cache| cache.tag_roots().ok())
            .unwrap_or_default()
    }

    /// An event of the library to show a layout with.
    pub fn sample_event(&self) -> Option<Placement> {
        self.cache.borrow().as_ref()?.sample_event().ok().flatten()
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
        // Place data thrown away for a new version is imported again from the dumps it was made
        // from, without going to the network.
        let kept = self.paths.dumps_dir();
        let local = self
            .paths
            .local_dumps()
            .map(Path::to_path_buf)
            .or_else(|| (!geo.is_filled() && photomanager_core::geo::import::present(&kept)).then_some(kept));
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

    /// Where Immich is, as the person set it.
    pub fn immich_address(&self) -> Option<String> {
        self.setting(settings::IMMICH_URL)
    }

    /// Where the library lies inside Immich, when it is not read from Immich itself.
    pub fn immich_prefix(&self) -> Option<String> {
        self.setting(settings::IMMICH_PREFIX)
            .filter(|prefix| !prefix.trim().is_empty())
    }

    /// How many persons the last snapshot names, and when it was fetched.
    pub fn people_known(&self) -> Option<(usize, String)> {
        let snapshot = Snapshot::open(&self.paths.immich_db()).ok().flatten()?;
        let named = snapshot.people().ok()?.iter().filter(|person| person.named()).count();
        Some((named, snapshot.about("fetched-at").ok().flatten().unwrap_or_default()))
    }

    /// Whether the address and the key work, asked off the main thread. Nothing is kept.
    pub fn check_immich<F: FnOnce(Result<String, String>) + 'static>(&self, url: &str, key: String, done: F) {
        let url = url.to_string();
        let (sender, receiver) = async_channel::bounded(1);
        std::thread::spawn(move || {
            let checked = immich::Client::new(&url, &key)
                .and_then(|mut client| client.check().map_err(|error| error.to_string()));
            let _ = sender.send_blocking(checked);
        });
        gtk::glib::spawn_future_local(async move {
            if let Ok(checked) = receiver.recv().await {
                done(checked);
            }
        });
    }

    /// Reads who is in the photos from Immich into the snapshot. Only reads, and only because the
    /// button was pressed.
    pub fn get_people<F: Fn(Event) + 'static>(self: &Rc<Self>, key: String, report: F) {
        if self.scanning.get() {
            return;
        }
        let Some(url) = self.immich_address() else {
            report(Event::Failed("there is no Immich address yet".to_string()));
            return;
        };
        let mut client = match immich::Client::new(&url, &key) {
            Ok(client) => client,
            Err(why) => {
                report(Event::Failed(why));
                return;
            }
        };
        self.scanning.set(true);
        self.cancel.store(false, Ordering::Relaxed);

        let (sender, receiver) = async_channel::unbounded();
        let file = self.paths.immich_db();
        let prefix = self.immich_prefix();
        let cancel = self.cancel.clone();
        let progress = sender.clone();
        std::thread::spawn(move || {
            let fetched = immich::fetch(
                &mut client,
                &file,
                prefix.as_deref(),
                &|step| {
                    let _ = progress.send_blocking(match step {
                        immich::Step::Faces(done, of) if done % 25 == 0 => Message::Done(done, of),
                        immich::Step::Faces(..) => return,
                        other => Message::Note(other.tells()),
                    });
                },
                &cancel,
            );
            let _ = sender.send_blocking(Message::People(fetched.map_err(|error| error.to_string())));
        });

        let this = self.clone();
        gtk::glib::spawn_future_local(async move {
            while let Ok(message) = receiver.recv().await {
                let event = match message {
                    Message::Note(line) => Event::Note(line),
                    Message::Done(done, total) => Event::Done(done, total),
                    Message::People(fetched) => {
                        this.scanning.set(false);
                        this.moved_on();
                        match fetched {
                            Ok(fetched) => Event::People(fetched),
                            Err(why) => Event::Failed(why),
                        }
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

    /// An edit's change set for the scope with the value given. Nothing is written.
    pub fn run_edit<F: Fn(Event) + 'static>(self: &Rc<Self>, edit: Edit, value: Value, scope: &Scope, report: F) {
        let scope = scope.clone();
        let geo_db = self.paths.geo_db();
        let root = self.paths.library().to_path_buf();
        self.build(
            move |cache| {
                let geo = Geo::read_only(&geo_db).ok().flatten();
                let mut set = edit.change_set(&value, cache, geo.as_ref(), &scope)?;
                set.look(&root);
                Ok(set)
            },
            report,
        );
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

    /// Writes the ticked fixes, finder by finder, reading the library again between passes.
    pub fn apply_fixes<F: Fn(Event) + 'static>(self: &Rc<Self>, ticked: Vec<Fix>, report: F) {
        self.write(Job::Fixes(ticked), report);
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
        let geo_db = self.paths.geo_db();
        let thumbs = self.thumbs.clone();

        std::thread::spawn(move || {
            let told = |done: usize, total: usize| {
                let _ = progress.send_blocking(Message::Done(done, total));
            };
            if let Job::Fixes(ticked) = &job {
                let geo = Geo::read_only(&geo_db).ok().flatten();
                let mut rescan = |cache: &mut Cache| -> Result<(), String> {
                    let _ = progress.send_blocking(Message::Note("Reading the photos again".to_string()));
                    scan::run(cache, &library, &Exiv2, &thumbs, Mode::Reconcile, &|_| {}, &cancel)
                        .map(|_| ())
                        .map_err(|error| error.to_string())
                };
                let outcome = Engine::new(&library)
                    .map_err(|error| error.to_string())
                    .and_then(|mut engine| {
                        fixes::apply(
                            ticked,
                            &mut cache,
                            geo.as_ref(),
                            &mut engine,
                            &mut journal,
                            &mut rescan,
                            &told,
                            &cancel,
                        )
                    });
                let _ = sender.send_blocking(Message::Fixed(outcome, cache, journal));
                return;
            }
            let outcome = match Engine::new(&library) {
                Ok(mut engine) => match &job {
                    Job::Fixes(_) => unreachable!("the fixes are applied above"),
                    Job::Apply(set) => changeset::apply(set, &mut engine, &mut journal, &mut cache, &told, &cancel),
                },
                Err(error) => Err(error),
            };
            let _ = sender.send_blocking(Message::Applied(
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
                    Message::Note(note) => Event::Note(note),
                    Message::Fixed(outcome, cache, journal) => {
                        *this.cache.borrow_mut() = Some(cache);
                        *this.journal.borrow_mut() = Some(journal);
                        this.working.set(false);
                        match outcome {
                            Ok(passes) => Event::Fixed(passes),
                            Err(why) => Event::Failed(why),
                        }
                    }
                    Message::Applied(outcome, cache, journal) => {
                        *this.cache.borrow_mut() = Some(cache);
                        *this.journal.borrow_mut() = Some(journal);
                        this.working.set(false);
                        match outcome {
                            Ok(summary) => Event::Applied(summary),
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
    Fixes(Vec<Fix>),
}

/// What the scanning thread sends back; the cache travels with the last message.
enum Message {
    Counted(usize),
    Done(usize, usize),
    Finished(Summary, Cache),
    Filled(Thumbnails),
    Places(Imported, Geo),
    PlacesFailed(String, Geo),
    People(std::result::Result<Fetched, String>),
    Note(String),
    Previewed(std::result::Result<ChangeSet, String>, Cache),
    Applied(std::result::Result<Applied, String>, Cache, Journal),
    Fixed(std::result::Result<Vec<FixPass>, String>, Cache, Journal),
    Failed(String, Cache),
}
