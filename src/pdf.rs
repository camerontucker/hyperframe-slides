use super::*;
use gtk::prelude::*;
use std::os::unix::fs::DirBuilderExt;
use std::{
    cell::{Cell, RefCell},
    rc::Rc,
    time::Duration,
};
use webkit::prelude::*;

pub(super) fn parse_exclusions(options: &[&str]) -> Result<Vec<String>, ApiError> {
    let mut ids = Vec::new();
    for pair in options.chunks(2) {
        if pair.len() != 2 || pair[0] != "--exclude" {
            return Err("Use deck export-pdf ID FILE [--exclude SLIDE_ID]...".into());
        }
        if !valid_id(pair[1]) {
            return Err("Invalid excluded slide ID".into());
        }
        if !ids.iter().any(|id| id == pair[1]) {
            ids.push(pair[1].to_owned());
        }
    }
    Ok(ids)
}

fn selected_deck(deck: &Deck, excluded: &[String]) -> Result<Deck, ApiError> {
    for id in excluded {
        slide_index_for_export(deck, id)?;
    }
    let mut selected = deck.clone();
    selected
        .slides
        .retain(|slide| !excluded.contains(&slide.id));
    if selected.slides.is_empty() {
        return Err("PDF export must include at least one slide".into());
    }
    // Notes are never part of a shareable PDF.
    for slide in &mut selected.slides {
        slide.notes.clear();
    }
    Ok(selected)
}

fn slide_index_for_export(deck: &Deck, id: &str) -> Result<(), ApiError> {
    if deck.slides.iter().any(|slide| slide.id == id) {
        Ok(())
    } else {
        Err(format!("Unknown excluded slide: {id}"))
    }
}

struct Scratch(PathBuf);
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

pub(super) fn export(
    deck: &Deck,
    source_revision: &str,
    path: &Path,
    excluded: &[String],
) -> Result<serde_json::Value, ApiError> {
    let selected = selected_deck(deck, excluded)?;
    validate_deck(&selected)?;
    if path.file_name().is_none() || path.is_dir() {
        return Err("PDF output must be a file path".into());
    }
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(internal)?
        .as_nanos();
    let scratch_path =
        env::temp_dir().join(format!("hyperframe-pdf-{}-{nonce}", std::process::id()));
    // Exclusive creation and private permissions keep presentation images private.
    fs::DirBuilder::new()
        .mode(0o700)
        .create(&scratch_path)
        .map_err(internal)?;
    let scratch = Scratch(scratch_path);
    let report = review::run(&selected, source_revision, &scratch.0)?;
    let slides = report["slides"]
        .as_array()
        .ok_or("Invalid rendered PDF slides")?;
    if slides.iter().any(|slide| {
        slide["issues"]
            .as_array()
            .is_some_and(|issues| issues.iter().any(|issue| issue["severity"] == "error"))
    }) {
        return Err("PDF export failed: a slide image could not be loaded".into());
    }
    let mut printable = selected.clone();
    compact_images(&mut printable)?;
    let bytes = print_document(&printable, &scratch.0)?;
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    fs::create_dir_all(parent).map_err(internal)?;
    write_private(path, &bytes)?;
    let warnings: Vec<_> = slides
        .iter()
        .filter(|slide| {
            slide["issues"]
                .as_array()
                .is_some_and(|issues| !issues.is_empty())
        })
        .map(|slide| serde_json::json!({"slideId":slide["slideId"],"issues":slide["issues"]}))
        .collect();
    Ok(serde_json::json!({
        "id": deck.id, "sourceRevision": source_revision,
        "path": fs::canonicalize(path).map_err(internal)?,
        "pageCount": selected.slides.len(), "excludedSlideIds": excluded,
        "includedSlideIds": selected.slides.iter().map(|slide| &slide.id).collect::<Vec<_>>(),
        "bytes": bytes.len(), "warnings": warnings
    }))
}

