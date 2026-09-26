use super::*;
use gtk::{Orientation, ResponseType};
use std::{
    cell::{Cell, RefCell},
    rc::Rc,
};

struct Editor {
    state: AppState,
    deck: RefCell<Deck>,
    selected: Cell<usize>,
    loading: Cell<bool>,
    list: gtk::ListBox,
    deck_title: gtk::Entry,
    theme: gtk::ComboBoxText,
    layout: gtk::ComboBoxText,
    eyebrow: gtk::Entry,
    headline: gtk::TextView,
    body: gtk::TextView,
    notes: gtk::TextView,
    preview: webkit2gtk::WebView,
    status: gtk::Label,
}

fn sample_deck() -> Deck {
    let id = format!("deck-{}", unique_id());
    Deck {
        id,
        title: "Untitled presentation".into(),
        theme: "midnight".into(),
        updated_at: now(),
        slides: vec![Slide {
            id: "slide-1".into(),
            layout: "title".into(),
            eyebrow: "A NEW PRESENTATION".into(),
            title: "Make your point beautifully.".into(),
            body: "A presentation made with HyperFrames Slides".into(),
            notes: "Add speaker notes here. Only you will see them in presenter mode.".into(),
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
                    decks.push(deck);
                }
            }
        }
    }
    decks.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
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
    fn persist(&self) {
        if self.loading.get() {
            return;
        }
        let mut deck = self.deck.borrow_mut();
        deck.title = self.deck_title.text().to_string();
        deck.theme = self
            .theme
            .active_id()
            .map(|s| s.to_string())
            .unwrap_or_else(|| "midnight".into());
        let index = self.selected.get();
        if let Some(slide) = deck.slides.get_mut(index) {
            slide.layout = self
                .layout
                .active_id()
                .map(|s| s.to_string())
                .unwrap_or_else(|| "title".into());
            slide.eyebrow = self.eyebrow.text().to_string();
            slide.title = text(&self.headline);
            slide.body = text(&self.body);
            slide.notes = text(&self.notes);
        }
        if deck.title.trim().is_empty() {
            deck.title = "Untitled presentation".into();
        }
        deck.updated_at = now();
        match write_deck(&self.state, &deck) {
            Ok(_) => self.status.set_text("Saved locally"),
            Err(error) => self.status.set_text(&format!("Save failed: {error}")),
        }
        drop(deck);
        self.refresh_list();
        self.refresh_preview();
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
        self.layout.set_active_id(Some(&slide.layout));
        self.eyebrow.set_text(&slide.eyebrow);
        set_text(&self.headline, &slide.title);
        set_text(&self.body, &slide.body);
        set_text(&self.notes, &slide.notes);
        self.loading.set(false);
        drop(deck);
        self.refresh_preview();
    }

    fn refresh_preview(&self) {
        let deck = self.deck.borrow();
        let slide = &deck.slides[self.selected.get()];
        let (bg, fg, muted, accent) = theme_colors(&deck.theme);
        let layout = if slide.layout == "quote" {
            "font-style:italic;font-weight:550"
        } else {
            ""
        };
        let html = format!(
            r#"<!doctype html><meta charset="utf-8"><style>*{{box-sizing:border-box}}html,body{{margin:0;width:100%;height:100%;font-family:system-ui,sans-serif;background:{};color:{}}}main{{height:100%;display:flex;flex-direction:column;justify-content:center;padding:8%}}.rule{{width:7%;height:1%;background:{};border-radius:10px;margin-bottom:4%}}.eyebrow{{font-size:2.2vw;font-weight:800;letter-spacing:.18em;color:{}}}h1{{font-size:5.7vw;line-height:1.05;letter-spacing:-.05em;white-space:pre-wrap;margin:2.5% 0 3%;{}}}p{{font-size:3vw;color:{};white-space:pre-wrap;margin:0;line-height:1.25}}.number{{position:absolute;right:7%;bottom:6%;font-size:1.7vw;color:{}}}</style><main><div class="rule"></div><div class="eyebrow">{}</div><h1>{}</h1><p>{}</p></main><div class="number">{:02} / {:02}</div>"#,
            bg,
            fg,
            accent,
            accent,
            layout,
            muted,
            muted,
            escape_html(&slide.eyebrow),
            escape_html(&slide.title),
            escape_html(&slide.body),
            self.selected.get() + 1,
            deck.slides.len()
        );
        self.preview.load_html(&html, None);
    }

    fn select(&self, index: usize) {
        if index >= self.deck.borrow().slides.len() {
            return;
        }
        self.selected.set(index);
        self.refresh_fields();
    }

    fn replace_deck(&self, deck: Deck) {
        *self.deck.borrow_mut() = deck;
        self.selected.set(0);
        self.refresh_list();
        self.refresh_fields();
        self.status.set_text("Saved locally");
    }

    fn add_slide(&self, duplicate: bool) {
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
            }
        };
        deck.slides.insert(index, slide);
        drop(deck);
        self.selected.set(index);
        self.refresh_list();
        self.refresh_fields();
        self.persist();
    }

    fn move_slide(&self, delta: isize) {
        let index = self.selected.get();
        let next = index as isize + delta;
        if next < 0 || next as usize >= self.deck.borrow().slides.len() {
            return;
        }
        self.deck.borrow_mut().slides.swap(index, next as usize);
        self.selected.set(next as usize);
        self.refresh_list();
        self.refresh_fields();
        self.persist();
    }

    fn delete_slide(&self) {
        if self.deck.borrow().slides.len() <= 1 {
            self.status.set_text("Keep at least one slide");
            return;
        }
        self.deck.borrow_mut().slides.remove(self.selected.get());
        self.selected
            .set(self.selected.get().min(self.deck.borrow().slides.len() - 1));
        self.refresh_list();
        self.refresh_fields();
        self.persist();
    }
}

