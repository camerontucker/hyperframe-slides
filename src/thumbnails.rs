use super::{review_html, Deck};
use gtk::prelude::*;
use sha2::{Digest, Sha256};
use std::{
    cell::{Cell, RefCell},
    collections::{HashMap, VecDeque},
    rc::Rc,
    time::Duration,
};
use webkit::prelude::*;

struct Job {
    key: String,
    html: String,
}

pub(super) struct ThumbnailWorker {
    view: webkit::WebView,
    queue: RefCell<VecDeque<Job>>,
    active: RefCell<Option<String>>,
    cache: RefCell<HashMap<String, gtk::gdk::Texture>>,
    targets: RefCell<HashMap<String, Vec<gtk::Picture>>>,
    started: Cell<bool>,
}

pub(super) fn key(deck: &Deck, index: usize) -> String {
    let bytes = serde_json::to_vec(&(
        &deck.id,
        &deck.theme,
        &deck.template,
        &deck.slides[index],
        index,
        deck.slides.len(),
    ))
    .expect("slide data is serializable");
    format!("{:x}", Sha256::digest(bytes))
}

impl ThumbnailWorker {
    pub(super) fn new(view: &webkit::WebView) -> Rc<Self> {
        let worker = Rc::new(Self {
            view: view.clone(),
            queue: RefCell::new(VecDeque::new()),
            active: RefCell::new(None),
            cache: RefCell::new(HashMap::new()),
            targets: RefCell::new(HashMap::new()),
            started: Cell::new(false),
        });
        let weak = Rc::downgrade(&worker);
        view.connect_notify_local(Some("title"), move |view, _| {
            let Some(worker) = weak.upgrade() else { return };
            let Some(title) = view.title() else { return };
            let Some(key) = title.strip_prefix("HyperFrames thumb ready ") else {
                return;
            };
            if worker.active.borrow().as_deref() != Some(key) {
                return;
            }
            let key = key.to_string();
            let on_snapshot = worker.clone();
            view.snapshot(
                webkit::SnapshotRegion::Visible,
                webkit::SnapshotOptions::NONE,
                None::<&gtk::gio::Cancellable>,
                move |result| {
                    if on_snapshot.active.borrow().as_deref() != Some(&key) {
                        return;
                    }
                    if let Ok(texture) = result {
                        if on_snapshot.cache.borrow().len() >= 96 {
                            on_snapshot.cache.borrow_mut().clear();
                        }
                        if let Some(pictures) = on_snapshot.targets.borrow().get(&key) {
                            for picture in pictures {
                                picture.set_paintable(Some(&texture));
                                picture.set_visible(true);
                            }
                        }
                        on_snapshot.cache.borrow_mut().insert(key.clone(), texture);
                    }
                    on_snapshot.active.borrow_mut().take();
                    on_snapshot.pump();
                },
            );
        });
        worker
    }

    pub(super) fn clear_targets(&self) {
        self.targets.borrow_mut().clear();
        self.queue.borrow_mut().clear();
    }

    pub(super) fn forget_target(&self, picture: &gtk::Picture) {
        // A picture reused by the change-review dialog may have an older render in flight.
        // Remove that target before showing a cached or newly rendered slide.
        for pictures in self.targets.borrow_mut().values_mut() {
            pictures.retain(|target| target != picture);
        }
    }

    pub(super) fn request(self: &Rc<Self>, deck: &Deck, index: usize, picture: &gtk::Picture) {
        let key = key(deck, index);
        self.forget_target(picture);
        if let Some(texture) = self.cache.borrow().get(&key) {
            picture.set_paintable(Some(texture));
            picture.set_visible(true);
            return;
        }
        self.targets
            .borrow_mut()
            .entry(key.clone())
            .or_default()
            .push(picture.clone());
        if self.active.borrow().as_deref() == Some(&key)
            || self.queue.borrow().iter().any(|job| job.key == key)
        {
            return;
        }
        if let Ok(html) = review_html(deck, index) {
            self.queue.borrow_mut().push_back(Job {
                html: html.replace(
                    "HyperFrames review ready",
                    &format!("HyperFrames thumb ready {key}"),
                ),
                key,
            });
            self.pump();
        }
    }

    pub(super) fn start(self: &Rc<Self>) {
        if self.started.replace(true) {
            return;
        }
        self.pump();
    }

    fn pump(self: &Rc<Self>) {
        if !self.started.get() || self.active.borrow().is_some() {
            return;
        }
        let Some(job) = self.queue.borrow_mut().pop_front() else {
            return;
        };
        let key = job.key.clone();
        *self.active.borrow_mut() = Some(key.clone());
        self.view.load_html(&job.html, None);
        let weak = Rc::downgrade(self);
        gtk::glib::timeout_add_local_once(Duration::from_secs(20), move || {
            let Some(worker) = weak.upgrade() else { return };
            if worker.active.borrow().as_deref() == Some(&key) {
                worker.active.borrow_mut().take();
                worker.view.stop_loading();
                worker.pump();
            }
        });
    }
}
