use super::*;
use gtk::prelude::*;
use std::{
    cell::{Cell, RefCell},
    rc::Rc,
    time::Duration,
};
use webkit::prelude::*;

const SLIDE_WIDTH: i32 = 960;
const SLIDE_HEIGHT: i32 = 540;
const THUMB_WIDTH: i32 = 320;
const THUMB_HEIGHT: i32 = 180;
const GAP: i32 = 20;
const LABEL_HEIGHT: i32 = 34;
const COLUMNS: i32 = 4;

struct ReviewJob {
    deck: Deck,
    directory: PathBuf,
    view: webkit::WebView,
    _window: gtk::Window,
    main_loop: gtk::glib::MainLoop,
    index: Cell<usize>,
    processing: Cell<bool>,
    error: RefCell<Option<ApiError>>,
    slides: RefCell<Vec<serde_json::Value>>,
    sheet: gtk::cairo::ImageSurface,
}

impl ReviewJob {
    fn fail(&self, error: impl std::fmt::Display) {
        *self.error.borrow_mut() = Some(error.to_string());
        self.main_loop.quit();
    }

    fn load(self: &Rc<Self>) {
        if self.index.get() == self.deck.slides.len() {
            self.main_loop.quit();
            return;
        }
        self.processing.set(false);
        let html = match review_html(&self.deck, self.index.get()) {
            Ok(html) => html,
            Err(error) => return self.fail(error),
        };
        self.view.load_html(&html, None);
        let job = self.clone();
        let index = self.index.get();
        gtk::glib::timeout_add_local_once(Duration::from_secs(20), move || {
            if job.index.get() == index && job.error.borrow().is_none() {
                job.fail(format!("Timed out rendering slide {}", index + 1));
            }
        });
    }

    fn capture(self: &Rc<Self>) {
        if self.processing.replace(true) {
            return;
        }
        let job = self.clone();
        self.view.evaluate_javascript(
            "JSON.stringify(window.__hfReview || {})",
            None,
            None,
            None::<&gtk::gio::Cancellable>,
            move |result| {
                let diagnostics = result.ok().and_then(|value| {
                    serde_json::from_str::<serde_json::Value>(&value.to_string()).ok()
                });
                let Some(diagnostics) = diagnostics else {
                    return job.fail("Could not inspect the rendered slide");
                };
                let job_for_snapshot = job.clone();
                job.view.snapshot(
                    webkit::SnapshotRegion::Visible,
                    webkit::SnapshotOptions::NONE,
                    None::<&gtk::gio::Cancellable>,
                    move |result| {
                        let surface = match result {
                            Ok(surface) => surface,
                            Err(error) => return job_for_snapshot.fail(error),
                        };
                        if let Err(error) = job_for_snapshot.record(&surface, &diagnostics) {
                            return job_for_snapshot.fail(error);
                        }
                        job_for_snapshot.index.set(job_for_snapshot.index.get() + 1);
                        job_for_snapshot.load();
                    },
                );
            },
        );
    }

    fn record(
        &self,
        texture: &gtk::gdk::Texture,
        diagnostics: &serde_json::Value,
    ) -> Result<(), ApiError> {
        let index = self.index.get();
        let source_width = texture.width();
        let source_height = texture.height();
        let slide = &self.deck.slides[index];
        let filename = format!("slide-{:02}-{}.png", index + 1, slide.id);
        let path = self.directory.join(&filename);
        let png = texture.save_to_png_bytes();
        let pixbuf = gtk::gdk_pixbuf::Pixbuf::from_read(std::io::Cursor::new(png.to_vec()))
            .map_err(internal)?;
        let resized = pixbuf
            .scale_simple(
                SLIDE_WIDTH,
                SLIDE_HEIGHT,
                gtk::gdk_pixbuf::InterpType::Bilinear,
            )
            .ok_or("Could not resize slide image")?;
        write_private(
            &path,
            &resized.save_to_bufferv("png", &[]).map_err(internal)?,
        )?;

        let column = index as i32 % COLUMNS;
        let row = index as i32 / COLUMNS;
        let x = GAP + column * (THUMB_WIDTH + GAP);
        let y = GAP + row * (THUMB_HEIGHT + LABEL_HEIGHT + GAP);
        let cr = gtk::cairo::Context::new(&self.sheet).map_err(internal)?;
        cr.save().map_err(internal)?;
        cr.translate(f64::from(x), f64::from(y));
        cr.scale(
            f64::from(THUMB_WIDTH) / f64::from(source_width),
            f64::from(THUMB_HEIGHT) / f64::from(source_height),
        );
        cr.set_source_pixbuf(&pixbuf, 0.0, 0.0);
        cr.paint().map_err(internal)?;
        cr.restore().map_err(internal)?;
        cr.set_source_rgb(0.95, 0.96, 0.98);
        cr.select_font_face(
            "Sans",
            gtk::cairo::FontSlant::Normal,
            gtk::cairo::FontWeight::Normal,
        );
        cr.set_font_size(15.0);
        cr.move_to(f64::from(x), f64::from(y + THUMB_HEIGHT + 23));
        let title: String = slide.title.chars().take(32).collect();
        cr.show_text(&format!("{:02}  {}", index + 1, title.replace('\n', " ")))
            .map_err(internal)?;

        let mut issues = Vec::new();
        for item in diagnostics["overflow"].as_array().into_iter().flatten() {
            issues.push(serde_json::json!({"code":"clipped","severity":"warning","elementId":item["id"],"elementKind":item["kind"]}));
        }
        for item in diagnostics["overlap"].as_array().into_iter().flatten() {
            issues.push(serde_json::json!({"code":"overlap","severity":"warning","firstId":item["firstId"],"secondId":item["secondId"],"firstKind":item["firstKind"],"secondKind":item["secondKind"]}));
        }
        for item in diagnostics["missingImages"]
            .as_array()
            .into_iter()
            .flatten()
        {
            issues.push(serde_json::json!({"code":"image_missing","severity":"error","elementId":item["id"],"elementKind":item["kind"]}));
        }
        self.slides.borrow_mut().push(serde_json::json!({
            "slideNumber": index + 1,
            "slideId": slide.id,
            "title": slide.title,
            "image": path,
            "issues": issues
        }));
        Ok(())
    }

