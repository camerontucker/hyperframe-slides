use super::*;
use gtk::{Orientation, ResponseType};
use javascriptcore::ValueExt;
use std::{
    cell::{Cell, RefCell},
    io::{Read, Write},
    os::unix::{
        fs::{MetadataExt, PermissionsExt},
        net::UnixListener,
    },
    rc::Rc,
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        mpsc, Arc,
    },
};
use webkit2gtk::{
    SecurityManagerExt, SettingsExt, URISchemeRequestExt, UserContentManagerExt, WebContextExt,
};

fn configure_acceleration(view: &webkit2gtk::WebView) {
    if let Some(settings) = webkit2gtk::WebViewExt::settings(view) {
        settings.set_hardware_acceleration_policy(webkit2gtk::HardwareAccelerationPolicy::Always);
        settings.set_enable_webgl(true);
    }
}

fn active_render_nodes() -> Vec<String> {
    let mut pending = vec![std::process::id()];
    let mut seen = std::collections::HashSet::new();
    let mut nodes = std::collections::BTreeSet::new();
    while let Some(pid) = pending.pop() {
        if !seen.insert(pid) {
            continue;
        }
        if let Ok(children) = fs::read_to_string(format!("/proc/{pid}/task/{pid}/children")) {
            pending.extend(
                children
                    .split_whitespace()
                    .filter_map(|value| value.parse::<u32>().ok()),
            );
        }
        if let Ok(entries) = fs::read_dir(format!("/proc/{pid}/fd")) {
            for entry in entries.flatten() {
                if let Ok(path) = fs::read_link(entry.path()) {
                    if let Some(name) = path.file_name().and_then(|name| name.to_str()) {
                        if name.starts_with("renderD") && path.starts_with("/dev/dri") {
                            nodes.insert(name.to_string());
                        }
                    }
                }
            }
        }
    }
    nodes.into_iter().collect()
}

fn gpu_diagnostics(value: &mut serde_json::Value) {
    let nodes = active_render_nodes();
    let amd = nodes.iter().any(|name| {
        fs::read_to_string(format!("/sys/class/drm/{name}/device/vendor"))
            .is_ok_and(|vendor| vendor.trim() == "0x1002")
    });
    if let Some(object) = value.as_object_mut() {
        object.insert("drmRenderNodesInUse".into(), serde_json::json!(nodes));
        object.insert("amdGpuInUse".into(), serde_json::json!(amd));
        object.insert(
            "rendererIdentityMayBeMasked".into(),
            serde_json::json!(true),
        );
    }
}

struct Editor {
    state: AppState,
    deck: RefCell<Deck>,
    last_disk: RefCell<Vec<u8>>,
    last_disk_stamp: Cell<Option<(u64, i64, i64, u64)>>,
    selected: Cell<usize>,
    loading: Cell<bool>,
    dirty: Cell<bool>,
    save_source: RefCell<Option<gtk::glib::SourceId>>,
    list: gtk::ListBox,
    deck_title: gtk::Entry,
    theme: gtk::ComboBoxText,
    template_header: gtk::Entry,
    template_footer: gtk::Entry,
    template_outline: gtk::CheckButton,
    font_status: gtk::Label,
    layout: gtk::ComboBoxText,
    animation: gtk::ComboBoxText,
    eyebrow: gtk::Entry,
    headline: gtk::TextView,
    body: gtk::TextView,
    notes: gtk::TextView,
    picture_count: gtk::Label,
    preview: webkit2gtk::WebView,
    status: gtk::Label,
}

fn file_stamp(path: &Path) -> Option<(u64, i64, i64, u64)> {
    fs::metadata(path)
        .ok()
        .map(|meta| (meta.ino(), meta.mtime(), meta.mtime_nsec(), meta.len()))
}

fn sample_deck() -> Deck {
    let id = format!("deck-{}", unique_id());
    Deck {
        schema_version: 1,
        id,
        title: "Untitled presentation".into(),
        theme: "midnight".into(),
        template: SlideTemplate::default(),
        updated_at: now(),
        slides: vec![Slide {
            id: "slide-1".into(),
            layout: "title".into(),
            eyebrow: "A NEW PRESENTATION".into(),
            title: "Make your point beautifully.".into(),
            body: "A presentation made with HyperFrames Slides".into(),
            notes: "Add speaker notes here. Only you will see them in presenter mode.".into(),
            animation: default_animation(),
            images: Vec::new(),
        }],
    }
}

fn unique_id() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos()
}

fn recent_decks(state: &AppState) -> Vec<Deck> {
    let mut decks = Vec::new();
    if let Ok(entries) = fs::read_dir(state.data_dir.join("decks")) {
        for entry in entries.flatten() {
            if entry.path().extension().and_then(|x| x.to_str()) != Some("json") {
                continue;
            }
            if let Ok(bytes) = fs::read(entry.path()) {
                if let Ok(deck) = serde_json::from_slice::<Deck>(&bytes) {
                    if validate_draft(&deck).is_ok()
                        && entry.path().file_stem().and_then(|x| x.to_str()) == Some(&deck.id)
                    {
                        decks.push(deck);
                    }
                }
            }
        }
    }
    decks.sort_by_key(|deck| std::cmp::Reverse(deck.updated_at));
    decks
}

fn text(view: &gtk::TextView) -> String {
    let buffer = view.buffer().expect("TextView buffer");
    buffer
        .text(&buffer.start_iter(), &buffer.end_iter(), true)
        .map(|v| v.to_string())
        .unwrap_or_default()
}

fn set_text(view: &gtk::TextView, value: &str) {
    view.buffer().expect("TextView buffer").set_text(value);
}

fn format_selection(view: &gtk::TextView, style: &str) {
    let buffer = view.buffer().expect("TextView buffer");
    if let Some((mut start, mut end)) = buffer.selection_bounds() {
        let selected = buffer.text(&start, &end, true).unwrap_or_default();
        let replacement = if style == "bullet" {
            selected
                .lines()
                .map(|line| format!("- {line}"))
                .collect::<Vec<_>>()
                .join("\n")
        } else {
            format!("{style}{selected}{style}")
        };
        buffer.delete(&mut start, &mut end);
        buffer.insert(&mut start, &replacement);
    } else {
        if style == "bullet" {
            buffer.insert_at_cursor("- ");
        } else {
            buffer.insert_at_cursor(&format!("{style}{style}"));
            let cursor =
                buffer.iter_at_offset(buffer.cursor_position() - style.chars().count() as i32);
            buffer.place_cursor(&cursor);
        }
    }
    view.grab_focus();
}

fn editor_label(text: &str) -> gtk::Label {
    let label = gtk::Label::new(Some(text));
    label.set_xalign(0.0);
    label.style_context().add_class("field-label");
    label
}

fn text_field(parent: &gtk::Box, label: &str, lines: i32) -> gtk::TextView {
    parent.pack_start(&editor_label(label), false, false, 0);
    let view = gtk::TextView::new();
    view.set_wrap_mode(gtk::WrapMode::WordChar);
    view.set_accepts_tab(false);
    view.set_size_request(-1, lines * 25 + 12);
    let frame = gtk::Frame::new(None);
    frame.add(&view);
    parent.pack_start(&frame, false, false, 0);
    view
}

impl Editor {
    fn schedule_persist(self: &Rc<Self>) {
        if self.loading.get() {
            return;
        }
        self.dirty.set(true);
        self.status.set_text("Unsaved changes");
        if let Some(source) = self.save_source.borrow_mut().take() {
            source.remove();
        }
        let editor = self.clone();
        let source =
            gtk::glib::timeout_add_local_once(std::time::Duration::from_millis(300), move || {
                editor.save_source.borrow_mut().take();
                let _ = editor.persist();
            });
        *self.save_source.borrow_mut() = Some(source);
    }

