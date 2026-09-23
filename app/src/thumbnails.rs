//! The grid's pictures, loaded when a cell is bound and never before. A few workers read and
//! decode off the main thread, in the order the cells were bound, which puts the screen first. A
//! cell that is unbound takes its request back out of the queue, so a fast scroll leaves nothing
//! behind to wait for. The last few hundred results are kept. A cell that shows something else
//! by the time its picture arrives never sees it.

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::rc::{Rc, Weak};
use std::sync::{Arc, Condvar, Mutex};

use gtk::prelude::Cast;
use gtk::{gdk, glib};
use photomanager_core::thumbs::{Rgba, Thumbs};

/// How many pictures are kept. At 256 px and four bytes a pixel that is about 120 MB at worst.
pub const KEPT: usize = 600;

const WORKERS: usize = 4;

/// The most recently used values, never more than the bound.
#[derive(Debug)]
pub struct Lru<V> {
    bound: usize,
    values: HashMap<String, V>,
    order: VecDeque<String>,
}

impl<V: Clone> Lru<V> {
    pub fn new(bound: usize) -> Lru<V> {
        Lru {
            bound,
            values: HashMap::new(),
            order: VecDeque::new(),
        }
    }

    pub fn get(&mut self, key: &str) -> Option<V> {
        let value = self.values.get(key)?.clone();
        self.touch(key);
        Some(value)
    }

    pub fn put(&mut self, key: String, value: V) {
        if self.values.insert(key.clone(), value).is_some() {
            self.touch(&key);
            return;
        }
        self.order.push_back(key);
        while self.order.len() > self.bound {
            if let Some(oldest) = self.order.pop_front() {
                self.values.remove(&oldest);
            }
        }
    }

    pub fn len(&self) -> usize {
        self.values.len()
    }

    pub fn is_empty(&self) -> bool {
        self.values.is_empty()
    }

    pub fn clear(&mut self) {
        self.values.clear();
        self.order.clear();
    }

    fn touch(&mut self, key: &str) {
        if let Some(at) = self.order.iter().position(|each| each == key) {
            let key = self.order.remove(at).expect("just found");
            self.order.push_back(key);
        }
    }
}

/// What a cell asks for: one picture, by the image it shows.
#[derive(Debug, Clone)]
pub struct Request {
    /// The content id, or the path of a file that has none.
    pub key: String,
    pub content_id: Option<String>,
    pub file: PathBuf,
    pub orientation: Option<i64>,
}

/// One per cell. Every bind makes the slot's earlier requests stale.
#[derive(Debug, Clone, Default)]
pub struct Slot {
    generation: Rc<Cell<u64>>,
    waiting_for: Rc<RefCell<Option<String>>>,
}

impl Slot {
    fn rebind(&self) -> u64 {
        self.generation.set(self.generation.get() + 1);
        self.generation.get()
    }

    fn is(&self, generation: u64) -> bool {
        self.generation.get() == generation
    }
}

type Deliver<T> = Box<dyn FnOnce(Option<T>)>;
/// A slot, the bind it asked in, and where the picture goes.
type Waiter<T> = (Slot, u64, Deliver<T>);
type Source = dyn Fn(&Request) -> Option<Rgba> + Send + Sync;

#[derive(Default)]
struct Jobs {
    queued: VecDeque<Request>,
    closed: bool,
}

struct Queue {
    jobs: Mutex<Jobs>,
    ready: Condvar,
}

struct Inner<T> {
    kept: RefCell<Lru<Option<T>>>,
    waiting: RefCell<HashMap<String, Vec<Waiter<T>>>>,
    queue: Arc<Queue>,
    make: Box<dyn Fn(Rgba) -> T>,
    delivered: Cell<usize>,
}

impl<T> Drop for Inner<T> {
    fn drop(&mut self) {
        if let Ok(mut jobs) = self.queue.jobs.lock() {
            jobs.closed = true;
        }
        self.queue.ready.notify_all();
    }
}

/// Loads pictures and turns them into `T` on the main thread: a texture in the application, a
/// plain value in a test.
pub struct Loader<T: Clone + 'static> {
    inner: Rc<Inner<T>>,
}

/// Another handle on the same loader: the same workers, the same pictures kept.
impl<T: Clone + 'static> Clone for Loader<T> {
    fn clone(&self) -> Self {
        Loader {
            inner: self.inner.clone(),
        }
    }
}

impl<T: Clone + 'static> std::fmt::Debug for Loader<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Loader").field("kept", &self.kept()).finish()
    }
}

