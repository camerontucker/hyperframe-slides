#![allow(deprecated)] // GTK4 compatibility widgets still support existing editor controls.
use super::*;
use crate::thumbnails::ThumbnailWorker;
use base64::Engine;
use gtk::prelude::*;
use gtk::{Orientation, ResponseType};
use std::{
    cell::{Cell, RefCell},
    io::{Read, Write},
    os::unix::{
        fs::{MetadataExt, PermissionsExt},
        net::UnixListener,
    },
    rc::{Rc, Weak},
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        mpsc, Arc,
    },
};
use webkit::prelude::*;

fn run_dialog(dialog: &impl IsA<gtk::Dialog>) -> ResponseType {
    let dialog = dialog.upcast_ref::<gtk::Dialog>();
    dialog.set_modal(true);
    let loop_ = gtk::glib::MainLoop::new(None, false);
    let response = Rc::new(Cell::new(ResponseType::Cancel));
    let response_for_signal = response.clone();
    let loop_for_signal = loop_.clone();
    let signal = dialog.connect_response(move |_, value| {
        response_for_signal.set(value);
        loop_for_signal.quit();
    });
    dialog.present();
    loop_.run();
    dialog.disconnect(signal);
    response.get()
}

fn configure_acceleration(view: &webkit::WebView) {
    if let Some(settings) = webkit::prelude::WebViewExt::settings(view) {
        settings.set_hardware_acceleration_policy(webkit::HardwareAccelerationPolicy::Always);
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
    self_weak: RefCell<Weak<Editor>>,
    state: AppState,
    deck: RefCell<Deck>,
    last_disk: RefCell<Vec<u8>>,
    last_disk_stamp: Cell<Option<(u64, i64, i64, u64)>>,
    selected: Cell<usize>,
    loading: Cell<bool>,
    dirty: Cell<bool>,
    save_source: RefCell<Option<gtk::glib::SourceId>>,
    list: gtk::ListBox,
    overview: gtk::FlowBox,
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
    picture_list: gtk::Box,
    preview: webkit::WebView,
    thumbnails: Rc<ThumbnailWorker>,
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
            if let Some(id) = entry.path().file_stem().and_then(|x| x.to_str()) {
                if let Ok(deck) = read_deck(state, id) {
                    decks.push(deck);
                }
            }
        }
    }
    decks.sort_by_key(|deck| std::cmp::Reverse(deck.updated_at));
    decks
}

fn text(view: &gtk::TextView) -> String {
    let buffer = view.buffer();
    buffer
        .text(&buffer.start_iter(), &buffer.end_iter(), true)
        .to_string()
}

fn set_text(view: &gtk::TextView, value: &str) {
    view.buffer().set_text(value);
}

fn picture_pixbuf(data_uri: &str) -> Option<gtk::gdk_pixbuf::Pixbuf> {
    let (_, encoded) = data_uri.split_once(',')?;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .ok()?;
    gtk::gdk_pixbuf::Pixbuf::from_read(std::io::Cursor::new(bytes))
        .ok()?
        .scale_simple(72, 48, gtk::gdk_pixbuf::InterpType::Bilinear)
}