    fn persist(&self) -> Result<(), ApiError> {
        if self.loading.get() {
            return Ok(());
        }
        if let Some(source) = self.save_source.borrow_mut().take() {
            source.remove();
        }
        let mut deck = self.deck.borrow_mut();
        let index = self.selected.get();
        let old_title = deck.slides.get(index).map(|slide| slide.title.clone());
        let old_visible = (
            deck.theme.clone(),
            deck.template.header.clone(),
            deck.template.footer.clone(),
            deck.slides.get(index).map(|slide| {
                (
                    slide.layout.clone(),
                    slide.eyebrow.clone(),
                    slide.title.clone(),
                    slide.body.clone(),
                )
            }),
        );
        deck.title = self.deck_title.text().to_string();
        deck.theme = self
            .theme
            .active_id()
            .map(|s| s.to_string())
            .unwrap_or_else(|| "midnight".into());
        deck.template.header = self.template_header.text().to_string();
        deck.template.footer = self.template_footer.text().to_string();
        deck.template.show_outline = self.template_outline.is_active();
        if let Some(slide) = deck.slides.get_mut(index) {
            slide.layout = self
                .layout
                .active_id()
                .map(|s| s.to_string())
                .unwrap_or_else(|| "title".into());
            slide.animation = self
                .animation
                .active_id()
                .map(|s| s.to_string())
                .unwrap_or_else(default_animation);
            slide.eyebrow = self.eyebrow.text().to_string();
            slide.title = text(&self.headline);
            slide.body = text(&self.body);
            slide.notes = text(&self.notes);
        }
        if deck.title.trim().is_empty() {
            deck.title = "Untitled presentation".into();
        }
        deck.updated_at = now();
        self.dirty.set(true);
        let result = (|| {
            let _write_lock = lock_deck_writes(&self.state)?;
            let path = deck_path(&self.state, &deck.id)?;
            let disk = fs::read(&path).map_err(internal)?;
            if disk != *self.last_disk.borrow() {
                return Err("Deck changed on disk. Save a copy of your edits or reload the agent's version.".into());
            }
            let bytes = write_deck_unlocked(&self.state, &deck)?;
            Ok((bytes, file_stamp(&path)))
        })();
        match &result {
            Ok((bytes, stamp)) => {
                self.dirty.set(false);
                *self.last_disk.borrow_mut() = bytes.clone();
                self.last_disk_stamp.set(*stamp);
                match validate_deck(&deck) {
                    Ok(_) => self.status.set_text("Saved locally"),
                    Err(error) => self
                        .status
                        .set_text(&format!("Saved draft · cannot present: {error}")),
                }
            }
            Err(error) => self
                .status
                .set_text(&format!("Unsaved changes · save failed: {error}")),
        }
        drop(deck);
        let deck = self.deck.borrow();
        let new_slide = deck.slides.get(index);
        let list_changed = old_title.as_deref() != new_slide.map(|slide| slide.title.as_str());
        let preview_changed = old_visible
            != (
                deck.theme.clone(),
                deck.template.header.clone(),
                deck.template.footer.clone(),
                new_slide.map(|slide| {
                    (
                        slide.layout.clone(),
                        slide.eyebrow.clone(),
                        slide.title.clone(),
                        slide.body.clone(),
                    )
                }),
            );
        drop(deck);
        if list_changed {
            self.refresh_list();
        }
        if preview_changed {
            self.refresh_preview();
        }
        result.map(|_| ())
    }

    fn refresh_list(&self) {
        self.loading.set(true);
        for row in self.list.children() {
            self.list.remove(&row);
        }
        let deck = self.deck.borrow();
        for (index, slide) in deck.slides.iter().enumerate() {
            let row = gtk::ListBoxRow::new();
            let box_ = gtk::Box::new(Orientation::Vertical, 2);
            box_.set_margin_top(12);
            box_.set_margin_bottom(12);
            box_.set_margin_start(14);
            box_.set_margin_end(14);
            let count = gtk::Label::new(Some(&format!("SLIDE {:02}", index + 1)));
            count.set_xalign(0.0);
            count.style_context().add_class("slide-count");
            let title = gtk::Label::new(Some(if slide.title.trim().is_empty() {
                "Untitled slide"
            } else {
                &slide.title
            }));
            title.set_xalign(0.0);
            title.set_line_wrap(true);
            title.set_lines(2);
            title.set_max_width_chars(23);
            box_.pack_start(&count, false, false, 0);
            box_.pack_start(&title, false, false, 0);
            row.add(&box_);
            self.list.add(&row);
            if index == self.selected.get() {
                self.list.select_row(Some(&row));
            }
        }
        self.list.show_all();
        self.loading.set(false);
    }

    fn refresh_fields(&self) {
        self.loading.set(true);
        let deck = self.deck.borrow();
        let slide = &deck.slides[self.selected.get()];
        self.deck_title.set_text(&deck.title);
        self.theme.set_active_id(Some(&deck.theme));
        self.template_header.set_text(&deck.template.header);
        self.template_footer.set_text(&deck.template.footer);
        self.template_outline.set_active(deck.template.show_outline);
        self.font_status.set_text(&format!(
            "Fonts: heading {} · body {}",
            if deck.template.heading_font.is_some() {
                "embedded"
            } else {
                "default"
            },
            if deck.template.body_font.is_some() {
                "embedded"
            } else {
                "default"
            }
        ));
        self.layout.set_active_id(Some(&slide.layout));
        self.animation.set_active_id(Some(&slide.animation));
        self.eyebrow.set_text(&slide.eyebrow);
        set_text(&self.headline, &slide.title);
        set_text(&self.body, &slide.body);
        set_text(&self.notes, &slide.notes);
        self.picture_count
            .set_text(&format!("{} picture(s) on this slide", slide.images.len()));
        self.loading.set(false);
        drop(deck);
        self.refresh_preview();
    }

    fn refresh_preview(&self) {
        let deck = self.deck.borrow();
        if let Ok(html) = preview_html(&deck, self.selected.get()) {
            self.preview.load_html(&html, None);
        }
    }

    fn select(&self, index: usize) {
        if self.dirty.get() && self.persist().is_err() {
            self.refresh_list();
            return;
        }
        if index >= self.deck.borrow().slides.len() {
            return;
        }
        self.selected.set(index);
        self.refresh_fields();
    }

    fn replace_deck(&self, deck: Deck) {
        let snapshot = (|| {
            let _write_lock = lock_deck_writes(&self.state)?;
            let bytes = fs::read(deck_path(&self.state, &deck.id)?).map_err(internal)?;
            let current: Deck = serde_json::from_slice(&bytes).map_err(internal)?;
            validate_draft(&current)?;
            if current.id != deck.id {
                return Err("Deck ID does not match its file name".into());
            }
            Ok::<_, ApiError>((
                current,
                bytes,
                file_stamp(&deck_path(&self.state, &deck.id)?),
            ))
        })();
        let (deck, bytes, stamp) = match snapshot {
            Ok(snapshot) => snapshot,
            Err(error) => {
                self.status.set_text(&format!("Open failed: {error}"));
                return;
            }
        };
        if let Some(source) = self.save_source.borrow_mut().take() {
            source.remove();
        }
        *self.last_disk.borrow_mut() = bytes;
        self.last_disk_stamp.set(stamp);
        *self.deck.borrow_mut() = deck;
        self.selected.set(0);
        self.refresh_list();
        self.refresh_fields();
        self.dirty.set(false);
        match validate_deck(&self.deck.borrow()) {
            Ok(_) => self.status.set_text("Saved locally"),
            Err(error) => self
                .status
                .set_text(&format!("Saved draft · cannot present: {error}")),
        }
    }

    fn add_slide(&self, duplicate: bool) {
        if self.dirty.get() && self.persist().is_err() {
            return;
        }
        let index = self.selected.get() + 1;
        let mut deck = self.deck.borrow_mut();
        let slide = if duplicate {
            let mut slide = deck.slides[index - 1].clone();
            slide.id = format!("slide-{}", unique_id());
            slide
        } else {
            Slide {
                id: format!("slide-{}", unique_id()),
                layout: "statement".into(),
                eyebrow: String::new(),
                title: "A clear idea goes here.".into(),
                body: String::new(),
                notes: String::new(),
                animation: default_animation(),
                images: Vec::new(),
            }
        };
        deck.slides.insert(index, slide);
        drop(deck);
        self.selected.set(index);
        self.refresh_list();
        self.refresh_fields();
        let _ = self.persist();
    }

    fn move_slide(&self, delta: isize) {
        if self.dirty.get() && self.persist().is_err() {
            return;
        }
        let index = self.selected.get();
        let next = index as isize + delta;
        if next < 0 || next as usize >= self.deck.borrow().slides.len() {
            return;
        }
        self.deck.borrow_mut().slides.swap(index, next as usize);
        self.selected.set(next as usize);
        self.refresh_list();
        self.refresh_fields();
        let _ = self.persist();
    }

    fn delete_slide(&self) {
        if self.dirty.get() && self.persist().is_err() {
            return;
        }
        if self.deck.borrow().slides.len() <= 1 {
            self.status.set_text("Keep at least one slide");
            return;
        }
        self.deck.borrow_mut().slides.remove(self.selected.get());
        self.selected
            .set(self.selected.get().min(self.deck.borrow().slides.len() - 1));
        self.refresh_list();
        self.refresh_fields();
        let _ = self.persist();
    }