impl<T: Clone + 'static> Loader<T> {
    pub fn new(
        bound: usize,
        source: impl Fn(&Request) -> Option<Rgba> + Send + Sync + 'static,
        make: impl Fn(Rgba) -> T + 'static,
    ) -> Loader<T> {
        let queue = Arc::new(Queue {
            jobs: Mutex::new(Jobs::default()),
            ready: Condvar::new(),
        });
        let source: Arc<Source> = Arc::new(source);
        let (sender, receiver) = async_channel::unbounded::<(String, Option<Rgba>)>();
        for _ in 0..WORKERS {
            let queue = queue.clone();
            let source = source.clone();
            let sender = sender.clone();
            std::thread::spawn(move || work(&queue, &*source, &sender));
        }

        let inner = Rc::new(Inner {
            kept: RefCell::new(Lru::new(bound)),
            waiting: RefCell::new(HashMap::new()),
            queue,
            make: Box::new(make),
            delivered: Cell::new(0),
        });
        let weak: Weak<Inner<T>> = Rc::downgrade(&inner);
        glib::MainContext::ref_thread_default().spawn_local(async move {
            while let Ok((key, picture)) = receiver.recv().await {
                let Some(inner) = weak.upgrade() else {
                    return;
                };
                Loader { inner }.arrived(key, picture);
            }
        });
        Loader { inner }
    }

    /// Asks for a picture for the slot. It comes right away when it is kept, later when it has to
    /// be read, and never when the slot has been bound to something else in the meantime.
    pub fn load(&self, slot: &Slot, request: Request, deliver: impl FnOnce(Option<T>) + 'static) {
        self.release(slot);
        let generation = slot.rebind();
        let kept = self.inner.kept.borrow_mut().get(&request.key);
        if let Some(kept) = kept {
            self.inner.delivered.set(self.inner.delivered.get() + 1);
            deliver(kept);
            return;
        }
        *slot.waiting_for.borrow_mut() = Some(request.key.clone());
        let mut waiting = self.inner.waiting.borrow_mut();
        let asked = waiting.contains_key(&request.key);
        waiting
            .entry(request.key.clone())
            .or_default()
            .push((slot.clone(), generation, Box::new(deliver)));
        if !asked {
            let mut jobs = self.inner.queue.jobs.lock().expect("the queue");
            jobs.queued.push_back(request);
            self.inner.queue.ready.notify_one();
        }
    }

    /// The slot no longer wants what it asked for. A request nobody wants any more is dropped
    /// before a worker spends time on it.
    pub fn release(&self, slot: &Slot) {
        slot.rebind();
        let Some(key) = slot.waiting_for.borrow_mut().take() else {
            return;
        };
        let mut waiting = self.inner.waiting.borrow_mut();
        let Some(waiters) = waiting.get_mut(&key) else {
            return;
        };
        waiters.retain(|(slot, generation, _)| slot.is(*generation));
        if waiters.is_empty() {
            waiting.remove(&key);
            let mut jobs = self.inner.queue.jobs.lock().expect("the queue");
            jobs.queued.retain(|job| job.key != key);
        }
    }

    /// Forgets what is kept, for when the thumbnails themselves may have changed.
    pub fn clear(&self) {
        self.inner.kept.borrow_mut().clear();
    }

    pub fn kept(&self) -> usize {
        self.inner.kept.borrow().len()
    }

    /// How many pictures reached a cell, kept or read.
    pub fn delivered(&self) -> usize {
        self.inner.delivered.get()
    }

    fn arrived(&self, key: String, picture: Option<Rgba>) {
        let value = picture.map(|picture| (self.inner.make)(picture));
        self.inner.kept.borrow_mut().put(key.clone(), value.clone());
        let waiters = self.inner.waiting.borrow_mut().remove(&key).unwrap_or_default();
        for (slot, generation, deliver) in waiters {
            if slot.is(generation) {
                slot.waiting_for.borrow_mut().take();
                self.inner.delivered.set(self.inner.delivered.get() + 1);
                deliver(value.clone());
            }
        }
    }
}

fn work(queue: &Queue, source: &Source, sender: &async_channel::Sender<(String, Option<Rgba>)>) {
    loop {
        let request = {
            let mut jobs = queue.jobs.lock().expect("the queue");
            loop {
                if jobs.closed {
                    return;
                }
                if let Some(request) = jobs.queued.pop_front() {
                    break request;
                }
                jobs = queue.ready.wait(jobs).expect("the queue");
            }
        };
        let picture = source(&request);
        if sender.send_blocking((request.key, picture)).is_err() {
            return;
        }
    }
}