// Resize only the export copy. Vector text/fonts stay untouched; photographic
// pixels beyond the slide's display size make no useful contribution to a PDF.
fn compact_images(deck: &mut Deck) -> Result<(), ApiError> {
    fn compact(uri: &mut String, width: i32, height: i32) -> Result<(), ApiError> {
        let encoded = uri.split_once(',').ok_or("Invalid PDF image")?.1;
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(encoded)
            .map_err(internal)?;
        let pixbuf =
            gtk::gdk_pixbuf::Pixbuf::from_read(std::io::Cursor::new(bytes)).map_err(internal)?;
        let scale = (width as f64 / pixbuf.width() as f64)
            .min(height as f64 / pixbuf.height() as f64)
            .min(1.0);
        let resized = pixbuf
            .scale_simple(
                (pixbuf.width() as f64 * scale).round().max(1.0) as i32,
                (pixbuf.height() as f64 * scale).round().max(1.0) as i32,
                gtk::gdk_pixbuf::InterpType::Bilinear,
            )
            .ok_or("Could not resize PDF image")?;
        let (format, mime) = if resized.has_alpha() {
            ("png", "png")
        } else {
            ("jpeg", "jpeg")
        };
        let options: &[(&str, &str)] = if format == "jpeg" {
            &[("quality", "85")]
        } else {
            &[]
        };
        let bytes = resized.save_to_bufferv(format, options).map_err(internal)?;
        *uri = format!(
            "data:image/{mime};base64,{}",
            base64::engine::general_purpose::STANDARD.encode(bytes)
        );
        Ok(())
    }
    if let Some(logo) = &mut deck.template.logo {
        compact(logo, 480, 240)?;
    }
    for slide in &mut deck.slides {
        for image in &mut slide.images {
            let (width, height) = if image.background {
                (960, 540)
            } else {
                (
                    (960.0 * image.width / 100.0).ceil() as i32,
                    (810.0 * image.height / 100.0).ceil() as i32,
                )
            };
            compact(&mut image.data_uri, width.max(1), height.max(1))?;
        }
    }
    Ok(())
}

// Layout zoom keeps WebKit pagination in the scaled coordinate system.
// A CSS transform scales only painting, causing blank space at page boundaries
// inside lists even when the finished slide fits entirely on one PDF page.
fn print_html(deck: &Deck) -> String {
    let mut html = format!("<!doctype html><html><head><meta charset=\"utf-8\"><title>{}</title><style>{}html,body{{height:auto;margin:0;padding:0;width:1280px;background:white}}@page{{size:1280px 720px;margin:0}}.pdf-page{{position:relative;width:1280px;height:720px;overflow:hidden;break-after:page;page-break-after:always}}.pdf-page:last-child{{break-after:auto;page-break-after:auto}}.pdf-stage{{position:absolute;top:0;left:0;width:1920px;height:1080px;zoom:.6666666667}}*{{-webkit-print-color-adjust:exact;print-color-adjust:exact}}</style></head><body>", escape_html(&deck.title), slide_css(deck));
    for (index, slide) in deck.slides.iter().enumerate() {
        html.push_str("<section class=\"pdf-page\"><div class=\"pdf-stage\">");
        html.push_str(&slide_html(deck, slide, index, deck.slides.len()));
        html.push_str("</div></section>");
    }
    html.push_str("<script>Promise.all([document.fonts.ready,...[...document.images].map(img=>img.decode?img.decode():Promise.resolve())]).then(()=>{for(const scene of document.querySelectorAll('.slide')){const body=scene.querySelector('.body');if(!body)continue;const stage=scene.parentElement.getBoundingClientRect();const protectedTop=Math.min(...[...scene.querySelectorAll('.template-footer,.slide-number')].map(el=>el.getBoundingClientRect().top),stage.bottom);const original=parseFloat(getComputedStyle(body).fontSize);let size=original;while(body.getBoundingClientRect().bottom>protectedTop-16 && size>original*.7){size-=1;body.style.fontSize=size+'px'}}document.title='HyperFrames PDF ready'}).catch(()=>{document.title='HyperFrames PDF image failed'})</script></body></html>");
    html
}