    fn insert_picture(&self, path: &std::path::Path) {
        if self.dirty.get() && self.persist().is_err() {
            return;
        }
        let mut deck = self.deck.borrow_mut();
        let index = self.selected.get();
        let alt = path
            .file_stem()
            .and_then(|name| name.to_str())
            .unwrap_or("Picture");
        let result = add_image_to_slide(&mut deck.slides[index], path, alt);
        drop(deck);
        match result {
            Ok(_) => {
                self.refresh_fields();
                let _ = self.persist();
            }
            Err(error) => self.status.set_text(&format!("Picture failed: {error}")),
        }
    }

    fn remove_picture(&self) {
        if self.dirty.get() && self.persist().is_err() {
            return;
        }
        let mut deck = self.deck.borrow_mut();
        if deck.slides[self.selected.get()].images.pop().is_none() {
            self.status.set_text("This slide has no pictures");
            return;
        }
        drop(deck);
        self.refresh_fields();
        let _ = self.persist();
    }

    fn set_logo(&self, path: &std::path::Path) {
        if self.dirty.get() && self.persist().is_err() {
            return;
        }
        match image_data_uri(path, 4_000_000) {
            Ok(uri) => {
                self.deck.borrow_mut().template.logo = Some(uri);
                self.refresh_preview();
                let _ = self.persist();
            }
            Err(error) => self.status.set_text(&format!("Logo failed: {error}")),
        }
    }

    fn clear_logo(&self) {
        if self.dirty.get() && self.persist().is_err() {
            return;
        }
        self.deck.borrow_mut().template.logo = None;
        self.refresh_preview();
        let _ = self.persist();
    }

    fn set_font(&self, role: &str, path: &std::path::Path) {
        if self.dirty.get() && self.persist().is_err() {
            return;
        }
        match font_data_uri(path) {
            Ok(uri) => {
                let mut deck = self.deck.borrow_mut();
                match role {
                    "heading" => deck.template.heading_font = Some(uri),
                    "body" => deck.template.body_font = Some(uri),
                    _ => return,
                }
                drop(deck);
                self.refresh_fields();
                let _ = self.persist();
            }
            Err(error) => self.status.set_text(&format!("Font failed: {error}")),
        }
    }

    fn clear_fonts(&self) {
        if self.dirty.get() && self.persist().is_err() {
            return;
        }
        let mut deck = self.deck.borrow_mut();
        deck.template.heading_font = None;
        deck.template.body_font = None;
        drop(deck);
        self.refresh_fields();
        let _ = self.persist();
    }

    fn move_image(&self, id: &str, x: f32, y: f32) {
        if self.dirty.get() && self.persist().is_err() {
            return;
        }
        let mut deck = self.deck.borrow_mut();
        let Some(image) = deck.slides[self.selected.get()]
            .images
            .iter_mut()
            .find(|image| image.id == id)
        else {
            return;
        };
        if !x.is_finite()
            || !y.is_finite()
            || x < 0.0
            || y < 0.0
            || x + image.width > 100.0
            || y + image.height > 100.0
        {
            return;
        }
        image.x = x;
        image.y = y;
        drop(deck);
        let _ = self.persist();
    }
}

fn button(label: &str) -> gtk::Button {
    gtk::Button::with_label(label)
}

fn choose_picture(parent: &gtk::ApplicationWindow, title: &str) -> Option<PathBuf> {
    let dialog = gtk::FileChooserDialog::with_buttons(
        Some(title),
        Some(parent),
        gtk::FileChooserAction::Open,
        &[
            ("Cancel", ResponseType::Cancel),
            ("Open", ResponseType::Accept),
        ],
    );
    let filter = gtk::FileFilter::new();
    filter.set_name(Some("PNG, JPEG, GIF, WebP"));
    for mime in ["image/png", "image/jpeg", "image/gif", "image/webp"] {
        filter.add_mime_type(mime);
    }
    dialog.add_filter(filter);
    let path = if dialog.run() == ResponseType::Accept {
        dialog.filename()
    } else {
        None
    };
    // GTK keeps a dialog alive after run(); nothing reads it after this point.
    unsafe { dialog.destroy() };
    path
}

fn choose_font(parent: &gtk::ApplicationWindow, title: &str) -> Option<PathBuf> {
    let dialog = gtk::FileChooserDialog::with_buttons(
        Some(title),
        Some(parent),
        gtk::FileChooserAction::Open,
        &[
            ("Cancel", ResponseType::Cancel),
            ("Open", ResponseType::Accept),
        ],
    );
    let filter = gtk::FileFilter::new();
    filter.set_name(Some("WOFF2 fonts"));
    filter.add_pattern("*.woff2");
    dialog.add_filter(filter);
    let path = if dialog.run() == ResponseType::Accept {
        dialog.filename()
    } else {
        None
    };
    unsafe { dialog.destroy() };
    path
}

fn release_session(state: &AppState, token: &str) {
    if let Ok(mut sessions) = state.presentations.lock() {
        sessions.remove(token);
    }
}

fn prepare_audience_on_hyprland() {
    if std::env::var_os("HYPRLAND_INSTANCE_SIGNATURE").is_none() {
        return;
    }
    let Ok(active) = std::process::Command::new("hyprctl")
        .args(["activewindow", "-j"])
        .output()
    else {
        return;
    };
    let Ok(active) = serde_json::from_slice::<serde_json::Value>(&active.stdout) else {
        return;
    };
    if active["pid"].as_u64() != Some(std::process::id() as u64)
        || !active["title"]
            .as_str()
            .is_some_and(|title| title.starts_with("HyperFrames Audience"))
    {
        return;
    }
    let Some(address) = active["address"].as_str().filter(|value| {
        value.starts_with("0x") && value[2..].chars().all(|c| c.is_ascii_hexdigit())
    }) else {
        return;
    };
    if active["floating"].as_bool() == Some(false) {
        let command = format!(
            "hl.dispatch(hl.dsp.window.float({{ window = 'address:{address}', action = 'toggle' }}))"
        );
        let _ = std::process::Command::new("hyprctl")
            .args(["eval", &command])
            .output();
        let command = format!(
            "hl.dispatch(hl.dsp.window.resize({{ window = 'address:{address}', x = 1280, y = 720 }}))"
        );
        let _ = std::process::Command::new("hyprctl")
            .args(["eval", &command])
            .output();
    }
    let center_command =
        format!("hl.dispatch(hl.dsp.window.center({{ window = 'address:{address}' }}))");
    let centered = std::process::Command::new("hyprctl")
        .args(["eval", &center_command])
        .output();
    if !centered.is_ok_and(|result| result.status.success()) {
        let _ = std::process::Command::new("hyprctl")
            .args(["dispatch", "centerwindow"])
            .output();
    }
    // Presentations should remain opaque even when the desktop uses window
    // transparency; otherwise the shared audience window leaks background UI.
    for property in ["opacity", "opacity_inactive"] {
        let command = format!(
            "hl.dispatch(hl.dsp.window.set_prop({{ prop = '{property}', value = '1', window = 'address:{address}' }}))"
        );
        let _ = std::process::Command::new("hyprctl")
            .args(["eval", &command])
            .output();
    }
}