fn format_selection(view: &gtk::TextView, style: &str) {
    let buffer = view.buffer();
    if let Some((mut start, mut end)) = buffer.selection_bounds() {
        let selected = buffer.text(&start, &end, true);
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
    parent.append(&editor_label(label));
    let view = gtk::TextView::new();
    view.set_wrap_mode(gtk::WrapMode::WordChar);
    view.set_accepts_tab(false);
    view.set_left_margin(12);
    view.set_right_margin(12);
    view.set_top_margin(10);
    view.set_bottom_margin(10);
    view.set_size_request(-1, lines * 25 + 20);
    let frame = gtk::Frame::new(None);
    frame.set_child(Some(&view));
    parent.append(&frame);
    view
}

impl Editor {
    fn schedule_persist(&self) {
        if self.loading.get() {
            return;
        }
        self.dirty.set(true);
        self.status.set_text("Unsaved changes");
        if let Some(source) = self.save_source.borrow_mut().take() {
            source.remove();
        }
        let editor = self.self_weak.borrow().clone();
        let source =
            gtk::glib::timeout_add_local_once(std::time::Duration::from_millis(300), move || {
                if let Some(editor) = editor.upgrade() {
                    editor.save_source.borrow_mut().take();
                    let _ = editor.persist();
                }
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
        if list_changed || preview_changed {
            self.refresh_list();
        }
        if preview_changed {
            self.refresh_preview();
        }
        result.map(|_| ())
    }

    fn refresh_list(&self) {
        self.loading.set(true);
        self.thumbnails.clear_targets();
        while let Some(row) = self.list.row_at_index(0) {
            self.list.remove(&row);
        }
        while let Some(card) = self.overview.child_at_index(0) {
            self.overview.remove(&card);
        }
        let deck = self.deck.borrow();
        for (index, slide) in deck.slides.iter().enumerate() {
            let row = gtk::ListBoxRow::new();
            row.add_css_class("slide-row");
            let box_ = gtk::Box::new(Orientation::Vertical, 7);
            box_.set_margin_top(10);
            box_.set_margin_bottom(10);
            box_.set_margin_start(10);
            box_.set_margin_end(10);

            let thumb = gtk::Overlay::new();
            thumb.add_css_class("slide-thumb");
            thumb.add_css_class(&format!("thumb-{}", deck.theme));
            thumb.set_overflow(gtk::Overflow::Hidden);
            let fallback = gtk::Box::new(Orientation::Vertical, 4);
            fallback.set_size_request(176, 99);
            fallback.set_margin_top(10);
            fallback.set_margin_start(12);
            fallback.set_margin_end(12);
            let eyebrow = gtk::Label::new(Some(&slide.eyebrow));
            eyebrow.set_xalign(0.0);
            eyebrow.set_ellipsize(gtk::pango::EllipsizeMode::End);
            eyebrow.add_css_class("thumb-eyebrow");
            let fallback_title = gtk::Label::new(Some(&slide.title));
            fallback_title.set_xalign(0.0);
            fallback_title.set_wrap(true);
            fallback_title.set_lines(2);
            fallback_title.add_css_class("thumb-title");
            fallback.append(&eyebrow);
            fallback.append(&fallback_title);
            thumb.set_child(Some(&fallback));
            let picture = gtk::Picture::new();
            picture.set_size_request(176, 99);
            picture.set_content_fit(gtk::ContentFit::Contain);
            picture.set_halign(gtk::Align::Fill);
            picture.set_valign(gtk::Align::Fill);
            picture.set_visible(false);
            thumb.add_overlay(&picture);
            box_.append(&thumb);
            self.thumbnails.request(&deck, index, &picture);

            let count = gtk::Label::new(Some(&format!(
                "{:02} / {:02}",
                index + 1,
                deck.slides.len()
            )));
            count.set_xalign(0.0);
            count.add_css_class("slide-count");
            let title = gtk::Label::new(Some(if slide.title.trim().is_empty() {
                "Untitled slide"
            } else {
                &slide.title
            }));
            title.set_xalign(0.0);
            title.set_wrap(true);
            title.set_lines(2);
            title.set_max_width_chars(23);
            title.add_css_class("slide-title");
            box_.append(&count);
            box_.append(&title);
            row.set_child(Some(&box_));
            self.list.append(&row);
            if index == self.selected.get() {
                self.list.select_row(Some(&row));
            }

            let card = gtk::Box::new(Orientation::Vertical, 8);
            card.add_css_class("overview-card");
            card.set_tooltip_text(Some("Open this slide or drag it before another slide"));
            let drag = gtk::DragSource::new();
            drag.set_actions(gtk::gdk::DragAction::MOVE);
            let source_id = slide.id.clone();
            drag.connect_prepare(move |_, _, _| {
                Some(gtk::gdk::ContentProvider::for_value(&source_id.to_value()))
            });
            card.add_controller(drag);
            let drop = gtk::DropTarget::new(String::static_type(), gtk::gdk::DragAction::MOVE);
            let target_id = slide.id.clone();
            let weak = self.self_weak.borrow().clone();
            drop.connect_drop(move |_, value, _, _| {
                if let (Ok(source_id), Some(editor)) = (value.get::<String>(), weak.upgrade()) {
                    editor.move_slide_before(&source_id, &target_id);
                    true
                } else {
                    false
                }
            });
            card.add_controller(drop);
            let visual = gtk::Overlay::new();
            visual.add_css_class("slide-thumb");
            visual.add_css_class(&format!("thumb-{}", deck.theme));
            let fallback = gtk::Label::new(Some(&slide.title));
            fallback.set_wrap(true);
            fallback.set_size_request(240, 135);
            fallback.add_css_class("thumb-title");
            visual.set_child(Some(&fallback));
            let picture = gtk::Picture::new();
            picture.set_size_request(240, 135);
            picture.set_content_fit(gtk::ContentFit::Contain);
            picture.set_visible(false);
            visual.add_overlay(&picture);
            self.thumbnails.request(&deck, index, &picture);
            card.append(&visual);
            let label = gtk::Label::new(Some(&format!("{:02}  {}", index + 1, slide.title)));
            label.set_xalign(0.0);
            label.set_ellipsize(gtk::pango::EllipsizeMode::End);
            label.set_max_width_chars(34);
            card.append(&label);
            self.overview.insert(&card, -1);
            if let Some(child) = self.overview.child_at_index(index as i32) {
                child.set_focusable(true);
            }
            if index == self.selected.get() {
                if let Some(child) = self.overview.child_at_index(index as i32) {
                    self.overview.select_child(&child);
                }
            }
        }
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
        self.picture_count.set_text(&format!(
            "{} {} on this slide",
            slide.images.len(),
            if slide.images.len() == 1 {
                "picture"
            } else {
                "pictures"
            }
        ));
        self.loading.set(false);
        drop(deck);
        self.refresh_pictures();
        self.refresh_preview();
    }

    fn refresh_pictures(&self) {
        while let Some(child) = self.picture_list.first_child() {
            self.picture_list.remove(&child);
        }
        let deck = self.deck.borrow();
        for (index, image) in deck.slides[self.selected.get()].images.iter().enumerate() {
            let row = gtk::Box::new(Orientation::Horizontal, 9);
            row.add_css_class("picture-row");
            if let Some(pixbuf) = picture_pixbuf(&image.data_uri) {
                let preview = gtk::Picture::for_pixbuf(&pixbuf);
                preview.set_size_request(72, 48);
                preview.set_content_fit(gtk::ContentFit::Contain);
                preview.add_css_class("picture-preview");
                row.append(&preview);
            }
            let details = gtk::Box::new(Orientation::Vertical, 3);
            details.set_hexpand(true);
            let name = gtk::Label::new(Some(&format!("Picture {:02}", index + 1)));
            name.set_xalign(0.0);
            name.add_css_class("picture-name");
            details.append(&name);
            let alt = gtk::Entry::new();
            alt.set_max_length(500);
            alt.set_text(&image.alt);
            alt.set_placeholder_text(Some("Description for accessibility"));
            alt.set_tooltip_text(Some(
                "Picture description for the audience and exported HTML",
            ));
            let weak = self.self_weak.borrow().clone();
            let id = image.id.clone();
            alt.connect_changed(move |entry| {
                if let Some(editor) = weak.upgrade() {
                    editor.set_image_alt(&id, &entry.text());
                }
            });
            details.append(&alt);
            row.append(&details);
            let remove = button("Remove");
            remove.add_css_class("flat");
            remove.set_valign(gtk::Align::Center);
            let weak = self.self_weak.borrow().clone();
            let id = image.id.clone();
            remove.connect_clicked(move |_| {
                if let Some(editor) = weak.upgrade() {
                    editor.remove_image(&id);
                }
            });
            row.append(&remove);
            self.picture_list.append(&row);
        }
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
            let current = parse_deck(&deck.id, &bytes, &self.state.data_dir.join("decks"))?;
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

    fn move_slide_before(&self, source_id: &str, target_id: &str) {
        if source_id == target_id || (self.dirty.get() && self.persist().is_err()) {
            return;
        }
        let mut deck = self.deck.borrow_mut();
        let Some(source) = deck.slides.iter().position(|slide| slide.id == source_id) else {
            return;
        };
        let Some(target) = deck.slides.iter().position(|slide| slide.id == target_id) else {
            return;
        };
        let selected_id = deck.slides[self.selected.get()].id.clone();
        let slide = deck.slides.remove(source);
        deck.slides
            .insert(if source < target { target - 1 } else { target }, slide);
        self.selected.set(
            deck.slides
                .iter()
                .position(|slide| slide.id == selected_id)
                .unwrap_or(0),
        );
        drop(deck);
        self.refresh_list();
        self.refresh_fields();
        let _ = self.persist();
    }

    fn overview_selection(&self) -> Vec<usize> {
        let mut indices = self
            .overview
            .selected_children()
            .iter()
            .map(|child| child.index() as usize)
            .collect::<Vec<_>>();
        indices.sort_unstable();
        indices
    }

    fn duplicate_overview_selection(&self) {
        if self.dirty.get() && self.persist().is_err() {
            return;
        }
        let indices = self.overview_selection();
        if indices.is_empty() {
            return;
        }
        let mut deck = self.deck.borrow_mut();
        for (offset, index) in indices.into_iter().enumerate() {
            let position = index + offset;
            let mut slide = deck.slides[position].clone();
            slide.id = format!("slide-{}", unique_id());
            deck.slides.insert(position + 1, slide);
            self.selected.set(position + 1);
        }
        drop(deck);
        self.refresh_list();
        self.refresh_fields();
        let _ = self.persist();
    }

    fn delete_overview_selection(&self) {
        if self.dirty.get() && self.persist().is_err() {
            return;
        }
        let indices = self.overview_selection();
        if indices.is_empty() {
            return;
        }
        let mut deck = self.deck.borrow_mut();
        if indices.len() >= deck.slides.len() {
            self.status.set_text("Keep at least one slide");
            return;
        }
        for index in indices.iter().rev() {
            deck.slides.remove(*index);
        }
        self.selected.set(indices[0].min(deck.slides.len() - 1));
        drop(deck);
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
                self.refresh_list();
                let _ = self.persist();
            }
            Err(error) => self.status.set_text(&format!("Picture failed: {error}")),
        }
    }

    fn insert_clipboard_picture(&self, texture: &gtk::gdk::Texture) {
        if self.dirty.get() && self.persist().is_err() {
            return;
        }
        let png = texture.save_to_png_bytes();
        if png.len() > 8_000_000 {
            self.status
                .set_text("Clipboard picture is larger than 8 MB");
            return;
        }
        let uri = format!(
            "data:image/png;base64,{}",
            base64::engine::general_purpose::STANDARD.encode(png.as_ref())
        );
        let mut deck = self.deck.borrow_mut();
        let index = self.selected.get();
        let result = add_image_uri_to_slide(&mut deck.slides[index], uri, "Pasted picture");
        drop(deck);
        match result {
            Ok(_) => {
                self.refresh_fields();
                self.refresh_list();
                let _ = self.persist();
            }
            Err(error) => self.status.set_text(&format!("Picture failed: {error}")),
        }
    }

    fn remove_image(&self, id: &str) {
        if self.dirty.get() && self.persist().is_err() {
            return;
        }
        let mut deck = self.deck.borrow_mut();
        let images = &mut deck.slides[self.selected.get()].images;
        let Some(index) = images.iter().position(|image| image.id == id) else {
            return;
        };
        images.remove(index);
        drop(deck);
        self.refresh_fields();
        self.refresh_list();
        let _ = self.persist();
    }

    fn set_image_alt(&self, id: &str, alt: &str) {
        if self.loading.get() || alt.len() > 500 {
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
        if image.alt == alt {
            return;
        }
        image.alt = alt.to_string();
        drop(deck);
        self.schedule_persist();
        self.refresh_preview();
    }

    fn set_logo(&self, path: &std::path::Path) {
        if self.dirty.get() && self.persist().is_err() {
            return;
        }
        match image_data_uri(path, 4_000_000) {
            Ok(uri) => {
                self.deck.borrow_mut().template.logo = Some(uri);
                self.refresh_preview();
                self.refresh_list();
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
        self.refresh_list();
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
                self.refresh_list();
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
        self.refresh_list();
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
        self.refresh_list();
        let _ = self.persist();
    }
}

fn button(label: &str) -> gtk::Button {
    gtk::Button::with_label(label)
}

fn paste_picture(editor: &Rc<Editor>) {
    let clipboard = gtk::gdk::Display::default().expect("display").clipboard();
    let editor = editor.clone();
    clipboard.read_texture_async(None::<&gtk::gio::Cancellable>, move |result| match result {
        Ok(Some(texture)) => editor.insert_clipboard_picture(&texture),
        _ => editor
            .status
            .set_text("Clipboard does not contain a picture"),
    });
}

fn focused_text_input(window: &gtk::ApplicationWindow) -> bool {
    gtk::prelude::GtkWindowExt::focus(window).is_some_and(|focus| {
        focus.is::<gtk::Entry>()
            || focus.is::<gtk::TextView>()
            || focus.ancestor(gtk::Entry::static_type()).is_some()
            || focus.ancestor(gtk::TextView::static_type()).is_some()
    })
}

fn update_workspace(
    compact: bool,
    editing: bool,
    bar: &gtk::Box,
    canvas: &gtk::Box,
    inspector: &gtk::Box,
    canvas_button: &gtk::Button,
    edit_button: &gtk::Button,
) {
    bar.set_visible(compact);
    canvas.set_visible(!compact || !editing);
    inspector.set_visible(!compact || editing);
    if editing {
        edit_button.add_css_class("view-active");
        canvas_button.remove_css_class("view-active");
    } else {
        canvas_button.add_css_class("view-active");
        edit_button.remove_css_class("view-active");
    }
}

fn choose_picture(parent: &gtk::ApplicationWindow, title: &str) -> Option<PathBuf> {
    let dialog = gtk::FileChooserDialog::new(
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
    dialog.add_filter(&filter);
    let path = if run_dialog(&dialog) == ResponseType::Accept {
        dialog.file().and_then(|file| file.path())
    } else {
        None
    };
    dialog.close();
    path
}

fn choose_font(parent: &gtk::ApplicationWindow, title: &str) -> Option<PathBuf> {
    let dialog = gtk::FileChooserDialog::new(
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
    dialog.add_filter(&filter);
    let path = if run_dialog(&dialog) == ResponseType::Accept {
        dialog.file().and_then(|file| file.path())
    } else {
        None
    };
    dialog.close();
    path
}

fn release_session(state: &AppState, token: &str) {
    if let Ok(mut sessions) = state.presentations.lock() {
        sessions.remove(token);
    }
}

fn set_hyprland_window_opaque(address: &str) {
    for property in ["opacity", "opacity_inactive"] {
        let command = format!(
            "hl.dispatch(hl.dsp.window.set_prop({{ prop = '{property}', value = '1', window = 'address:{address}' }}))"
        );
        let _ = std::process::Command::new("hyprctl")
            .args(["eval", &command])
            .output();
    }
}

fn prepare_editor_on_hyprland() {
    if std::env::var_os("HYPRLAND_INSTANCE_SIGNATURE").is_none() {
        return;
    }
    let Ok(output) = std::process::Command::new("hyprctl")
        .args(["clients", "-j"])
        .output()
    else {
        return;
    };
    let Ok(clients) = serde_json::from_slice::<serde_json::Value>(&output.stdout) else {
        return;
    };
    let Some(address) = clients
        .as_array()
        .and_then(|clients| {
            clients.iter().find(|client| {
                client["pid"].as_u64() == Some(std::process::id() as u64)
                    && client["title"].as_str() == Some("HyperFrames Slides")
            })
        })
        .and_then(|client| client["address"].as_str())
    else {
        return;
    };
    if address.starts_with("0x") && address[2..].chars().all(|c| c.is_ascii_hexdigit()) {
        set_hyprland_window_opaque(address);
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
    // Shared presentation windows must not reveal the desktop behind them.
    set_hyprland_window_opaque(address);
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
    view: &webkit::WebView,
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
                    view.evaluate_javascript(&script, None, None, None::<&gtk::gio::Cancellable>, move |result| {
                        let mut value = match result {
                            Ok(result) => serde_json::from_str::<serde_json::Value>(&result.to_string()).ok()
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
    window.connect_close_request(move |_| {
        running.store(false, Ordering::Relaxed);
        if let Some(source) = source.borrow_mut().take() {
            source.remove();
        }
        let _ = fs::remove_file(&path_for_close);
        gtk::glib::Propagation::Proceed
    });
    Ok(())
}

fn open_audience(app: &gtk::Application, url: &str, state: &AppState) -> Result<(), ApiError> {
    let window = gtk::ApplicationWindow::new(app);
    window.set_title(Some("HyperFrames Audience · share this window in Zoom"));
    window.set_default_size(1280, 720);
    let view = webkit::WebView::new();
    configure_acceleration(&view);
    view.connect_load_failed(|_, _, uri, error| {
        eprintln!("Audience failed to load {uri}: {error}");
        false
    });
    window.set_child(Some(&view));
    let token = url
        .strip_prefix("hyperframe://app/")
        .and_then(|path| path.split('/').next())
        .unwrap_or_default()
        .to_string();
    {
        let state = state.clone();
        let token = token.clone();
        window.connect_close_request(move |_| {
            release_session(&state, &token);
            gtk::glib::Propagation::Proceed
        });
    }
    let keys = gtk::EventControllerKey::new();
    let window_for_key = window.clone();
    keys.connect_key_pressed(move |_, key, _, _| {
        if key == gtk::gdk::Key::Escape {
            let window = window_for_key.clone();
            gtk::glib::idle_add_local_once(move || window.close());
            gtk::glib::Propagation::Stop
        } else {
            gtk::glib::Propagation::Proceed
        }
    });
    window.add_controller(keys);
    register_control(state, &token, &view, &window)?;
    view.load_uri(url);
    window.present();
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

fn change_preview(title: &str) -> (gtk::Frame, gtk::Picture, gtk::Label) {
    let frame = gtk::Frame::new(Some(title));
    let overlay = gtk::Overlay::new();
    overlay.set_size_request(480, 270);
    let picture = gtk::Picture::new();
    picture.set_content_fit(gtk::ContentFit::Contain);
    picture.set_visible(false);
    overlay.set_child(Some(&picture));
    let fallback = gtk::Label::new(Some("Rendering preview…"));
    fallback.set_wrap(true);
    overlay.add_overlay(&fallback);
    let fallback_for_visibility = fallback.clone();
    picture.connect_visible_notify(move |picture| {
        fallback_for_visibility.set_visible(!picture.is_visible());
    });
    frame.set_child(Some(&overlay));
    (frame, picture, fallback)
}

fn show_change_review(editor: &Rc<Editor>, parent: &gtk::ApplicationWindow) {
    if !resolve_unsaved(editor, parent, "review changes") {
        return;
    }
    let id = editor.deck.borrow().id.clone();
    let versions = match history::list(&editor.state, &id) {
        Ok(value) => value,
        Err(error) => {
            editor.status.set_text(&format!("Review failed: {error}"));
            return;
        }
    };
    let Some(entries) = versions["history"].as_array() else {
        editor.status.set_text("Could not read version history");
        return;
    };
    if entries.is_empty() {
        editor
            .status
            .set_text("No earlier saved version to compare");
        return;
    }
    let picker = gtk::Dialog::with_buttons(
        Some("Compare with saved version"),
        Some(parent),
        gtk::DialogFlags::MODAL,
        &[
            ("Cancel", ResponseType::Cancel),
            ("Compare", ResponseType::Accept),
        ],
    );
    let picker_content = gtk::Box::new(Orientation::Vertical, 12);
    picker_content.set_margin_top(18);
    picker_content.set_margin_bottom(18);
    picker_content.set_margin_start(18);
    picker_content.set_margin_end(18);
    picker_content.append(&gtk::Label::new(Some(
        "Choose the saved version to compare with the current deck.",
    )));
    let version_choice = gtk::ComboBoxText::new();
    for entry in entries {
        let Some(hash) = entry["revision"].as_str() else {
            continue;
        };
        let count = entry["slideCount"].as_u64().unwrap_or_default();
        version_choice.append(Some(hash), &format!("{count} slides · {}", &hash[..8]));
    }
    version_choice.set_active(Some(0));
    picker_content.append(&version_choice);
    picker.content_area().append(&picker_content);
    let selected_revision = if run_dialog(&picker) == ResponseType::Accept {
        version_choice.active_id().map(|value| value.to_string())
    } else {
        None
    };
    picker.close();
    let Some(selected_revision) = selected_revision else {
        return;
    };
    let (current, current_revision) = match read_deck_snapshot(&editor.state, &id) {
        Ok(snapshot) => snapshot,
        Err(error) => {
            editor.status.set_text(&format!("Review failed: {error}"));
            return;
        }
    };
    let before = match history::load(&editor.state, &id, &selected_revision) {
        Ok(deck) => deck,
        Err(error) => {
            editor.status.set_text(&format!("Review failed: {error}"));
            return;
        }
    };
    let changes = diff::changes(&before, &current);
    if changes.is_empty() {
        editor.status.set_text(if before.title != current.title {
            "Only the deck title changed"
        } else {
            "No changes from that saved version"
        });
        return;
    }
    let before = Rc::new(before);
    let current = Rc::new(current);
    let changes = Rc::new(changes);
    let viewer = gtk::Dialog::new();
    viewer.set_title(Some("Review changes"));
    viewer.set_transient_for(Some(parent));
    viewer.set_modal(true);
    viewer.set_default_size(1050, 460);
    viewer.add_button("Done", ResponseType::Close);
    let revert_button = viewer.add_button("Revert selected slide", ResponseType::Other(1));
    let content = gtk::Box::new(Orientation::Vertical, 14);
    content.set_margin_top(18);
    content.set_margin_bottom(18);
    content.set_margin_start(18);
    content.set_margin_end(18);
    let slide_choice = gtk::ComboBoxText::new();
    for (index, change) in changes.iter().enumerate() {
        slide_choice.append(
            Some(&index.to_string()),
            &format!("{} · {}", change.kind, change.title),
        );
    }
    content.append(&slide_choice);
    let previews = gtk::Box::new(Orientation::Horizontal, 14);
    let (old_frame, old_picture, old_fallback) = change_preview("Before");
    let (new_frame, new_picture, new_fallback) = change_preview("After");
    previews.append(&old_frame);
    previews.append(&new_frame);
    content.append(&previews);
    let summary = gtk::Label::new(None);
    summary.set_wrap(true);
    summary.set_xalign(0.0);
    content.append(&summary);
    viewer.content_area().append(&content);
    {
        let changes = changes.clone();
        let before = before.clone();
        let current = current.clone();
        let worker = editor.thumbnails.clone();
        slide_choice.connect_changed(move |choice| {
            let Some(index) = choice.active().map(|value| value as usize) else {
                return;
            };
            let Some(change) = changes.get(index) else {
                return;
            };
            summary.set_text(&change.summary.join(" · "));
            revert_button.set_sensitive(change.can_revert);
            for (picture, fallback, position, deck) in [
                (&old_picture, &old_fallback, change.before_index, &before),
                (&new_picture, &new_fallback, change.after_index, &current),
            ] {
                worker.forget_target(picture);
                picture.set_visible(false);
                picture.set_paintable(None::<&gtk::gdk::Texture>);
                if let Some(position) = position {
                    fallback.set_text("Rendering preview…");
                    worker.request(deck, position, picture);
                } else {
                    fallback.set_text("No slide in this version");
                }
            }
        });
    }
    slide_choice.set_active(Some(0));
    let result = run_dialog(&viewer);
    let selected_id = slide_choice
        .active()
        .and_then(|index| changes.get(index as usize))
        .map(|change| change.id.clone());
    viewer.close();
    if result == ResponseType::Other(1) {
        if let Some(slide_id) = selected_id {
            match diff::revert_slide(
                &editor.state,
                &id,
                &selected_revision,
                &slide_id,
                &current_revision,
            ) {
                Ok(_) => match read_deck(&editor.state, &id) {
                    Ok(saved) => editor.replace_deck(saved),
                    Err(error) => editor.status.set_text(&format!("Reload failed: {error}")),
                },
                Err(error) => editor.status.set_text(&format!("Revert failed: {error}")),
            }
        }
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
    message.set_wrap(true);
    message.set_margin_top(18);
    message.set_margin_bottom(18);
    message.set_margin_start(18);
    message.set_margin_end(18);
    dialog.content_area().append(&message);
    dialog.present();
    let choice = run_dialog(&dialog);
    // GTK keeps a dialog alive after run(); nothing reads it after this point.
    dialog.close();
    match choice {
        ResponseType::Reject => true,
        ResponseType::Other(1) => {
            let chooser = gtk::FileChooserDialog::new(
                Some("Save a recovery copy"),
                Some(parent),
                gtk::FileChooserAction::Save,
                &[
                    ("Cancel", ResponseType::Cancel),
                    ("Save copy", ResponseType::Accept),
                ],
            );

            chooser.set_current_name(&format!("{}-recovery.json", editor.deck.borrow().id));
            let result = if run_dialog(&chooser) == ResponseType::Accept {
                chooser.file().and_then(|file| file.path()).map(|path| {
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
            chooser.close();
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
    window.set_title(Some("HyperFrames Slides"));
    window.set_default_size(1440, 880);
    let root = gtk::Box::new(Orientation::Vertical, 0);
    let header = gtk::Box::new(Orientation::Horizontal, 8);
    header.style_context().add_class("toolbar");
    let logo = gtk::Label::new(Some("◈  HyperFrames Slides"));
    logo.style_context().add_class("logo");
    header.append(&logo);
    let deck_title = gtk::Entry::new();
    deck_title.set_width_chars(30);
    deck_title.set_max_length(160);
    deck_title.add_css_class("deck-title");
    header.append(&deck_title);
    let new_btn = button("New");
    let duplicate_deck_btn = button("Duplicate deck");
    let overview_btn = button("Overview");
    duplicate_deck_btn.set_tooltip_text(Some("Create a new presentation from this deck"));
    let open_btn = button("Import bundle");
    let recent_btn = button("Recent");
    let reload_btn = button("Reload");
    let history_btn = button("Version history");
    let changes_btn = button("Review changes");
    let save_btn = button("Save");
    let export_btn = button("Export HTML");
    let export_audience_btn = button("Export for audience");
    let present_btn = button("Present");
    let menu_button = gtk::MenuButton::new();
    menu_button.set_label("Menu");
    menu_button.set_tooltip_text(Some("Presentation and file actions"));
    let menu = gtk::Popover::new();
    menu.add_css_class("action-menu");
    let menu_items = gtk::Box::new(Orientation::Vertical, 3);
    menu_items.set_margin_top(10);
    menu_items.set_margin_bottom(10);
    menu_items.set_margin_start(10);
    menu_items.set_margin_end(10);
    for (index, (label, action, shortcut)) in [
        ("New", &new_btn, "Ctrl+N"),
        ("Duplicate deck", &duplicate_deck_btn, ""),
        ("Overview", &overview_btn, "Ctrl+G"),
        ("Import bundle", &open_btn, "Ctrl+O"),
        ("Recent", &recent_btn, ""),
        ("Reload", &reload_btn, ""),
        ("Version history", &history_btn, ""),
        ("Review changes", &changes_btn, ""),
        ("Save", &save_btn, "Ctrl+S"),
        ("Export HTML", &export_btn, ""),
        ("Export for audience", &export_audience_btn, ""),
    ]
    .into_iter()
    .enumerate()
    {
        if let Some(section) = match index {
            0 => Some("PRESENTATION"),
            3 => Some("FILE"),
            9 => Some("EXPORT"),
            _ => None,
        } {
            let heading = gtk::Label::new(Some(section));
            heading.set_xalign(0.0);
            heading.add_css_class("menu-section");
            menu_items.append(&heading);
        }
        let item = gtk::Button::new();
        item.add_css_class("menu-action");
        let item_content = gtk::Box::new(Orientation::Horizontal, 18);
        let name = gtk::Label::new(Some(label));
        name.set_xalign(0.0);
        name.set_hexpand(true);
        item_content.append(&name);
        if !shortcut.is_empty() {
            let hint = gtk::Label::new(Some(shortcut));
            hint.add_css_class("shortcut-hint");
            item_content.append(&hint);
        }
        item.set_child(Some(&item_content));
        let action = action.clone();
        let popover = menu.clone();
        item.connect_clicked(move |_| {
            popover.popdown();
            action.emit_clicked();
        });
        menu_items.append(&item);
    }
    menu.set_child(Some(&menu_items));
    menu_button.set_popover(Some(&menu));
    menu_button.add_css_class("menu-trigger");
    header.append(&menu_button);
    header.append(&present_btn);
    present_btn.style_context().add_class("suggested-action");
    root.append(&header);
    let main = gtk::Paned::new(Orientation::Horizontal);
    main.set_hexpand(true);
    main.set_vexpand(true);
    main.set_resize_start_child(false);
    main.set_shrink_start_child(false);
    main.set_position(225);
    let left = gtk::Box::new(Orientation::Vertical, 8);
    left.set_size_request(190, -1);
    left.style_context().add_class("sidebar");
    let slides_heading = editor_label("SLIDES");
    left.append(&slides_heading);
    let list = gtk::ListBox::new();
    list.set_selection_mode(gtk::SelectionMode::Single);
    let scroller = gtk::ScrolledWindow::new();
    scroller.set_vexpand(true);
    scroller.set_child(Some(&list));
    left.append(&scroller);
    let add_btn = button("＋ Add slide");
    let duplicate_btn = button("Duplicate");
    let remove_btn = button("Delete");
    let slide_buttons = gtk::Box::new(Orientation::Vertical, 6);
    for b in [&add_btn, &duplicate_btn, &remove_btn] {
        slide_buttons.append(b);
    }
    let movement = gtk::Box::new(Orientation::Horizontal, 6);
    let up_btn = button("↑ Move");
    let down_btn = button("↓ Move");
    movement.append(&up_btn);
    movement.append(&down_btn);
    slide_buttons.append(&movement);
    left.append(&slide_buttons);
    main.set_start_child(Some(&left));

    let content_area = gtk::Box::new(Orientation::Vertical, 0);
    content_area.set_hexpand(true);
    content_area.set_vexpand(true);
    let compact_bar = gtk::Box::new(Orientation::Horizontal, 6);
    compact_bar.add_css_class("compact-bar");
    compact_bar.set_visible(false);
    let canvas_btn = button("Canvas");
    let edit_btn = button("Edit slide");
    edit_btn.set_tooltip_text(Some("Toggle the inspector with Ctrl+E"));
    canvas_btn.add_css_class("view-active");
    compact_bar.append(&canvas_btn);
    compact_bar.append(&edit_btn);
    content_area.append(&compact_bar);
    let content = gtk::Paned::new(Orientation::Horizontal);
    content.set_hexpand(true);
    content.set_vexpand(true);
    content.set_resize_end_child(false);
    content.set_shrink_start_child(false);
    content.set_shrink_end_child(true);
    content.set_position(760);
    let center = gtk::Box::new(Orientation::Vertical, 8);
    center.set_hexpand(true);
    center.set_vexpand(true);
    center.set_margin_top(20);
    center.set_margin_bottom(20);
    center.set_margin_start(20);
    center.set_margin_end(20);
    let preview_label = editor_label("CANVAS  ·  AUDIENCE VIEW  ·  16:9");
    center.append(&preview_label);
    let preview_manager = webkit::UserContentManager::new();
    preview_manager.register_script_message_handler("imagePosition", None);
    let preview = webkit::WebView::builder()
        .user_content_manager(&preview_manager)
        .build();
    configure_acceleration(&preview);
    preview.set_size_request(320, 180);
    preview.set_hexpand(true);
    preview.set_vexpand(true);
    let thumbnail_view = webkit::WebView::new();
    thumbnail_view.set_size_request(320, 180);
    let stage_layers = gtk::Overlay::new();
    stage_layers.set_child(Some(&thumbnail_view));
    stage_layers.add_overlay(&preview);
    stage_layers.set_measure_overlay(&preview, true);
    let preview_frame = gtk::Frame::new(None);
    preview_frame.set_child(Some(&stage_layers));
    let aspect = gtk::AspectFrame::new(0.5, 0.5, 16.0 / 9.0, false);
    aspect.set_child(Some(&preview_frame));
    center.append(&aspect);
    let fit_label = gtk::Label::new(Some("Checking slide fit…"));
    fit_label.set_xalign(0.0);
    fit_label.style_context().add_class("help");
    center.append(&fit_label);
    preview.connect_notify_local(Some("title"), move |view, _| {
        if let Some(title) = view.title() {
            if title.starts_with("Check slide:") || title.starts_with("No clipping") {
                fit_label.set_text(&title);
            }
        }
    });
    let help = gtk::Label::new(Some(
        "Drag pictures on the slide to position them. Present opens the Audience window for Zoom.",
    ));
    help.set_wrap(true);
    help.set_xalign(0.0);
    help.style_context().add_class("help");
    center.append(&help);
    content.set_start_child(Some(&center));
    let inspector_shell = gtk::Box::new(Orientation::Vertical, 0);
    inspector_shell.add_css_class("inspector");
    inspector_shell.set_size_request(290, -1);
    let inspector_stack = gtk::Stack::new();
    inspector_stack.set_vexpand(true);
    let inspector_switcher = gtk::StackSwitcher::new();
    inspector_switcher.set_stack(Some(&inspector_stack));
    inspector_switcher.add_css_class("inspector-switcher");
    inspector_shell.append(&inspector_switcher);
    inspector_shell.append(&inspector_stack);
    let fields_scroll = gtk::ScrolledWindow::new();
    fields_scroll.set_vexpand(true);
    fields_scroll.set_size_request(270, -1);
    let fields = gtk::Box::new(Orientation::Vertical, 8);
    fields.set_margin_top(20);
    fields.set_margin_bottom(20);
    fields.set_margin_start(18);
    fields.set_margin_end(18);
    fields.append(&editor_label("EDIT SLIDE"));
    fields.append(&editor_label("Layout"));
    let layout = gtk::ComboBoxText::new();
    for (id, label) in [
        ("title", "Title"),
        ("statement", "Statement"),
        ("split", "Split"),
        ("quote", "Quote"),
    ] {
        layout.append(Some(id), label);
    }
    fields.append(&layout);
    fields.append(&editor_label("Eyebrow"));
    let eyebrow = gtk::Entry::new();
    eyebrow.set_max_length(100);
    fields.append(&eyebrow);
    let headline = text_field(&fields, "Headline", 4);
    let body = text_field(&fields, "Supporting text", 5);
    let formatting = gtk::Box::new(Orientation::Horizontal, 6);
    let bold_btn = button("Bold");
    let italic_btn = button("Italic");
    let bullets_btn = button("Bullets");
    for widget in [&bold_btn, &italic_btn, &bullets_btn] {
        formatting.append(widget);
    }
    fields.append(&formatting);
    let notes_toggle = gtk::CheckButton::with_label("Show speaker notes");
    fields.append(&notes_toggle);
    let notes_revealer = gtk::Revealer::new();
    notes_revealer.set_reveal_child(false);
    let notes_panel = gtk::Box::new(Orientation::Vertical, 8);
    let notes = text_field(&notes_panel, "Speaker notes", 8);
    notes_revealer.set_child(Some(&notes_panel));
    fields.append(&notes_revealer);
    notes_toggle.connect_toggled(move |toggle| {
        notes_revealer.set_reveal_child(toggle.is_active());
    });
    fields.append(&editor_label("Slide animation"));
    let animation = gtk::ComboBoxText::new();
    for (id, label) in [
        ("none", "None"),
        ("fade", "Fade"),
        ("rise", "Rise"),
        ("zoom", "Zoom"),
    ] {
        animation.append(Some(id), label);
    }
    fields.append(&animation);
    fields.append(&editor_label("PICTURES"));
    let picture_count = editor_label("0 pictures on this slide");
    fields.append(&picture_count);
    let picture_list = gtk::Box::new(Orientation::Vertical, 6);
    fields.append(&picture_list);
    let insert_picture_btn = button("Insert picture");
    fields.append(&insert_picture_btn);
    let paste_picture_btn = button("Paste picture");
    paste_picture_btn.set_tooltip_text(Some("Insert an image copied to the clipboard"));
    fields.append(&paste_picture_btn);
    let picture_drop = gtk::Frame::new(None);
    let picture_drop_label = gtk::Label::new(Some("Drop PNG, JPEG, GIF, or WebP here"));
    picture_drop_label.set_margin_top(16);
    picture_drop_label.set_margin_bottom(16);
    picture_drop.set_child(Some(&picture_drop_label));
    picture_drop.add_css_class("picture-drop");
    fields.append(&picture_drop);
    fields_scroll.set_child(Some(&fields));
    inspector_stack.add_titled(&fields_scroll, Some("slide"), "Slide");

    let deck_scroll = gtk::ScrolledWindow::new();
    deck_scroll.set_vexpand(true);
    let deck_fields = gtk::Box::new(Orientation::Vertical, 9);
    deck_fields.set_margin_top(20);
    deck_fields.set_margin_bottom(24);
    deck_fields.set_margin_start(18);
    deck_fields.set_margin_end(18);
    deck_fields.append(&editor_label("DECK DESIGN"));
    let deck_help = gtk::Label::new(Some("These choices appear on every slide."));
    deck_help.set_xalign(0.0);
    deck_help.set_wrap(true);
    deck_help.add_css_class("help");
    deck_fields.append(&deck_help);
    deck_fields.append(&editor_label("Theme"));
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
    deck_fields.append(&theme);
    deck_fields.append(&editor_label("HEADER AND FOOTER"));
    deck_fields.append(&editor_label("Header"));
    let template_header = gtk::Entry::new();
    template_header.set_max_length(500);
    deck_fields.append(&template_header);
    deck_fields.append(&editor_label("Footer"));
    let template_footer = gtk::Entry::new();
    template_footer.set_max_length(500);
    deck_fields.append(&template_footer);
    let template_outline = gtk::CheckButton::with_label("Show outline to audience");
    deck_fields.append(&template_outline);
    deck_fields.append(&editor_label("LOGO"));
    let logo_buttons = gtk::Box::new(Orientation::Horizontal, 6);
    let set_logo_btn = button("Set logo");
    let clear_logo_btn = button("Clear logo");
    logo_buttons.append(&set_logo_btn);
    logo_buttons.append(&clear_logo_btn);
    deck_fields.append(&logo_buttons);
    deck_fields.append(&editor_label("FONTS"));
    let font_status = editor_label("Fonts: heading default · body default");
    deck_fields.append(&font_status);
    let font_buttons = gtk::Box::new(Orientation::Horizontal, 6);
    let set_heading_font_btn = button("Heading font");
    let set_body_font_btn = button("Body font");
    let clear_fonts_btn = button("Clear fonts");
    font_buttons.append(&set_heading_font_btn);
    font_buttons.append(&set_body_font_btn);
    font_buttons.append(&clear_fonts_btn);
    deck_fields.append(&font_buttons);
    deck_scroll.set_child(Some(&deck_fields));
    inspector_stack.add_titled(&deck_scroll, Some("deck"), "Deck");
    content.set_end_child(Some(&inspector_shell));
    content_area.append(&content);
    main.set_end_child(Some(&content_area));
    let workspace_stack = gtk::Stack::new();
    workspace_stack.set_hexpand(true);
    workspace_stack.set_vexpand(true);
    workspace_stack.add_named(&main, Some("editor"));
    let overview_page = gtk::Box::new(Orientation::Vertical, 12);
    overview_page.add_css_class("overview-page");
    let overview_header = gtk::Box::new(Orientation::Horizontal, 12);
    let overview_heading = gtk::Label::new(Some("Slide overview"));
    overview_heading.add_css_class("overview-heading");
    overview_heading.set_hexpand(true);
    overview_heading.set_xalign(0.0);
    overview_header.append(&overview_heading);
    let overview_duplicate_btn = button("Duplicate selected");
    overview_header.append(&overview_duplicate_btn);
    let overview_delete_btn = button("Delete selected");
    overview_header.append(&overview_delete_btn);
    let back_btn = button("Back to editor");
    overview_header.append(&back_btn);
    overview_page.append(&overview_header);
    let overview = gtk::FlowBox::new();
    overview.set_selection_mode(gtk::SelectionMode::Multiple);
    overview.set_activate_on_single_click(false);
    overview.set_min_children_per_line(1);
    overview.set_max_children_per_line(5);
    overview.set_column_spacing(18);
    overview.set_row_spacing(18);
    overview.set_valign(gtk::Align::Start);
    let overview_scroll = gtk::ScrolledWindow::new();
    overview_scroll.set_vexpand(true);
    overview_scroll.set_child(Some(&overview));
    overview_page.append(&overview_scroll);
    workspace_stack.add_named(&overview_page, Some("overview"));
    root.append(&workspace_stack);
    let status = gtk::Label::new(Some("Saved locally"));
    status.set_xalign(0.0);
    status.style_context().add_class("status");
    root.append(&status);
    window.set_child(Some(&root));
    let initial_disk = deck_path(&state, &initial.id)
        .and_then(|path| fs::read(path).map_err(internal))
        .unwrap_or_default();
    let initial_stamp = deck_path(&state, &initial.id)
        .ok()
        .and_then(|path| file_stamp(&path));
    let thumbnails = ThumbnailWorker::new(&thumbnail_view);
    let editor = Rc::new(Editor {
        self_weak: RefCell::new(Weak::new()),
        state,
        deck: RefCell::new(initial),
        last_disk: RefCell::new(initial_disk),
        last_disk_stamp: Cell::new(initial_stamp),
        selected: Cell::new(0),
        loading: Cell::new(false),
        dirty: Cell::new(false),
        save_source: RefCell::new(None),
        list,
        overview,
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
        picture_list,
        preview,
        thumbnails: thumbnails.clone(),
        status,
    });
    *editor.self_weak.borrow_mut() = Rc::downgrade(&editor);
    let initial = editor.deck.borrow().clone();
    editor.replace_deck(initial);
    {
        let stack = workspace_stack.clone();
        let overview = editor.overview.clone();
        let selected = editor.clone();
        overview_btn.connect_clicked(move |_| {
            stack.set_visible_child_name("overview");
            let overview = overview.clone();
            let selected = selected.clone();
            gtk::glib::idle_add_local_once(move || {
                if let Some(card) = overview.child_at_index(selected.selected.get() as i32) {
                    card.grab_focus();
                }
            });
        });
    }
    {
        let stack = workspace_stack.clone();
        back_btn.connect_clicked(move |_| stack.set_visible_child_name("editor"));
    }
    {
        let e = editor.clone();
        overview_duplicate_btn.connect_clicked(move |_| e.duplicate_overview_selection());
    }
    {
        let e = editor.clone();
        overview_delete_btn.connect_clicked(move |_| e.delete_overview_selection());
    }
    {
        let e = editor.clone();
        let stack = workspace_stack.clone();
        editor.overview.connect_child_activated(move |_, child| {
            e.select(child.index() as usize);
            stack.set_visible_child_name("editor");
        });
    }
    {
        let e = editor.clone();
        preview_manager.connect_script_message_received(Some("imagePosition"), move |_, value| {
            if let Ok(position) = serde_json::from_str::<serde_json::Value>(&value.to_string()) {
                if let (Some(id), Some(x), Some(y)) = (
                    position.get("id").and_then(|v| v.as_str()),
                    position.get("x").and_then(|v| v.as_f64()),
                    position.get("y").and_then(|v| v.as_f64()),
                ) {
                    e.move_image(id, x as f32, y as f32);
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
        view.buffer().connect_changed(move |_| {
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
        paste_picture_btn.connect_clicked(move |_| paste_picture(&e));
    }
    {
        let e = editor.clone();
        let drop = gtk::DropTarget::new(
            gtk::gdk::FileList::static_type(),
            gtk::gdk::DragAction::COPY,
        );
        drop.connect_drop(move |_, value, _, _| {
            if let Ok(files) = value.get::<gtk::gdk::FileList>() {
                let mut added = false;
                for file in files.files() {
                    if let Some(path) = file.path() {
                        e.insert_picture(&path);
                        added = true;
                    }
                }
                added
            } else {
                false
            }
        });
        picture_drop.add_controller(drop);
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
            let dialog = gtk::FileChooserDialog::new(
                Some("Import presentation bundle"),
                Some(&parent),
                gtk::FileChooserAction::SelectFolder,
                &[
                    ("Cancel", ResponseType::Cancel),
                    ("Import", ResponseType::Accept),
                ],
            );
            let _ = dialog.set_current_folder(Some(&gtk::gio::File::for_path(&e.state.data_dir)));
            let path = if run_dialog(&dialog) == ResponseType::Accept {
                dialog.file().and_then(|file| file.path())
            } else {
                None
            };
            dialog.close();
            if let Some(path) = path {
                match bundle::import(&e.state, &path) {
                    Ok(deck) => e.replace_deck(deck),
                    Err(error) => e.status.set_text(&format!("Import failed: {error}")),
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
            dialog.content_area().append(&chooser);
            dialog.present();
            if run_dialog(&dialog) == ResponseType::Accept {
                if let Some(id) = chooser.active_id() {
                    if let Ok(deck) = read_deck(&e.state, &id) {
                        e.replace_deck(deck);
                    }
                }
            }
            dialog.close();
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
        let parent = window.clone();
        history_btn.connect_clicked(move |_| {
            if !resolve_unsaved(&e, &parent, "open version history") {
                return;
            }
            let id = e.deck.borrow().id.clone();
            let versions = match history::list(&e.state, &id) {
                Ok(value) => value,
                Err(error) => {
                    e.status.set_text(&format!("History failed: {error}"));
                    return;
                }
            };
            let Some(entries) = versions["history"].as_array() else {
                e.status.set_text("Could not read version history");
                return;
            };
            if entries.is_empty() {
                e.status.set_text("No earlier saved versions yet");
                return;
            }
            let current_revision = match deck_revision(&e.state, &id) {
                Ok(revision) => revision,
                Err(error) => {
                    e.status.set_text(&format!("History failed: {error}"));
                    return;
                }
            };
            let dialog = gtk::Dialog::with_buttons(
                Some("Version history"),
                Some(&parent),
                gtk::DialogFlags::MODAL,
                &[
                    ("Cancel", ResponseType::Cancel),
                    ("Restore", ResponseType::Accept),
                ],
            );
            let contents = gtk::Box::new(Orientation::Vertical, 12);
            contents.set_margin_top(18);
            contents.set_margin_bottom(18);
            contents.set_margin_start(18);
            contents.set_margin_end(18);
            let explanation = gtk::Label::new(Some(
                "Restore an earlier saved version. The current version will be kept in history.",
            ));
            explanation.set_wrap(true);
            explanation.set_xalign(0.0);
            contents.append(&explanation);
            let chooser = gtk::ComboBoxText::new();
            for entry in entries {
                let Some(hash) = entry["revision"].as_str() else {
                    continue;
                };
                let saved_at = entry["savedAt"].as_i64().unwrap_or_default();
                let date = gtk::glib::DateTime::from_unix_local(saved_at)
                    .ok()
                    .and_then(|value| value.format("%Y-%m-%d %H:%M").ok())
                    .map(|value| value.to_string())
                    .unwrap_or_else(|| saved_at.to_string());
                let count = entry["slideCount"].as_u64().unwrap_or_default();
                chooser.append(
                    Some(hash),
                    &format!("{date} · {count} slides · {}", &hash[..8]),
                );
            }
            chooser.set_active(Some(0));
            contents.append(&chooser);
            dialog.content_area().append(&contents);
            dialog.present();
            let selected = if run_dialog(&dialog) == ResponseType::Accept {
                chooser.active_id().map(|value| value.to_string())
            } else {
                None
            };
            dialog.close();
            if let Some(hash) = selected {
                match history::restore(&e.state, &id, &hash, &current_revision) {
                    Ok(deck) => e.replace_deck(deck),
                    Err(error) => e.status.set_text(&format!("Restore failed: {error}")),
                }
            }
        });
    }
    {
        let e = editor.clone();
        let parent = window.clone();
        changes_btn.connect_clicked(move |_| show_change_review(&e, &parent));
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
            let dialog = gtk::FileChooserDialog::new(
                Some("Export HyperFrames deck"),
                Some(&parent),
                gtk::FileChooserAction::Save,
                &[
                    ("Cancel", ResponseType::Cancel),
                    ("Export", ResponseType::Accept),
                ],
            );

            dialog.set_current_name("presentation.html");
            if run_dialog(&dialog) == ResponseType::Accept {
                if let Some(path) = dialog.file().and_then(|file| file.path()) {
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
            dialog.close();
        });
    }
    {
        let e = editor.clone();
        let parent = window.clone();
        export_audience_btn.connect_clicked(move |_| {
            let save_failed = e.persist().is_err();
            let dialog = gtk::FileChooserDialog::new(
                Some("Export audience deck without speaker notes"),
                Some(&parent),
                gtk::FileChooserAction::Save,
                &[
                    ("Cancel", ResponseType::Cancel),
                    ("Export", ResponseType::Accept),
                ],
            );

            dialog.set_current_name("audience.html");
            if run_dialog(&dialog) == ResponseType::Accept {
                if let Some(path) = dialog.file().and_then(|file| file.path()) {
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
            dialog.close();
        });
    }
    {
        let e = editor.clone();
        let app = app.clone();
        present_btn.connect_clicked(move |_| show_presentation(&e, &app));
    }
    let compact_mode = Rc::new(Cell::new(false));
    let edit_mode = Rc::new(Cell::new(false));
    let refresh_workspace = Rc::new({
        let compact_mode = compact_mode.clone();
        let edit_mode = edit_mode.clone();
        let compact_bar = compact_bar.clone();
        let center = center.clone();
        let inspector_shell = inspector_shell.clone();
        let canvas_btn = canvas_btn.clone();
        let edit_btn = edit_btn.clone();
        move || {
            update_workspace(
                compact_mode.get(),
                edit_mode.get(),
                &compact_bar,
                &center,
                &inspector_shell,
                &canvas_btn,
                &edit_btn,
            )
        }
    });
    {
        let edit_mode = edit_mode.clone();
        let refresh_workspace = refresh_workspace.clone();
        canvas_btn.connect_clicked(move |_| {
            edit_mode.set(false);
            refresh_workspace();
        });
    }
    {
        let edit_mode = edit_mode.clone();
        let refresh_workspace = refresh_workspace.clone();
        edit_btn.connect_clicked(move |_| {
            edit_mode.set(true);
            refresh_workspace();
        });
    }
    {
        let e = editor.clone();
        let app = app.clone();
        let new_btn = new_btn.clone();
        let open_btn = open_btn.clone();
        let save_btn = save_btn.clone();
        let overview_btn = overview_btn.clone();
        let window_for_keys = window.clone();
        let compact_mode = compact_mode.clone();
        let edit_mode = edit_mode.clone();
        let refresh_workspace = refresh_workspace.clone();
        let keys = gtk::EventControllerKey::new();
        keys.set_propagation_phase(gtk::PropagationPhase::Capture);
        keys.connect_key_pressed(move |_, key, _, state| {
            if state.contains(gtk::gdk::ModifierType::CONTROL_MASK) {
                let handled = match key.to_unicode().map(|value| value.to_ascii_lowercase()) {
                    Some('n') => {
                        new_btn.emit_clicked();
                        true
                    }
                    Some('o') => {
                        open_btn.emit_clicked();
                        true
                    }
                    Some('s') => {
                        save_btn.emit_clicked();
                        true
                    }
                    Some('g') => {
                        overview_btn.emit_clicked();
                        true
                    }
                    Some('v') if !focused_text_input(&window_for_keys) => {
                        let clipboard = gtk::gdk::Display::default().expect("display").clipboard();
                        let formats = clipboard.formats();
                        if formats.contains_type(gtk::gdk::Texture::static_type())
                            || ["image/png", "image/jpeg", "image/webp", "image/gif"]
                                .iter()
                                .any(|mime| formats.contain_mime_type(mime))
                        {
                            paste_picture(&e);
                            true
                        } else {
                            false
                        }
                    }
                    Some('e') if compact_mode.get() => {
                        edit_mode.set(!edit_mode.get());
                        refresh_workspace();
                        true
                    }
                    Some('p') => {
                        show_presentation(&e, &app);
                        true
                    }
                    _ => false,
                };
                if handled {
                    return gtk::glib::Propagation::Stop;
                }
            }
            gtk::glib::Propagation::Proceed
        });
        window.add_controller(keys);
    }
    {
        let e = editor.clone();
        window.connect_close_request(move |window| {
            if resolve_unsaved(&e, window, "close the editor") {
                gtk::glib::Propagation::Proceed
            } else {
                gtk::glib::Propagation::Stop
            }
        });
    }
    window.present();
    editor.deck_title.select_region(0, 0);
    editor.list.grab_focus();
    gtk::glib::timeout_add_local_once(std::time::Duration::from_millis(150), || {
        prepare_editor_on_hyprland();
    });
    if window.width() > 0 {
        compact_mode.set(window.width() < 1250);
        refresh_workspace();
    }
    {
        let window = window.downgrade();
        gtk::glib::timeout_add_local(std::time::Duration::from_millis(250), move || {
            let Some(window) = window.upgrade() else {
                return gtk::glib::ControlFlow::Break;
            };
            let compact = window.width() < 1250;
            if compact != compact_mode.get() {
                compact_mode.set(compact);
                refresh_workspace();
            }
            gtk::glib::ControlFlow::Continue
        });
    }
    gtk::glib::idle_add_local_once(move || thumbnails.start());
}

fn register_scheme(state: AppState) {
    let context = webkit::WebContext::default().expect("WebKit context");
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
    provider.load_from_data(include_str!("../assets/editor.css"));
    gtk::style_context_add_provider_for_display(
        &gtk::gdk::Display::default().expect("display"),
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
