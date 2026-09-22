//! The library as the window sees it: the cache, and a scan running off the main thread.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use photomanager_core::cache::Cache;
use photomanager_core::metadata::Exiv2;
use photomanager_core::paths::Paths;
use photomanager_core::scan::{self, Mode, Progress, Summary, Thumbnails};
use photomanager_core::thumbs::{Size, Thumbs};

#[derive(Debug)]
pub enum Event {
    Counted(usize),
    Done(usize, usize),
    Finished(Summary),
    Filled(Thumbnails),
    Failed(String),
}

#[derive(Debug, Clone, Copy, Default)]
pub struct Counts {
    pub photos: i64,
    pub events: i64,
    pub issues: i64,
    pub thumbnails: i64,
}

#[derive(Debug)]
pub struct Library {
    paths: Paths,
    thumbs: Thumbs,
    cache: RefCell<Option<Cache>>,
    scanning: Cell<bool>,
    cancel: Arc<AtomicBool>,
    last: RefCell<Option<Summary>>,
}

impl Library {
    pub fn open(paths: Paths) -> Result<Rc<Library>, String> {
        let cache = Cache::open(&paths.cache_db()).map_err(|error| error.to_string())?;
        Ok(Rc::new(Library {
            thumbs: Thumbs::new(paths.thumbs_dir()),
            paths,
            cache: RefCell::new(Some(cache)),
            scanning: Cell::new(false),
            cancel: Arc::new(AtomicBool::new(false)),
            last: RefCell::new(None),
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
        }
    }

    pub fn issue_counts(&self) -> Vec<(String, i64)> {
        let cache = self.cache.borrow();
        cache
            .as_ref()
            .and_then(|cache| cache.issue_counts().ok())
            .unwrap_or_default()
    }

    pub fn cancel_scan(&self) {
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
    Failed(String, Cache),
}