fn control_script(command: &str) -> Result<String, ApiError> {
    if command == "gpu" {
        return Ok("(()=>{const canvas=document.createElement('canvas');const gl=canvas.getContext('webgl');if(!gl)return JSON.stringify({webgl:false,policy:'always'});const info=gl.getExtension('WEBGL_debug_renderer_info');return JSON.stringify({webgl:true,policy:'always',vendor:gl.getParameter(info?info.UNMASKED_VENDOR_WEBGL:gl.VENDOR),renderer:gl.getParameter(info?info.UNMASKED_RENDERER_WEBGL:gl.RENDERER)})})()".into());
    }
    if command == "inspect" || command == "inspect-audience" {
        return Ok(r#"(() => {
            const p = document.querySelector('hyperframes-player');
            const f = p?.querySelector('iframe') || p?.shadowRoot?.querySelector('iframe') || document.querySelector('iframe');
            const d = f?.contentDocument;
            if (!d) return JSON.stringify({frameReadable: false});
            const visible = el => {
                if (!el) return false;
                const rect = el.getBoundingClientRect();
                if (rect.width <= 0 || rect.height <= 0 || rect.right <= 0 || rect.bottom <= 0 || rect.left >= d.defaultView.innerWidth || rect.top >= d.defaultView.innerHeight) return false;
                for (let node = el; node && node.nodeType === 1; node = node.parentElement) {
                    const style = d.defaultView.getComputedStyle(node);
                    if (style.display === 'none' || style.visibility !== 'visible' || Number(style.opacity) < .05) return false;
                }
                return true;
            };
            const scenes = [...d.querySelectorAll('.slide')].map(s => {
                const style = d.defaultView.getComputedStyle(s);
                const rect = s.getBoundingClientRect();
                const regions = [...s.querySelectorAll('.template-header,.template-footer,.template-logo,.eyebrow,.heading,.body,.slide-image,.slide-number')];
                const overlaps = [];
                for (let i = 0; i < regions.length; i++) for (let j = i + 1; j < regions.length; j++) {
                    const a = regions[i].getBoundingClientRect(), b = regions[j].getBoundingClientRect();
                    if (Math.min(a.right,b.right)-Math.max(a.left,b.left)>8 && Math.min(a.bottom,b.bottom)-Math.max(a.top,b.top)>8) overlaps.push([regions[i].id,regions[j].id]);
                }
                return {
                    id: s.dataset.compositionId,
                    title: s.querySelector('.heading')?.textContent || '',
                    body: s.querySelector('.body')?.textContent || '',
                    headlineVisible: visible(s.querySelector('.heading .motion')),
                    bodyVisible: visible(s.querySelector('.body .motion')),
                    headlineOpacity: s.querySelector('.heading .motion') ? Number(d.defaultView.getComputedStyle(s.querySelector('.heading .motion')).opacity) : null,
                    bodyOpacity: s.querySelector('.body .motion') ? Number(d.defaultView.getComputedStyle(s.querySelector('.body .motion')).opacity) : null,
                    headingFont: s.querySelector('.heading') ? d.defaultView.getComputedStyle(s.querySelector('.heading')).fontFamily : '',
                    bodyFont: s.querySelector('.body') ? d.defaultView.getComputedStyle(s.querySelector('.body')).fontFamily : '',
                    overlaps,
                    outlineVisible: visible(s.querySelector('.deck-outline')),
                    outlineItems: s.querySelectorAll('.deck-outline li').length,
                    outlineActive: s.querySelector('.deck-outline .active')?.textContent?.trim() || '',
                    header: s.querySelector('.template-header')?.textContent || '',
                    footer: s.querySelector('.template-footer')?.textContent || '',
                    images: [...s.querySelectorAll('img')].map(i => ({id: i.id, loaded: i.complete && i.naturalWidth > 0, opacity: Number(d.defaultView.getComputedStyle(i).opacity)})),
                    clips: [...s.querySelectorAll('.clip')].map(i => {
                        const clipStyle = d.defaultView.getComputedStyle(i);
                        return {id: i.id, display: clipStyle.display, visibility: clipStyle.visibility, opacity: clipStyle.opacity};
                    }),
                    bounds: {x: rect.x, y: rect.y, width: rect.width, height: rect.height},
                    inViewport: rect.right > 0 && rect.bottom > 0 && rect.left < d.defaultView.innerWidth && rect.top < d.defaultView.innerHeight,
                    display: style.display,
                    visibility: style.visibility,
                    opacity: style.opacity
                };
            });
            const s = document.querySelector('hyperframes-slideshow');
            const notes = s?.querySelector('[data-hf-presenter]');
            return JSON.stringify({frameReadable: true, notesEnabled: s?.getAttribute('data-hf-show-notes') === 'true', notesPaneVisible: !!notes && getComputedStyle(notes).display !== 'none', textEntranceActive: s?.dataset.hfTextEntranceActive === 'true', fonts: [...d.fonts].map(f => ({family: f.family, status: f.status})), scenes});
        })()"#.into());
    }
    let action = match command {
        "status" => String::new(),
        "next" => "c.next();".into(),
        "prev" => "c.prev();".into(),
        "notes on" | "notes off" => {
            return Err("Speaker notes cannot be shown in the audience window".into())
        }
        _ if command.starts_with("goto ") => {
            let position: usize = command[5..].parse().map_err(|_| "Invalid slide position")?;
            format!("if({position}<1||{position}>c.counter.total)return JSON.stringify({{error:'Position is out of range'}});c.goToSlide({});", position - 1)
        }
        _ => return Err("Unknown presentation command".into()),
    };
    Ok(format!("(()=>{{const s=document.querySelector('hyperframes-slideshow');const c=s?.controller;if(!c)return JSON.stringify({{error:'Audience is loading'}});{action}return JSON.stringify({{ready:true,slideIndex:c.position.slideIndex,slideNumber:c.counter.index,slideCount:c.counter.total,presenting:true,notesEnabled:false}})}})()"))
}

fn write_control_reply(stream: std::os::unix::net::UnixStream, value: serde_json::Value) {
    std::thread::spawn(move || {
        let mut stream = stream;
        let _ = stream.set_write_timeout(Some(std::time::Duration::from_millis(300)));
        let _ = stream.write_all(value.to_string().as_bytes());
    });
}

fn register_control(
    state: &AppState,
    token: &str,
    view: &webkit2gtk::WebView,
    window: &gtk::ApplicationWindow,
) -> Result<(), ApiError> {
    let path = control_path(state, token)?;
    let directory = path.parent().ok_or("Invalid control path")?;
    fs::create_dir_all(directory).map_err(internal)?;
    fs::set_permissions(directory, fs::Permissions::from_mode(0o700)).map_err(internal)?;
    let listener = UnixListener::bind(&path).map_err(internal)?;
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).map_err(internal)?;
    listener.set_nonblocking(true).map_err(internal)?;
    let (sender, receiver) = mpsc::sync_channel::<(String, std::os::unix::net::UnixStream)>(32);
    let running = Arc::new(AtomicBool::new(true));
    let active = Arc::new(AtomicUsize::new(0));
    let running_for_listener = running.clone();
    std::thread::spawn(move || {
        while running_for_listener.load(Ordering::Relaxed) {
            match listener.accept() {
                Ok((stream, _)) => {
                    if active.fetch_add(1, Ordering::Relaxed) >= 32 {
                        active.fetch_sub(1, Ordering::Relaxed);
                        write_control_reply(stream, serde_json::json!({"error":"Control busy"}));
                        continue;
                    }
                    let sender = sender.clone();
                    let active = active.clone();
                    std::thread::spawn(move || {
                        let mut stream = stream;
                        let _ =
                            stream.set_read_timeout(Some(std::time::Duration::from_millis(300)));
                        // The client closes its write half to mark the end of one command.
                        let mut bytes = Vec::new();
                        let result = (&mut stream).take(129).read_to_end(&mut bytes);
                        match result {
                            Ok(_) if bytes.len() <= 128 => match String::from_utf8(bytes) {
                                Ok(command) if !command.trim().is_empty() => {
                                    if let Err(error) =
                                        sender.try_send((command.trim().into(), stream))
                                    {
                                        let (_, stream) = match error {
                                            mpsc::TrySendError::Full(value)
                                            | mpsc::TrySendError::Disconnected(value) => value,
                                        };
                                        write_control_reply(
                                            stream,
                                            serde_json::json!({"error":"Control busy"}),
                                        );
                                    }
                                }
                                _ => write_control_reply(
                                    stream,
                                    serde_json::json!({"error":"Invalid command"}),
                                ),
                            },
                            Ok(_) => write_control_reply(
                                stream,
                                serde_json::json!({"error":"Command is too long"}),
                            ),
                            Err(error) => write_control_reply(
                                stream,
                                serde_json::json!({"error":error.to_string()}),
                            ),
                        }
                        active.fetch_sub(1, Ordering::Relaxed);
                    });
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(std::time::Duration::from_millis(20));
                }
                Err(error) => {
                    eprintln!("Presentation control failed: {error}");
                    break;
                }
            }
        }
    });
    let view = view.clone();
    let window_for_commands = window.clone();
    let token_for_reply = token.to_string();
    let source = gtk::glib::timeout_add_local(std::time::Duration::from_millis(40), move || {
        for _ in 0..16 {
            let (command, stream) = match receiver.try_recv() {
                Ok(value) => value,
                Err(_) => break,
            };
            let command = command.as_str();
            if command == "close" {
                write_control_reply(
                    stream,
                    serde_json::json!({"session":token_for_reply,"closed":true}),
                );
                let window = window_for_commands.clone();
                gtk::glib::idle_add_local_once(move || window.close());
                continue;
            }
            if command == "audience" {
                window_for_commands.present();
                write_control_reply(
                    stream,
                    serde_json::json!({"session":token_for_reply,"audienceOpen":true}),
                );
                continue;
            }
            if command == "audience-close" {
                write_control_reply(
                    stream,
                    serde_json::json!({"session":token_for_reply,"audienceOpen":false,"wasOpen":true}),
                );
                let window = window_for_commands.clone();
                gtk::glib::idle_add_local_once(move || window.close());
                continue;
            }
            match control_script(command) {
                Ok(script) => {
                    let token = token_for_reply.clone();
                    let include_audience_position = command == "status";
                    let include_gpu_diagnostics = command == "gpu";
                    view.run_javascript(&script, None::<&gtk::gio::Cancellable>, move |result| {
                        let mut value = match result {
                            Ok(result) => result.js_value()
                                .and_then(|value| serde_json::from_str::<serde_json::Value>(&value.to_str()).ok())
                                .unwrap_or_else(|| serde_json::json!({"error":"Could not read presenter state"})),
                            Err(error) => serde_json::json!({"error":error.to_string()}),
                        };
                        if let Some(object) = value.as_object_mut() {
                            object.insert("session".into(), serde_json::Value::String(token));
                            object.insert("audienceOpen".into(), serde_json::Value::Bool(true));
                            if include_audience_position {
                                let number = object.get("slideNumber").cloned().unwrap_or(serde_json::Value::Null);
                                object.insert("audienceSlideNumber".into(), number);
                            }
                        }
                        if include_gpu_diagnostics { gpu_diagnostics(&mut value); }
                        write_control_reply(stream, value);
                    });
                }
                Err(error) => write_control_reply(stream, serde_json::json!({"error":error})),
            }
        }
        gtk::glib::ControlFlow::Continue
    });
    let path_for_close = path.clone();
    let source = RefCell::new(Some(source));
    window.connect_destroy(move |_| {
        running.store(false, Ordering::Relaxed);
        if let Some(source) = source.borrow_mut().take() {
            source.remove();
        }
        let _ = fs::remove_file(&path_for_close);
    });
    Ok(())
}