    fn report(&self) -> Result<serde_json::Value, ApiError> {
        let sheet_width = GAP + COLUMNS * (THUMB_WIDTH + GAP);
        let rows = (self.deck.slides.len() as i32 + COLUMNS - 1) / COLUMNS;
        let sheet_height = GAP + rows * (THUMB_HEIGHT + LABEL_HEIGHT + GAP);
        self.sheet.flush();
        let pixbuf =
            gtk::gdk::pixbuf_get_from_surface(&self.sheet, 0, 0, sheet_width, sheet_height)
                .ok_or("Could not capture contact sheet pixels")?;
        let contact_sheet = self.directory.join("contact-sheet.png");
        write_private(
            &contact_sheet,
            &pixbuf.save_to_bufferv("png", &[]).map_err(internal)?,
        )?;
        let slides = self.slides.borrow();
        let issue_count: usize = slides
            .iter()
            .map(|slide| slide["issues"].as_array().map_or(0, Vec::len))
            .sum();
        let report_path = self.directory.join("report.json");
        let report = serde_json::json!({
            "reportVersion": 1,
            "deckId": self.deck.id,
            "directory": self.directory,
            "contactSheet": contact_sheet,
            "report": report_path,
            "slideCount": slides.len(),
            "issueCount": issue_count,
            "ok": issue_count == 0,
            "slides": *slides
        });
        write_private(
            &report_path,
            &serde_json::to_vec_pretty(&report).map_err(internal)?,
        )?;
        Ok(report)
    }
}

pub(super) fn run(deck: &Deck, directory: &Path) -> Result<serde_json::Value, ApiError> {
    validate_deck(deck)?;
    let existed = directory.exists();
    fs::create_dir_all(directory).map_err(internal)?;
    if !existed {
        fs::set_permissions(directory, fs::Permissions::from_mode(0o700)).map_err(internal)?;
    }
    let directory = fs::canonicalize(directory).map_err(internal)?;
    gtk::init().map_err(internal)?;
    let window = gtk::Window::new();
    window.set_default_size(SLIDE_WIDTH, SLIDE_HEIGHT);
    let view = webkit::WebView::new();
    if let Some(settings) = webkit::prelude::WebViewExt::settings(&view) {
        settings.set_hardware_acceleration_policy(webkit::HardwareAccelerationPolicy::Never);
    }
    view.set_size_request(SLIDE_WIDTH, SLIDE_HEIGHT);
    window.set_child(Some(&view));
    window.present();
    // WebKit needs a mapped surface for snapshots; keep CLI review out of the way.
    window.minimize();
    let sheet_width = GAP + COLUMNS * (THUMB_WIDTH + GAP);
    let rows = (deck.slides.len() as i32 + COLUMNS - 1) / COLUMNS;
    let sheet_height = GAP + rows * (THUMB_HEIGHT + LABEL_HEIGHT + GAP);
    let sheet =
        gtk::cairo::ImageSurface::create(gtk::cairo::Format::ARgb32, sheet_width, sheet_height)
            .map_err(internal)?;
    let cr = gtk::cairo::Context::new(&sheet).map_err(internal)?;
    cr.set_source_rgb(0.07, 0.09, 0.13);
    cr.paint().map_err(internal)?;
    let job = Rc::new(ReviewJob {
        deck: deck.clone(),
        directory,
        view: view.clone(),
        _window: window,
        main_loop: gtk::glib::MainLoop::new(None, false),
        index: Cell::new(0),
        processing: Cell::new(false),
        error: RefCell::new(None),
        slides: RefCell::new(Vec::new()),
        sheet,
    });
    let on_title = job.clone();
    view.connect_notify_local(Some("title"), move |view, _| {
        if view.title().as_deref() == Some("HyperFrames review ready") {
            on_title.capture();
        }
    });
    let on_failure = job.clone();
    view.connect_load_failed(move |_, _, _, error| {
        on_failure.fail(error);
        false
    });
    job.load();
    job.main_loop.run();
    if let Some(error) = job.error.borrow_mut().take() {
        return Err(error);
    }
    job.report()
}