fn button(label: &str) -> gtk::Button {
    gtk::Button::with_label(label)
}

fn open_presenter(app: &gtk::Application, url: &str) {
    let window = gtk::ApplicationWindow::new(app);
    window.set_title("HyperFrames Presenter");
    window.set_default_size(900, 700);
    let view = webkit2gtk::WebView::new();
    configure_popup(&view, app);
    view.load_uri(url);
    let shell = gtk::Box::new(Orientation::Vertical, 0);
    let toolbar = gtk::Box::new(Orientation::Horizontal, 8);
    toolbar.style_context().add_class("toolbar");
    let label = gtk::Label::new(Some("Presenter"));
    let audience_button = button("Audience");
    audience_button.set_tooltip_text(Some("Open a separate audience window to share in Zoom"));
    audience_button
        .style_context()
        .add_class("suggested-action");
    toolbar.pack_start(&audience_button, false, false, 8);
    toolbar.pack_start(&label, true, true, 8);
    let accelerators = gtk::AccelGroup::new();
    window.add_accel_group(&accelerators);
    audience_button.add_accelerator(
        "clicked",
        &accelerators,
        *gtk::gdk::keys::constants::p,
        gtk::gdk::ModifierType::CONTROL_MASK,
        gtk::AccelFlags::VISIBLE,
    );
    shell.pack_start(&toolbar, false, false, 0);
    shell.pack_start(&view, true, true, 0);
    window.add(&shell);
    let parent_view = view.clone();
    let audience_url = format!("{}?mode=audience", url);
    let audience_app = app.clone();
    let audience_window: Rc<RefCell<Option<gtk::ApplicationWindow>>> = Rc::new(RefCell::new(None));
    let audience_slot = audience_window.clone();
    let open_audience = Rc::new(move || {
        if let Some(open) = audience_slot.borrow().as_ref() {
            open.present();
            return;
        }
        let audience = webkit2gtk::WebView::with_related_view(&parent_view);
        let audience_shell = gtk::ApplicationWindow::new(&audience_app);
        audience_shell.set_title("HyperFrames Audience · share this window in Zoom");
        audience_shell.set_default_size(1280, 720);
        audience_shell.add(&audience);
        audience.load_uri(&audience_url);
        audience_shell.show_all();
        let on_close = audience_slot.clone();
        audience_shell.connect_destroy(move |_| *on_close.borrow_mut() = None);
        *audience_slot.borrow_mut() = Some(audience_shell);
        parent_view.run_javascript(
            "const ss=document.querySelector('hyperframes-slideshow');if(ss){ss.setAttribute('data-hf-presenting','true');ss.postCurrentPresenterPositionBurst();ss.presenterStartMs=Date.now();if(ss.presenterInterval===null)ss.presenterInterval=setInterval(()=>ss.updateElapsed(),1000);ss.render()}",
            None::<&gtk::gio::Cancellable>,
            |_| {},
        );
    });
    {
        let open = open_audience.clone();
        audience_button.connect_clicked(move |_| open());
    }
    {
        let open = open_audience.clone();
        window.connect_key_press_event(move |_, event| {
            if event.state().is_empty()
                && event
                    .keyval()
                    .to_unicode()
                    .is_some_and(|c| c.eq_ignore_ascii_case(&'p'))
            {
                open();
                return gtk::glib::Propagation::Stop;
            }
            gtk::glib::Propagation::Proceed
        });
    }
    window.show_all();
    audience_button.grab_focus();
}