fn open_audience(app: &gtk::Application, url: &str, state: &AppState) -> Result<(), ApiError> {
    let window = gtk::ApplicationWindow::new(app);
    window.set_title("HyperFrames Audience · share this window in Zoom");
    window.set_default_size(1280, 720);
    let view = webkit2gtk::WebView::new();
    configure_acceleration(&view);
    view.connect_load_failed(|_, _, uri, error| {
        eprintln!("Audience failed to load {uri}: {error}");
        false
    });
    window.add(&view);
    let token = url
        .strip_prefix("hyperframe://app/")
        .and_then(|path| path.split('/').next())
        .unwrap_or_default()
        .to_string();
    {
        let state = state.clone();
        let token = token.clone();
        window.connect_destroy(move |_| release_session(&state, &token));
    }
    for widget in [
        window.clone().upcast::<gtk::Widget>(),
        view.clone().upcast::<gtk::Widget>(),
    ] {
        let window = window.clone();
        widget.connect_key_press_event(move |_, event| {
            if event.keyval() == gtk::gdk::keys::constants::Escape {
                let window = window.clone();
                gtk::glib::idle_add_local_once(move || window.close());
                gtk::glib::Propagation::Stop
            } else {
                gtk::glib::Propagation::Proceed
            }
        });
    }
    register_control(state, &token, &view, &window)?;
    view.load_uri(url);
    window.show_all();
    gtk::glib::timeout_add_local_once(std::time::Duration::from_millis(150), || {
        prepare_audience_on_hyprland();
    });
    Ok(())
}

fn show_presentation(editor: &Editor, app: &gtk::Application) {
    if editor.persist().is_err() {
        return;
    }
    let id = editor.deck.borrow().id.clone();
    match present(&editor.state, &id) {
        Ok(url) => {
            editor
                .status
                .set_text("Audience window opened · share it in Zoom");
            if let Err(error) = open_audience(app, &url, &editor.state) {
                editor
                    .status
                    .set_text(&format!("Presentation failed: {error}"));
            }
        }
        Err(msg) => editor
            .status
            .set_text(&format!("Presentation failed: {msg}")),
    }
}

fn resolve_unsaved(editor: &Editor, parent: &gtk::ApplicationWindow, action: &str) -> bool {
    if !editor.dirty.get() || editor.persist().is_ok() {
        return true;
    }
    let dialog = gtk::Dialog::with_buttons(
        Some("Unsaved presentation changes"),
        Some(parent),
        gtk::DialogFlags::MODAL,
        &[
            ("Cancel", ResponseType::Cancel),
            ("Save a copy", ResponseType::Other(1)),
            ("Discard my edits", ResponseType::Reject),
        ],
    );
    let message = gtk::Label::new(Some(&format!(
        "The latest edits could not be saved. To {action}, save a recovery copy or explicitly discard those edits."
    )));
    message.set_line_wrap(true);
    message.set_margin_top(18);
    message.set_margin_bottom(18);
    message.set_margin_start(18);
    message.set_margin_end(18);
    dialog.content_area().add(&message);
    dialog.show_all();
    let choice = dialog.run();
    // GTK keeps a dialog alive after run(); nothing reads it after this point.
    unsafe { dialog.destroy() };
    match choice {
        ResponseType::Reject => true,
        ResponseType::Other(1) => {
            let chooser = gtk::FileChooserDialog::with_buttons(
                Some("Save a recovery copy"),
                Some(parent),
                gtk::FileChooserAction::Save,
                &[
                    ("Cancel", ResponseType::Cancel),
                    ("Save copy", ResponseType::Accept),
                ],
            );
            chooser.set_do_overwrite_confirmation(true);
            chooser.set_current_name(&format!("{}-recovery.json", editor.deck.borrow().id));
            let result = if chooser.run() == ResponseType::Accept {
                chooser.filename().map(|path| {
                    let original = deck_path(&editor.state, &editor.deck.borrow().id)?;
                    let same_existing_file = fs::canonicalize(&path)
                        .ok()
                        .zip(fs::canonicalize(&original).ok())
                        .is_some_and(|(a, b)| a == b);
                    if path == original || same_existing_file {
                        return Err("Choose a different path for the recovery copy".into());
                    }
                    let bytes =
                        serde_json::to_vec_pretty(&*editor.deck.borrow()).map_err(internal)?;
                    write_private(&path, &bytes)
                })
            } else {
                None
            };
            unsafe { chooser.destroy() };
            match result {
                Some(Ok(())) => true,
                Some(Err(error)) => {
                    editor
                        .status
                        .set_text(&format!("Recovery copy failed: {error}"));
                    false
                }
                None => false,
            }
        }
        _ => false,
    }
}