fn print_document(deck: &Deck, scratch: &Path) -> Result<Vec<u8>, ApiError> {
    gtk::init().map_err(internal)?;
    let window = gtk::Window::new();
    window.set_default_size(960, 540);
    let view = webkit::WebView::new();
    if let Some(settings) = webkit::prelude::WebViewExt::settings(&view) {
        settings.set_print_backgrounds(true);
        settings.set_hardware_acceleration_policy(webkit::HardwareAccelerationPolicy::Never);
    }
    window.set_child(Some(&view));
    window.present();
    window.minimize();
    let loop_ = gtk::glib::MainLoop::new(None, false);
    let error = Rc::new(RefCell::new(None::<String>));
    let started = Rc::new(Cell::new(false));
    let output_path = scratch.join("presentation.pdf");
    let operation = webkit::PrintOperation::new(&view);
    let settings = gtk::PrintSettings::new();
    settings.set_printer("Print to File");
    settings.set("output-file-format", Some("pdf"));
    settings.set(
        "output-uri",
        Some(&gtk::gio::File::for_path(&output_path).uri()),
    );
    let setup = gtk::PageSetup::new();
    setup.set_paper_size(&gtk::PaperSize::new_custom(
        "slides",
        "Slides",
        960.0,
        540.0,
        gtk::Unit::Points,
    ));
    setup.set_top_margin(0.0, gtk::Unit::Points);
    setup.set_bottom_margin(0.0, gtk::Unit::Points);
    setup.set_left_margin(0.0, gtk::Unit::Points);
    setup.set_right_margin(0.0, gtk::Unit::Points);
    operation.set_print_settings(&settings);
    operation.set_page_setup(&setup);
    let failure = error.clone();
    operation.connect_failed(move |_, problem| *failure.borrow_mut() = Some(problem.to_string()));
    let finished = loop_.clone();
    operation.connect_finished(move |_| finished.quit());
    let print = operation.clone();
    let ready = started.clone();
    let failure = error.clone();
    let failed_loop = loop_.clone();
    let title_script = format!(
        "document.title={}",
        serde_json::to_string(&deck.title).map_err(internal)?
    );
    let signal = view.connect_notify_local(Some("title"), move |view, _| {
        if view.title().as_deref() == Some("HyperFrames PDF ready") && !ready.replace(true) {
            let print = print.clone();
            view.evaluate_javascript(
                &title_script,
                None,
                None,
                None::<&gtk::gio::Cancellable>,
                move |_| print.print(),
            );
        } else if view.title().as_deref() == Some("HyperFrames PDF image failed") {
            *failure.borrow_mut() = Some("PDF image could not be decoded".into());
            failed_loop.quit();
        }
    });
    let timed_loop = loop_.clone();
    let failure = error.clone();
    let timeout = gtk::glib::timeout_add_local_once(Duration::from_secs(30), move || {
        *failure.borrow_mut() = Some("Timed out printing PDF".into());
        timed_loop.quit();
    });
    view.load_html(&print_html(deck), None);
    loop_.run();
    let timed_out = error.borrow().as_deref() == Some("Timed out printing PDF");
    if !timed_out {
        timeout.remove();
    }
    view.disconnect(signal);
    window.close();
    if let Some(error) = error.borrow_mut().take() {
        return Err(error);
    }
    let bytes = fs::read(output_path).map_err(internal)?;
    if !bytes.starts_with(b"%PDF-") {
        return Err("Native printer produced invalid PDF data".into());
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn exclusions_are_explicit_and_invalid_selections_fail() {
        assert!(parse_exclusions(&["--exclude"]).is_err());
        assert!(parse_exclusions(&["--unknown", "one"]).is_err());
        assert!(parse_exclusions(&["--exclude", "../one"]).is_err());
        assert_eq!(
            parse_exclusions(&["--exclude", "one", "--exclude", "one"]).unwrap(),
            vec!["one"]
        );
        let deck: Deck = serde_json::from_value(serde_json::json!({
            "schemaVersion":1,"id":"test","title":"PDF test","theme":"paper","updatedAt":1,
            "slides":[{"id":"one","layout":"title","eyebrow":"","title":"First","body":"","notes":"Private"},
                      {"id":"two","layout":"title","eyebrow":"","title":"Second","body":"","notes":"Private"}]
        })).unwrap();
        assert!(selected_deck(&deck, &["missing".into()]).is_err());
        assert!(selected_deck(&deck, &["one".into(), "two".into()]).is_err());
        let selected = selected_deck(&deck, &["one".into()]).unwrap();
        assert_eq!(selected.slides[0].id, "two");
        assert!(selected.slides[0].notes.is_empty());
        assert_eq!(deck.slides.len(), 2);
        assert_eq!(deck.slides[1].notes, "Private");
    }
}