fn show_presentation(editor: &Editor, app: &gtk::Application) {
    editor.persist();
    let id = editor.deck.borrow().id.clone();
    match present(&editor.state, &id) {
        Ok(url) => {
            editor
                .status
                .set_text("Presenter opened · use Open audience window for Zoom");
            open_presenter(app, &url);
        }
        Err(msg) => editor
            .status
            .set_text(&format!("Presentation failed: {msg}")),
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
    let open_btn = button("Open");
    let save_btn = button("Save");
    let export_btn = button("Export HTML");
    let present_btn = button("Present");
    for b in [&new_btn, &open_btn, &save_btn, &export_btn, &present_btn] {
        header.pack_start(b, false, false, 0);
    }
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
    let preview = webkit2gtk::WebView::new();
    preview.set_size_request(480, 270);
    let preview_frame = gtk::Frame::new(None);
    preview_frame.add(&preview);
    let aspect = gtk::AspectFrame::new(None, 0.5, 0.5, 16.0 / 9.0, false);
    aspect.add(&preview_frame);
    center.pack_start(&aspect, true, true, 0);
    let help = gtk::Label::new(Some("Present opens a native WebKitGTK window. Click Audience there to open the window Zoom should share."));
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
    let notes = text_field(&fields, "Speaker notes", 8);
    fields.pack_start(&editor_label("Theme"), false, false, 0);
    let theme = gtk::ComboBoxText::new();
    for (id, label) in [
        ("midnight", "Midnight"),
        ("paper", "Paper"),
        ("cobalt", "Cobalt"),
        ("sunset", "Sunset"),
    ] {
        theme.append(Some(id), label);
    }
    fields.pack_start(&theme, false, false, 0);
    fields_scroll.add(&fields);
    content.pack2(&fields_scroll, false, false);
    main.pack2(&content, true, false);
    root.pack_start(&main, true, true, 0);
    let status = gtk::Label::new(Some("Saved locally"));
    status.set_xalign(0.0);
    status.style_context().add_class("status");
    root.pack_start(&status, false, false, 0);
    window.add(&root);
    let editor = Rc::new(Editor {
        state,
        deck: RefCell::new(initial),
        selected: Cell::new(0),
        loading: Cell::new(false),
        list,
        deck_title,
        theme,
        layout,
        eyebrow,
        headline,
        body,
        notes,
        preview,
        status,
    });
    editor.refresh_list();
    editor.refresh_fields();

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
        widget.connect_changed(move |_| e.persist());
    }
    {
        let e = editor.clone();
        let widget = e.theme.clone();
        widget.connect_changed(move |_| e.persist());
    }
    {
        let e = editor.clone();
        let widget = e.layout.clone();
        widget.connect_changed(move |_| e.persist());
    }
    {
        let e = editor.clone();
        let widget = e.eyebrow.clone();
        widget.connect_changed(move |_| e.persist());
    }
    for view in [&editor.headline, &editor.body, &editor.notes] {
        let e = editor.clone();
        view.buffer()
            .expect("buffer")
            .connect_changed(move |_| e.persist());
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
        new_btn.connect_clicked(move |_| {
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
        open_btn.connect_clicked(move |_| {
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
            dialog.close();
        });
    }
    {
        let e = editor.clone();
        save_btn.connect_clicked(move |_| e.persist());
    }
    {
        let e = editor.clone();
        let parent = window.clone();
        export_btn.connect_clicked(move |_| {
            e.persist();
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
                        .and_then(|html| fs::write(&path, html).map_err(internal))
                    {
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
    window.show_all();
}

pub fn launch_gui(state: AppState) {
    gtk::init().expect("GTK display");
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
    let shutdown_state = state.clone();
    app.connect_shutdown(move |_| {
        if let Ok(mut running) = shutdown_state.presentations.lock() {
            for presentation in running.values_mut() {
                let _ = presentation.child.kill();
            }
        }
    });
    app.connect_activate(move |app| build(app, state.clone()));
    app.run();
}