fn build(app: &gtk::Application, state: AppState) {
    let initial = recent_decks(&state).into_iter().next().unwrap_or_else(|| {
        let deck = sample_deck();
        let _ = write_deck(&state, &deck);
        deck
    });
    let window = gtk::ApplicationWindow::new(app);
    window.set_title("HyperFrames Slides");
    window.set_default_size(1440, 880);
    let root = gtk::Box::new(Orientation::Vertical, 0);
    let header = gtk::Box::new(Orientation::Horizontal, 8);
    header.style_context().add_class("toolbar");
    let logo = gtk::Label::new(Some("◈  HyperFrames Slides"));
    logo.style_context().add_class("logo");
    header.pack_start(&logo, false, false, 10);
    let deck_title = gtk::Entry::new();
    deck_title.set_width_chars(30);
    deck_title.set_max_length(160);
    header.pack_start(&deck_title, true, true, 0);
    let new_btn = button("New");
    let duplicate_deck_btn = button("Duplicate deck");
    duplicate_deck_btn.set_tooltip_text(Some("Create a new presentation from this deck"));
    let open_btn = button("Open");
    let recent_btn = button("Recent");
    let reload_btn = button("Reload");
    let save_btn = button("Save");
    let export_btn = button("Export HTML");
    let export_audience_btn = button("Export for audience");
    let present_btn = button("Present");
    let menu_button = gtk::MenuButton::new();
    menu_button.set_label("Menu");
    menu_button.set_tooltip_text(Some("Presentation and file actions"));
    let menu = gtk::Menu::new();
    for (label, action) in [
        ("New", &new_btn),
        ("Duplicate deck", &duplicate_deck_btn),
        ("Open", &open_btn),
        ("Recent", &recent_btn),
        ("Reload", &reload_btn),
        ("Save", &save_btn),
        ("Export HTML", &export_btn),
        ("Export for audience", &export_audience_btn),
    ] {
        let item = gtk::MenuItem::with_label(label);
        let action = action.clone();
        item.connect_activate(move |_| action.emit_clicked());
        menu.append(&item);
    }
    menu.show_all();
    menu_button.set_popup(Some(&menu));
    header.pack_start(&menu_button, false, false, 0);
    header.pack_start(&present_btn, false, false, 0);
    present_btn.style_context().add_class("suggested-action");
    root.pack_start(&header, false, false, 0);
    let main = gtk::Paned::new(Orientation::Horizontal);
    let left = gtk::Box::new(Orientation::Vertical, 8);
    left.set_size_request(225, -1);
    left.style_context().add_class("sidebar");
    let slides_heading = editor_label("SLIDES");
    left.pack_start(&slides_heading, false, false, 8);
    let list = gtk::ListBox::new();
    list.set_selection_mode(gtk::SelectionMode::Single);
    let scroller = gtk::ScrolledWindow::new(None::<&gtk::Adjustment>, None::<&gtk::Adjustment>);
    scroller.add(&list);
    left.pack_start(&scroller, true, true, 0);
    let add_btn = button("＋ Add slide");
    let duplicate_btn = button("Duplicate");
    let remove_btn = button("Delete");
    let slide_buttons = gtk::Box::new(Orientation::Vertical, 6);
    for b in [&add_btn, &duplicate_btn, &remove_btn] {
        slide_buttons.pack_start(b, false, false, 0);
    }
    let movement = gtk::Box::new(Orientation::Horizontal, 6);
    let up_btn = button("↑ Move");
    let down_btn = button("↓ Move");
    movement.pack_start(&up_btn, true, true, 0);
    movement.pack_start(&down_btn, true, true, 0);
    slide_buttons.pack_start(&movement, false, false, 0);
    left.pack_start(&slide_buttons, false, false, 0);
    main.pack1(&left, false, false);

    let content = gtk::Paned::new(Orientation::Horizontal);
    let center = gtk::Box::new(Orientation::Vertical, 8);
    center.set_margin_top(20);
    center.set_margin_bottom(20);
    center.set_margin_start(20);
    center.set_margin_end(20);
    let preview_label = editor_label("AUDIENCE PREVIEW  ·  16:9");
    center.pack_start(&preview_label, false, false, 0);
    let preview_manager = webkit2gtk::UserContentManager::new();
    preview_manager.register_script_message_handler("imagePosition");
    let preview = webkit2gtk::WebView::with_user_content_manager(&preview_manager);
    configure_acceleration(&preview);
    preview.set_size_request(480, 270);
    let preview_frame = gtk::Frame::new(None);
    preview_frame.add(&preview);
    let aspect = gtk::AspectFrame::new(None, 0.5, 0.5, 16.0 / 9.0, false);
    aspect.add(&preview_frame);
    center.pack_start(&aspect, true, true, 0);
    let fit_label = gtk::Label::new(Some("Checking slide fit…"));
    fit_label.set_xalign(0.0);
    fit_label.style_context().add_class("help");
    center.pack_start(&fit_label, false, false, 0);
    preview.connect_notify_local(Some("title"), move |view, _| {
        if let Some(title) = view.title() {
            if title.starts_with("Check slide:") || title.starts_with("No clipping") {
                fit_label.set_text(&title);
            }
        }
    });
    let help = gtk::Label::new(Some("Present opens the Audience window to share in Zoom."));
    help.set_line_wrap(true);
    help.set_xalign(0.0);
    help.style_context().add_class("help");
    center.pack_start(&help, false, false, 0);
    content.pack1(&center, true, false);
    let fields_scroll =
        gtk::ScrolledWindow::new(None::<&gtk::Adjustment>, None::<&gtk::Adjustment>);
    fields_scroll.set_size_request(335, -1);
    let fields = gtk::Box::new(Orientation::Vertical, 8);
    fields.set_margin_top(20);
    fields.set_margin_bottom(20);
    fields.set_margin_start(18);
    fields.set_margin_end(18);
    fields.pack_start(&editor_label("EDIT SLIDE"), false, false, 5);
    fields.pack_start(&editor_label("Layout"), false, false, 0);
    let layout = gtk::ComboBoxText::new();
    for (id, label) in [
        ("title", "Title"),
        ("statement", "Statement"),
        ("split", "Split"),
        ("quote", "Quote"),
    ] {
        layout.append(Some(id), label);
    }
    fields.pack_start(&layout, false, false, 0);
    fields.pack_start(&editor_label("Eyebrow"), false, false, 0);
    let eyebrow = gtk::Entry::new();
    eyebrow.set_max_length(100);
    fields.pack_start(&eyebrow, false, false, 0);
    let headline = text_field(&fields, "Headline", 4);
    let body = text_field(&fields, "Supporting text", 5);
    let formatting = gtk::Box::new(Orientation::Horizontal, 6);
    let bold_btn = button("Bold");
    let italic_btn = button("Italic");
    let bullets_btn = button("Bullets");
    for widget in [&bold_btn, &italic_btn, &bullets_btn] {
        formatting.pack_start(widget, false, false, 0);
    }
    fields.pack_start(&formatting, false, false, 0);
    let notes_toggle = gtk::CheckButton::with_label("Show speaker notes");
    fields.pack_start(&notes_toggle, false, false, 0);
    let notes_revealer = gtk::Revealer::new();
    notes_revealer.set_reveal_child(false);
    let notes_panel = gtk::Box::new(Orientation::Vertical, 8);
    let notes = text_field(&notes_panel, "Speaker notes", 8);
    notes_revealer.add(&notes_panel);
    fields.pack_start(&notes_revealer, false, false, 0);
    notes_toggle.connect_toggled(move |toggle| {
        notes_revealer.set_reveal_child(toggle.is_active());
    });
    fields.pack_start(&editor_label("Slide animation"), false, false, 0);
    let animation = gtk::ComboBoxText::new();
    for (id, label) in [
        ("none", "None"),
        ("fade", "Fade"),
        ("rise", "Rise"),
        ("zoom", "Zoom"),
    ] {
        animation.append(Some(id), label);
    }
    fields.pack_start(&animation, false, false, 0);
    fields.pack_start(&editor_label("PICTURES"), false, false, 5);
    let picture_count = editor_label("0 picture(s) on this slide");
    fields.pack_start(&picture_count, false, false, 0);
    let picture_buttons = gtk::Box::new(Orientation::Horizontal, 6);
    let insert_picture_btn = button("Insert picture");
    let remove_picture_btn = button("Remove last");
    picture_buttons.pack_start(&insert_picture_btn, false, false, 0);
    picture_buttons.pack_start(&remove_picture_btn, false, false, 0);
    fields.pack_start(&picture_buttons, false, false, 0);
    let picture_drop = gtk::EventBox::new();
    let picture_drop_label = gtk::Label::new(Some("Drop PNG, JPEG, GIF, or WebP here"));
    picture_drop_label.set_margin_top(16);
    picture_drop_label.set_margin_bottom(16);
    picture_drop.add(&picture_drop_label);
    picture_drop.drag_dest_set(
        gtk::DestDefaults::ALL,
        &[gtk::TargetEntry::new(
            "text/uri-list",
            gtk::TargetFlags::OTHER_APP,
            0,
        )],
        gtk::gdk::DragAction::COPY,
    );
    fields.pack_start(&picture_drop, false, false, 0);
    fields.pack_start(&editor_label("Theme"), false, false, 0);
    let theme = gtk::ComboBoxText::new();
    for (id, label) in [
        ("midnight", "Midnight"),
        ("paper", "Paper"),
        ("cobalt", "Cobalt"),
        ("sunset", "Sunset"),
        ("regent", "Regent College"),
    ] {
        theme.append(Some(id), label);
    }
    fields.pack_start(&theme, false, false, 0);
    fields.pack_start(&editor_label("TEMPLATE · EVERY SLIDE"), false, false, 5);
    fields.pack_start(&editor_label("Header"), false, false, 0);
    let template_header = gtk::Entry::new();
    template_header.set_max_length(500);
    fields.pack_start(&template_header, false, false, 0);
    fields.pack_start(&editor_label("Footer"), false, false, 0);
    let template_footer = gtk::Entry::new();
    template_footer.set_max_length(500);
    fields.pack_start(&template_footer, false, false, 0);
    let template_outline = gtk::CheckButton::with_label("Show outline to audience");
    fields.pack_start(&template_outline, false, false, 0);
    let logo_buttons = gtk::Box::new(Orientation::Horizontal, 6);
    let set_logo_btn = button("Set logo");
    let clear_logo_btn = button("Clear logo");
    logo_buttons.pack_start(&set_logo_btn, false, false, 0);
    logo_buttons.pack_start(&clear_logo_btn, false, false, 0);
    fields.pack_start(&logo_buttons, false, false, 0);
    let font_status = editor_label("Fonts: heading default · body default");
    fields.pack_start(&font_status, false, false, 0);
    let font_buttons = gtk::Box::new(Orientation::Horizontal, 6);
    let set_heading_font_btn = button("Heading font");
    let set_body_font_btn = button("Body font");
    let clear_fonts_btn = button("Clear fonts");
    font_buttons.pack_start(&set_heading_font_btn, false, false, 0);
    font_buttons.pack_start(&set_body_font_btn, false, false, 0);
    font_buttons.pack_start(&clear_fonts_btn, false, false, 0);
    fields.pack_start(&font_buttons, false, false, 0);
    fields_scroll.add(&fields);
    content.pack2(&fields_scroll, false, false);
    main.pack2(&content, true, false);
    root.pack_start(&main, true, true, 0);
    let status = gtk::Label::new(Some("Saved locally"));
    status.set_xalign(0.0);
    status.style_context().add_class("status");
    root.pack_start(&status, false, false, 0);
    window.add(&root);
    let initial_disk = deck_path(&state, &initial.id)
        .and_then(|path| fs::read(path).map_err(internal))
        .unwrap_or_default();
    let initial_stamp = deck_path(&state, &initial.id)
        .ok()
        .and_then(|path| file_stamp(&path));
    let editor = Rc::new(Editor {
        state,
        deck: RefCell::new(initial),
        last_disk: RefCell::new(initial_disk),
        last_disk_stamp: Cell::new(initial_stamp),
        selected: Cell::new(0),
        loading: Cell::new(false),
        dirty: Cell::new(false),
        save_source: RefCell::new(None),
        list,
        deck_title,
        theme,
        template_header,
        template_footer,
        template_outline,
        font_status,
        layout,
        animation,
        eyebrow,
        headline,
        body,
        notes,
        picture_count,
        preview,
        status,
    });
    let initial = editor.deck.borrow().clone();
    editor.replace_deck(initial);
    {
        let e = editor.clone();
        preview_manager.connect_script_message_received(Some("imagePosition"), move |_, result| {
            if let Some(value) = result.js_value() {
                if let Ok(position) = serde_json::from_str::<serde_json::Value>(&value.to_str()) {
                    if let (Some(id), Some(x), Some(y)) = (
                        position.get("id").and_then(|v| v.as_str()),
                        position.get("x").and_then(|v| v.as_f64()),
                        position.get("y").and_then(|v| v.as_f64()),
                    ) {
                        e.move_image(id, x as f32, y as f32);
                    }
                }
            }
        });
    }
    {
        let e = editor.clone();
        gtk::glib::timeout_add_seconds_local(1, move || {
            if !e.dirty.get() {
                let id = e.deck.borrow().id.clone();
                if let Ok(path) = deck_path(&e.state, &id) {
                    let stamp = file_stamp(&path);
                    if stamp != e.last_disk_stamp.get() {
                        if let Ok(bytes) = fs::read(path) {
                            if bytes != *e.last_disk.borrow() {
                                if let Ok(deck) = read_deck(&e.state, &id) {
                                    e.replace_deck(deck);
                                }
                            } else {
                                e.last_disk_stamp.set(stamp);
                            }
                        }
                    }
                }
            }
            gtk::glib::ControlFlow::Continue
        });
    }

    {
        let e = editor.clone();
        let widget = e.list.clone();
        widget.connect_row_selected(move |_, row| {
            if e.loading.get() {
                return;
            }
            if let Some(row) = row {
                e.select(row.index() as usize);
            }
        });
    }
    {
        let e = editor.clone();
        let widget = e.deck_title.clone();
        widget.connect_changed(move |_| {
            e.schedule_persist();
        });
    }
    {
        let e = editor.clone();
        let widget = e.theme.clone();
        widget.connect_changed(move |_| {
            e.schedule_persist();
        });
    }
    for widget in [&editor.template_header, &editor.template_footer] {
        let e = editor.clone();
        widget.connect_changed(move |_| e.schedule_persist());
    }
    {
        let e = editor.clone();
        let widget = e.template_outline.clone();
        widget.connect_toggled(move |_| e.schedule_persist());
    }
    {
        let e = editor.clone();
        let widget = e.layout.clone();
        widget.connect_changed(move |_| {
            e.schedule_persist();
        });
    }
    {
        let e = editor.clone();
        let widget = e.animation.clone();
        widget.connect_changed(move |_| e.schedule_persist());
    }
    {
        let e = editor.clone();
        let widget = e.eyebrow.clone();
        widget.connect_changed(move |_| {
            e.schedule_persist();
        });
    }
    for view in [&editor.headline, &editor.body, &editor.notes] {
        let e = editor.clone();
        view.buffer().expect("buffer").connect_changed(move |_| {
            e.schedule_persist();
        });
    }
    {
        let view = editor.body.clone();
        bold_btn.connect_clicked(move |_| format_selection(&view, "**"));
    }
    {
        let view = editor.body.clone();
        italic_btn.connect_clicked(move |_| format_selection(&view, "*"));
    }
    {
        let view = editor.body.clone();
        bullets_btn.connect_clicked(move |_| format_selection(&view, "bullet"));
    }
    {
        let e = editor.clone();
        let parent = window.clone();
        insert_picture_btn.connect_clicked(move |_| {
            if let Some(path) = choose_picture(&parent, "Insert picture on slide") {
                e.insert_picture(&path);
            }
        });
    }
    {
        let e = editor.clone();
        remove_picture_btn.connect_clicked(move |_| e.remove_picture());
    }
    {
        let e = editor.clone();
        picture_drop.connect_drag_data_received(move |_, context, _, _, data, _, time| {
            let path = data
                .uris()
                .first()
                .and_then(|uri| gtk::gio::File::for_uri(uri.as_str()).path());
            let success = if let Some(path) = path {
                e.insert_picture(&path);
                true
            } else {
                false
            };
            context.drag_finish(success, false, time);
        });
    }
    {
        let e = editor.clone();
        let parent = window.clone();
        set_logo_btn.connect_clicked(move |_| {
            if let Some(path) = choose_picture(&parent, "Set logo on every slide") {
                e.set_logo(&path);
            }
        });
    }
    {
        let e = editor.clone();
        clear_logo_btn.connect_clicked(move |_| e.clear_logo());
    }
    {
        let e = editor.clone();
        let parent = window.clone();
        set_heading_font_btn.connect_clicked(move |_| {
            if let Some(path) = choose_font(&parent, "Set heading WOFF2 on every slide") {
                e.set_font("heading", &path);
            }
        });
    }
    {
        let e = editor.clone();
        let parent = window.clone();
        set_body_font_btn.connect_clicked(move |_| {
            if let Some(path) = choose_font(&parent, "Set body WOFF2 on every slide") {
                e.set_font("body", &path);
            }
        });
    }
    {
        let e = editor.clone();
        clear_fonts_btn.connect_clicked(move |_| e.clear_fonts());
    }
    {
        let e = editor.clone();
        add_btn.connect_clicked(move |_| e.add_slide(false));
    }
    {
        let e = editor.clone();
        duplicate_btn.connect_clicked(move |_| e.add_slide(true));
    }
    {
        let e = editor.clone();
        remove_btn.connect_clicked(move |_| e.delete_slide());
    }
    {
        let e = editor.clone();
        up_btn.connect_clicked(move |_| e.move_slide(-1));
    }
    {
        let e = editor.clone();
        down_btn.connect_clicked(move |_| e.move_slide(1));
    }
    {
        let e = editor.clone();
        let parent = window.clone();
        new_btn.connect_clicked(move |_| {
            if !resolve_unsaved(&e, &parent, "create a new deck") {
                return;
            }
            let deck = sample_deck();
            match write_deck(&e.state, &deck) {
                Ok(_) => e.replace_deck(deck),
                Err(msg) => e.status.set_text(&msg),
            }
        });
    }
    {
        let e = editor.clone();
        let parent = window.clone();
        duplicate_deck_btn.connect_clicked(move |_| {
            if !resolve_unsaved(&e, &parent, "duplicate this deck") {
                return;
            }
            let source = e.deck.borrow().clone();
            let title = format!("{} (copy)", source.title);
            let deck = duplicate_deck(&source, &title);
            match write_deck(&e.state, &deck) {
                Ok(_) => e.replace_deck(deck),
                Err(error) => e.status.set_text(&error),
            }
        });
    }
    {
        let e = editor.clone();
        let parent = window.clone();
        open_btn.connect_clicked(move |_| {
            if !resolve_unsaved(&e, &parent, "open another deck") {
                return;
            }
            let dialog = gtk::FileChooserDialog::with_buttons(
                Some("Open presentation JSON"),
                Some(&parent),
                gtk::FileChooserAction::Open,
                &[
                    ("Cancel", ResponseType::Cancel),
                    ("Open", ResponseType::Accept),
                ],
            );
            let filter = gtk::FileFilter::new();
            filter.set_name(Some("HyperFrames decks (*.json)"));
            filter.add_pattern("*.json");
            dialog.add_filter(filter);
            let _ = dialog.set_current_folder(e.state.data_dir.join("decks"));
            let path = if dialog.run() == ResponseType::Accept {
                dialog.filename()
            } else {
                None
            };
            unsafe { dialog.destroy() };
            if let Some(path) = path {
                match import_deck_file(&e.state, &path) {
                    Ok(deck) => e.replace_deck(deck),
                    Err(error) => e.status.set_text(&format!("Open failed: {error}")),
                }
            }
        });
    }
    {
        let e = editor.clone();
        let parent = window.clone();
        recent_btn.connect_clicked(move |_| {
            if !resolve_unsaved(&e, &parent, "open another deck") {
                return;
            }
            let decks = recent_decks(&e.state);
            let dialog = gtk::Dialog::with_buttons(
                Some("Open presentation"),
                Some(&parent),
                gtk::DialogFlags::MODAL,
                &[
                    ("Cancel", ResponseType::Cancel),
                    ("Open", ResponseType::Accept),
                ],
            );
            let chooser = gtk::ComboBoxText::new();
            for deck in &decks {
                chooser.append(Some(&deck.id), &deck.title);
            }
            if !decks.is_empty() {
                chooser.set_active(Some(0));
            }
            dialog.content_area().add(&chooser);
            dialog.show_all();
            if dialog.run() == ResponseType::Accept {
                if let Some(id) = chooser.active_id() {
                    if let Ok(deck) = read_deck(&e.state, &id) {
                        e.replace_deck(deck);
                    }
                }
            }
            unsafe { dialog.destroy() };
        });
    }
    {
        let e = editor.clone();
        let parent = window.clone();
        reload_btn.connect_clicked(move |_| {
            if !resolve_unsaved(&e, &parent, "reload from disk") {
                return;
            }
            let id = e.deck.borrow().id.clone();
            match read_deck(&e.state, &id) {
                Ok(deck) => e.replace_deck(deck),
                Err(error) => e.status.set_text(&format!("Reload failed: {error}")),
            }
        });
    }
    {
        let e = editor.clone();
        save_btn.connect_clicked(move |_| {
            let _ = e.persist();
        });
    }
    {
        let e = editor.clone();
        let parent = window.clone();
        export_btn.connect_clicked(move |_| {
            let save_failed = e.persist().is_err();
            let dialog = gtk::FileChooserDialog::with_buttons(
                Some("Export HyperFrames deck"),
                Some(&parent),
                gtk::FileChooserAction::Save,
                &[
                    ("Cancel", ResponseType::Cancel),
                    ("Export", ResponseType::Accept),
                ],
            );
            dialog.set_do_overwrite_confirmation(true);
            dialog.set_current_name("presentation.html");
            if dialog.run() == ResponseType::Accept {
                if let Some(path) = dialog.filename() {
                    match export_html(&e.deck.borrow())
                        .and_then(|html| write_private(&path, html.as_bytes()))
                    {
                        Ok(_) if save_failed => e.status.set_text(&format!(
                            "Exported {} · local deck has unsaved changes",
                            path.display()
                        )),
                        Ok(_) => e.status.set_text(&format!("Exported {}", path.display())),
                        Err(msg) => e.status.set_text(&msg),
                    }
                }
            }
            unsafe { dialog.destroy() };
        });
    }
    {
        let e = editor.clone();
        let parent = window.clone();
        export_audience_btn.connect_clicked(move |_| {
            let save_failed = e.persist().is_err();
            let dialog = gtk::FileChooserDialog::with_buttons(
                Some("Export audience deck without speaker notes"),
                Some(&parent),
                gtk::FileChooserAction::Save,
                &[
                    ("Cancel", ResponseType::Cancel),
                    ("Export", ResponseType::Accept),
                ],
            );
            dialog.set_do_overwrite_confirmation(true);
            dialog.set_current_name("audience.html");
            if dialog.run() == ResponseType::Accept {
                if let Some(path) = dialog.filename() {
                    match export_html_with_notes(&e.deck.borrow(), false)
                        .and_then(|html| write_private(&path, html.as_bytes()))
                    {
                        Ok(_) if save_failed => e.status.set_text(&format!(
                            "Exported {} without notes · local deck has unsaved changes",
                            path.display()
                        )),
                        Ok(_) => e
                            .status
                            .set_text(&format!("Exported {} without notes", path.display())),
                        Err(msg) => e.status.set_text(&msg),
                    }
                }
            }
            unsafe { dialog.destroy() };
        });
    }
    {
        let e = editor.clone();
        let app = app.clone();
        present_btn.connect_clicked(move |_| show_presentation(&e, &app));
    }
    {
        let e = editor.clone();
        let app = app.clone();
        window.connect_key_press_event(move |_, event| {
            if event.state().contains(gtk::gdk::ModifierType::CONTROL_MASK)
                && event
                    .keyval()
                    .to_unicode()
                    .is_some_and(|c| c.eq_ignore_ascii_case(&'p'))
            {
                show_presentation(&e, &app);
                return gtk::glib::Propagation::Stop;
            }
            gtk::glib::Propagation::Proceed
        });
    }
    {
        let e = editor.clone();
        window.connect_delete_event(move |window, _| {
            if resolve_unsaved(&e, window, "close the editor") {
                gtk::glib::Propagation::Proceed
            } else {
                gtk::glib::Propagation::Stop
            }
        });
    }
    window.show_all();
}