/// The loader the gallery uses: stored thumbnails first, the embedded one as the placeholder.
pub fn textures(thumbs: Thumbs) -> Loader<gdk::Texture> {
    Loader::new(
        KEPT,
        move |request| thumbs.for_grid(request.content_id.as_deref(), &request.file, request.orientation),
        |picture| {
            let stride = picture.stride();
            gdk::MemoryTexture::new(
                picture.width as i32,
                picture.height as i32,
                gdk::MemoryFormat::R8g8b8a8,
                &glib::Bytes::from_owned(picture.pixels),
                stride,
            )
            .upcast()
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn request(key: &str) -> Request {
        Request {
            key: key.to_string(),
            content_id: Some(key.to_string()),
            file: PathBuf::from(key),
            orientation: None,
        }
    }

    fn picture(width: u32) -> Rgba {
        Rgba {
            pixels: vec![0; width as usize * 4],
            width,
            height: 1,
        }
    }

    /// Runs the main loop until `done` or a few seconds have passed.
    fn until(context: &glib::MainContext, done: impl Fn() -> bool) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while !done() && std::time::Instant::now() < deadline {
            context.iteration(false);
        }
    }

    #[test]
    fn keeps_no_more_than_its_bound() {
        let mut kept = Lru::new(3);
        for key in ["a", "b", "c"] {
            kept.put(key.to_string(), key.len());
        }
        assert_eq!(kept.get("a"), Some(1), "a is now the newest");
        kept.put("d".to_string(), 1);
        assert_eq!(kept.len(), 3);
        assert_eq!(kept.get("b"), None, "the oldest went");
        assert_eq!(kept.get("a"), Some(1));
        for at in 0..100 {
            kept.put(at.to_string(), at);
            assert!(kept.len() <= 3);
        }
    }

    #[test]
    fn a_recycled_cell_never_gets_what_it_asked_for_before() {
        let context = glib::MainContext::new();
        context
            .with_thread_default(|| {
                // `slow` is held back until the cell has moved on to `fast`.
                let gate = Arc::new(Mutex::new(()));
                let held = gate.lock().unwrap();
                let read = Arc::new(AtomicUsize::new(0));
                let loader: Loader<u32> = Loader::new(
                    10,
                    {
                        let gate = gate.clone();
                        let read = read.clone();
                        move |request| {
                            read.fetch_add(1, Ordering::SeqCst);
                            match request.key.as_str() {
                                "slow" => {
                                    let _wait = gate.lock().unwrap();
                                    Some(picture(1))
                                }
                                "none" => None,
                                _ => Some(picture(2)),
                            }
                        }
                    },
                    |picture| picture.width,
                );

                let slot = Slot::default();
                type Got = Rc<RefCell<Vec<(&'static str, Option<u32>)>>>;
                let got: Got = Rc::default();
                let record = |name: &'static str| {
                    let got = got.clone();
                    move |value| got.borrow_mut().push((name, value))
                };
                loader.load(&slot, request("slow"), record("slow"));
                until(&context, || read.load(Ordering::SeqCst) == 1);
                loader.load(&slot, request("fast"), record("fast"));
                until(&context, || !got.borrow().is_empty());
                drop(held);
                until(&context, || loader.kept() == 2);

                assert_eq!(*got.borrow(), [("fast", Some(2))], "the slow one was not delivered");
                assert_eq!(loader.kept(), 2, "but it is kept for the next cell that shows it");

                let other = Slot::default();
                loader.load(&other, request("slow"), record("kept"));
                assert_eq!(
                    got.borrow().last(),
                    Some(&("kept", Some(1))),
                    "straight from what is kept"
                );

                loader.load(&other, request("none"), record("none"));
                until(&context, || got.borrow().len() == 3);
                assert_eq!(got.borrow().last(), Some(&("none", None)), "nothing is an answer too");
                assert_eq!(
                    loader.delivered(),
                    3,
                    "fast, the kept one and none; never the stale one"
                );
            })
            .unwrap();
    }

    #[test]
    fn a_released_request_is_not_read() {
        let context = glib::MainContext::new();
        context
            .with_thread_default(|| {
                let gate = Arc::new(Mutex::new(()));
                let held = gate.lock().unwrap();
                let read = Arc::new(Mutex::new(Vec::new()));
                let loader: Loader<u32> = Loader::new(
                    10,
                    {
                        let gate = gate.clone();
                        let read = read.clone();
                        move |request| {
                            let _wait = gate.lock().unwrap();
                            read.lock().unwrap().push(request.key.clone());
                            Some(picture(1))
                        }
                    },
                    |picture| picture.width,
                );
                // Every worker is held on one request each, so the rest wait in the queue.
                let busy: Vec<Slot> = (0..WORKERS).map(|_| Slot::default()).collect();
                for (at, slot) in busy.iter().enumerate() {
                    loader.load(slot, request(&format!("busy{at}")), |_| {});
                }
                std::thread::sleep(std::time::Duration::from_millis(100));
                let slot = Slot::default();
                loader.load(&slot, request("gone"), |_| panic!("delivered after release"));
                loader.release(&slot);
                drop(held);
                until(&context, || loader.kept() == WORKERS);
                std::thread::sleep(std::time::Duration::from_millis(100));
                assert!(!read.lock().unwrap().contains(&"gone".to_string()));
            })
            .unwrap();
    }
}