fn register_scheme(state: AppState) {
    let context = webkit2gtk::WebContext::default().expect("WebKit context");
    let manager = context.security_manager().expect("WebKit security manager");
    manager.register_uri_scheme_as_secure("hyperframe");
    manager.register_uri_scheme_as_cors_enabled("hyperframe");
    context.register_uri_scheme("hyperframe", move |request| {
        let resource = request
            .uri()
            .and_then(|uri| resource_for_uri(&state, uri.as_str()));
        let (bytes, mime) = resource.unwrap_or_else(|| (b"Not found".to_vec(), "text/plain"));
        let length = bytes.len() as i64;
        let stream = gtk::gio::MemoryInputStream::from_bytes(&gtk::glib::Bytes::from_owned(bytes));
        request.finish(&stream, length, Some(mime));
    });
}

pub fn launch_audience(state: AppState, url: String) {
    gtk::init().expect("GTK display");
    register_scheme(state.clone());
    let app = gtk::Application::new(None::<&str>, gtk::gio::ApplicationFlags::NON_UNIQUE);
    app.connect_activate(move |app| {
        if let Err(error) = open_audience(app, &url, &state) {
            eprintln!("{{\"error\":{}}}", serde_json::json!(error));
        }
    });
    app.run_with_args(&["hyperframe-slides"]);
}

pub fn launch_gui(state: AppState) {
    gtk::init().expect("GTK display");
    register_scheme(state.clone());
    let app = gtk::Application::new(
        Some("io.github.camerontucker.hyperframeslides"),
        Default::default(),
    );
    let provider = gtk::CssProvider::new();
    provider.load_from_data(b"window { background: #111923; color: #eaf0f6; } .toolbar { background: #17212c; padding: 8px; border-bottom: 1px solid #314151; } .logo { color: #9de8ca; font-size: 17px; font-weight: 800; } .sidebar { background: #18232e; padding: 14px; } .field-label, .slide-count { color: #9de8ca; font-size: 11px; font-weight: 800; letter-spacing: 1px; } .help { color: #a5b5c2; font-size: 12px; } .status { color: #a5b5c2; background: #17212c; padding: 7px 15px; } list row { border-radius: 8px; margin: 3px 0; } list row:selected { background: #385b58; color: white; } button.suggested-action { background: #9de8ca; color: #0e271e; font-weight: 800; }").expect("GTK CSS");
    gtk::StyleContext::add_provider_for_screen(
        &gtk::gdk::Screen::default().expect("display"),
        &provider,
        gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
    );
    app.connect_activate(move |app| {
        if let Some(window) = app
            .windows()
            .into_iter()
            .find(|window| window.title().as_deref() == Some("HyperFrames Slides"))
        {
            window.present();
        } else {
            build(app, state.clone());
        }
    });
    app.run();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn audience_control_accepts_only_defined_actions() {
        assert!(control_script("status").unwrap().contains("slideCount"));
        assert!(control_script("next").unwrap().contains("c.next()"));
        assert!(control_script("prev").unwrap().contains("c.prev()"));
        assert!(control_script("goto 2").unwrap().contains("c.goToSlide(1)"));
        assert!(control_script("notes on").is_err());
        assert!(control_script("notes off").is_err());
        assert!(control_script("notes maybe").is_err());
        assert!(control_script("goto zero").is_err());
        assert!(control_script("quit").is_err());
    }
}
