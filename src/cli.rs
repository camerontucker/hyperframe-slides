use super::*;
use std::io::{self, Read, Write};
use std::os::unix::net::UnixStream;
use std::time::Duration;

thread_local! {
    static DEFER_OUTPUT: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    static PENDING_OUTPUT: std::cell::RefCell<Option<String>> = const { std::cell::RefCell::new(None) };
}

fn id() -> String {
    format!(
        "slide-{}",
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
    )
}

fn new_deck(title: &str) -> Deck {
    Deck {
        schema_version: 1,
        id: format!(
            "deck-{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ),
        title: title.into(),
        theme: "midnight".into(),
        template: SlideTemplate::default(),
        slides: vec![Slide {
            id: id(),
            layout: "title".into(),
            eyebrow: String::new(),
            title: "New slide".into(),
            body: String::new(),
            notes: String::new(),
            animation: default_animation(),
            images: Vec::new(),
        }],
        updated_at: now(),
    }
}

fn input(path: &str) -> Result<Vec<u8>, ApiError> {
    if path == "-" {
        let mut bytes = Vec::new();
        io::stdin().read_to_end(&mut bytes).map_err(internal)?;
        Ok(bytes)
    } else {
        fs::read(path).map_err(internal)
    }
}

fn output(value: impl Serialize) -> Result<(), ApiError> {
    let json = serde_json::to_string_pretty(&value).map_err(internal)?;
    if DEFER_OUTPUT.with(|defer| defer.get()) {
        PENDING_OUTPUT.with(|pending| *pending.borrow_mut() = Some(json));
    } else {
        println!("{json}");
    }
    Ok(())
}

fn control(state: &AppState, token: &str, command: &str) -> Result<serde_json::Value, ApiError> {
    let path = control_path(state, token)?;
    let mut stream =
        UnixStream::connect(path).map_err(|_| "Presentation session is not running")?;
    stream
        .set_read_timeout(Some(Duration::from_secs(3)))
        .map_err(internal)?;
    stream.write_all(command.as_bytes()).map_err(internal)?;
    stream
        .shutdown(std::net::Shutdown::Write)
        .map_err(internal)?;
    let mut reply = Vec::new();
    stream.read_to_end(&mut reply).map_err(internal)?;
    let value: serde_json::Value = serde_json::from_slice(&reply).map_err(internal)?;
    if let Some(error) = value.get("error").and_then(|value| value.as_str()) {
        return Err(error.into());
    }
    Ok(value)
}

fn usage() {
    println!(
        "HyperFrames Slides CLI (JSON output; errors on stderr as JSON)\n\
Usage:\n\
  hyperframe-slides                         Open the GTK editor\n\
  hyperframe-slides schema                  Print deck template and command schema\n\
  hyperframe-slides deck list               List local decks\n\
  hyperframe-slides deck new [TITLE]        Create a deck\n\
  hyperframe-slides deck get ID             Print a complete deck\n\
  hyperframe-slides deck snapshot ID        Print a deck and its matching revision\n\
  hyperframe-slides deck put FILE|- [--if-revision HASH]  Create or replace a deck\n\
  hyperframe-slides deck revision ID        Print the current content revision\n\
  hyperframe-slides deck validate ID        Validate for presentation\n\
  hyperframe-slides deck export ID DIR      Export HyperFrames index.html\n\
  hyperframe-slides deck export-audience ID DIR  Export without speaker notes\n\
  hyperframe-slides deck present ID         Open native presenter window\n\
  hyperframe-slides deck present ID --audience  Open presenter and Zoom audience windows\n\
  hyperframe-slides present list          List live local presentations\n\
  hyperframe-slides present status SESSION\n\
  hyperframe-slides present gpu SESSION\n\
  hyperframe-slides present inspect SESSION\n\
  hyperframe-slides present inspect-audience SESSION\n\
  hyperframe-slides present next SESSION\n\
  hyperframe-slides present prev SESSION\n\
  hyperframe-slides present goto SESSION POSITION  (1-based)\n\
  hyperframe-slides present audience SESSION\n\
  hyperframe-slides present audience-close SESSION\n\
  hyperframe-slides present close SESSION\n\
  hyperframe-slides slide add ID [FILE|-]   Insert a slide after the last slide\n\
  hyperframe-slides slide duplicate ID SLIDE_ID\n\
  hyperframe-slides slide set ID SLIDE_ID FILE|- --if-revision HASH\n\
  hyperframe-slides slide move ID SLIDE_ID POSITION  (1-based)\n\
  hyperframe-slides slide delete ID SLIDE_ID\n\
  hyperframe-slides slide animation ID SLIDE_ID none|fade|rise|zoom\n\
  hyperframe-slides image add ID SLIDE_ID FILE [ALT]\n\
  hyperframe-slides image remove ID SLIDE_ID IMAGE_ID\n\
  hyperframe-slides image position ID SLIDE_ID IMAGE_ID X Y WIDTH HEIGHT\n\
  hyperframe-slides template header ID TEXT\n\
  hyperframe-slides template footer ID TEXT\n\
  hyperframe-slides template logo ID FILE\n\
  hyperframe-slides template logo-clear ID\n\
  hyperframe-slides --export ID DIR         Legacy export alias"
    );
}

fn save(state: &AppState, deck: &mut Deck) -> Result<(), ApiError> {
    deck.updated_at = now();
    write_deck_unlocked(state, deck).map(|_| ())
}

fn slide_index(deck: &Deck, id: &str) -> Result<usize, ApiError> {
    deck.slides
        .iter()
        .position(|slide| slide.id == id)
        .ok_or_else(|| "Slide not found".into())
}

pub(super) fn run(state: &AppState, args: &[String]) -> Result<(), ApiError> {
    let words: Vec<&str> = args.iter().map(String::as_str).collect();
    // Read agent-provided files and stdin before locking the active document.
    let document_input = match words.as_slice() {
        ["deck", "put", file, ..]
        | ["slide", "add", _, file]
        | ["slide", "set", _, _, file, ..] => Some(input(file)?),
        _ => None,
    };
    let mut image_input = match words.as_slice() {
        ["image", "add", _, _, file, ..] => {
            Some(image_data_uri(std::path::Path::new(file), 8_000_000)?)
        }
        ["template", "logo", _, file] => {
            Some(image_data_uri(std::path::Path::new(file), 4_000_000)?)
        }
        _ => None,
    };
    // Hold one lock across each CLI read, mutation, validation, and replacement.
    // Scoped commands apply to the latest disk version while holding it.
    let mutates = matches!(words.first(), Some(&"slide" | &"image" | &"template"))
        || matches!(words.as_slice(), ["deck", "new", ..] | ["deck", "put", ..]);
    let write_lock = if mutates {
        Some(lock_deck_writes(state)?)
    } else {
        None
    };
    DEFER_OUTPUT.with(|defer| defer.set(mutates));
    let result = (|| match words.as_slice() {
        ["help"] | ["--help"] | ["-h"] => {
            usage();
            Ok(())
        }
        ["schema"] => output(serde_json::json!({
            "format": "HyperFrames Slides deck JSON v1",
            "commands": ["deck list", "deck new [TITLE]", "deck get ID", "deck snapshot ID", "deck revision ID", "deck put FILE|- [--if-revision HASH]", "deck validate ID", "deck export ID DIR", "deck export-audience ID DIR", "deck present ID [--audience]", "present list", "present status SESSION", "present gpu SESSION", "present inspect SESSION", "present inspect-audience SESSION", "present next SESSION", "present prev SESSION", "present goto SESSION POSITION", "present audience SESSION", "present audience-close SESSION", "present close SESSION", "slide add ID [FILE|-]", "slide duplicate ID SLIDE_ID", "slide set ID SLIDE_ID FILE|- --if-revision HASH", "slide move ID SLIDE_ID POSITION", "slide delete ID SLIDE_ID", "slide animation ID SLIDE_ID MODE", "image add ID SLIDE_ID FILE [ALT]", "image remove ID SLIDE_ID IMAGE_ID", "image position ID SLIDE_ID IMAGE_ID X Y WIDTH HEIGHT", "template header ID TEXT", "template footer ID TEXT", "template logo ID FILE", "template logo-clear ID"],
            "deckTemplate": new_deck("Untitled presentation"),
            "notes": "Use deck snapshot to read a deck and its SHA-256 revision atomically. Existing decks and slide set require that revision when replacing content. Scoped commands update the latest deck under a document lock. Slide body/headline accept Markdown, animation is none|fade|rise|zoom, and image coordinates are percentages. Use image add or template logo to embed local pictures. Use - to read JSON from stdin. IDs use ASCII letters, digits, and hyphens. deck put stores safe drafts; deck validate checks presentation limits."
        })),
        ["deck", "list"] => {
            let mut decks = Vec::new();
            for entry in fs::read_dir(state.data_dir.join("decks"))
                .map_err(internal)?
                .flatten()
            {
                if entry.path().extension().and_then(|value| value.to_str()) != Some("json") {
                    continue;
                }
                if let Some(id) = entry.path().file_stem().and_then(|value| value.to_str()) {
                    if let Ok(deck) = read_deck(state, id) {
                        decks.push(deck);
                    }
                }
            }
            decks.sort_by_key(|deck| std::cmp::Reverse(deck.updated_at));
            output(decks)
        }
        ["deck", "new"] | ["deck", "new", _] => {
            let mut deck = new_deck(words.get(2).copied().unwrap_or("Untitled presentation"));
            save(state, &mut deck)?;
            output(deck)
        }
        ["deck", "get", id] => output(read_deck(state, id)?),
        ["deck", "snapshot", id] => {
            let (deck, revision) = read_deck_snapshot(state, id)?;
            output(serde_json::json!({"revision": revision, "deck": deck}))
        }
        ["deck", "revision", id] => {
            output(serde_json::json!({"id": id, "revision": deck_revision(state, id)?}))
        }
        ["deck", "put", file] | ["deck", "put", file, "--if-revision", _] => {
            let _ = file;
            let mut deck: Deck =
                serde_json::from_slice(document_input.as_deref().unwrap()).map_err(internal)?;
            let path = deck_path(state, &deck.id)?;
            let expected = words.get(4).copied();
            match fs::read(path) {
                Ok(bytes) if expected != Some(revision(&bytes).as_str()) => {
                    return Err("Deck changed or revision missing; run deck revision ID and retry with --if-revision HASH".into());
                }
                Err(error) if error.kind() == io::ErrorKind::NotFound && expected.is_none() => {}
                Err(error) if error.kind() == io::ErrorKind::NotFound => {
                    return Err("Deck no longer exists".into());
                }
                Err(error) => return Err(internal(error)),
                _ => {}
            }
            save(state, &mut deck)?;
            output(deck)
        }
        ["deck", "validate", id] => {
            let deck = read_deck(state, id)?;
            validate_deck(&deck)?;
            output(serde_json::json!({"id": id, "valid": true, "slideCount": deck.slides.len()}))
        }
        ["deck", "export", id, directory] | ["--export", id, directory] => {
            let deck = read_deck(state, id)?;
            let html = export_html(&deck)?;
            let path = PathBuf::from(directory).join("index.html");
            fs::create_dir_all(directory).map_err(internal)?;
            write_private(&path, html.as_bytes())?;
            output(serde_json::json!({"id": id, "path": path}))
        }
        ["deck", "export-audience", id, directory] => {
            let deck = read_deck(state, id)?;
            let html = export_html_with_notes(&deck, false)?;
            let path = PathBuf::from(directory).join("index.html");
            fs::create_dir_all(directory).map_err(internal)?;
            write_private(&path, html.as_bytes())?;
            output(serde_json::json!({"id": id, "path": path, "speakerNotes": false}))
        }
        ["deck", "present", id] | ["deck", "present", id, "--audience"] => {
            let with_audience = words.len() == 4;
            let url = present(state, id)?;
            let session = url
                .strip_prefix("hyperframe://app/")
                .and_then(|value| value.split('/').next())
                .ok_or("Invalid presentation URI")?;
            output(
                serde_json::json!({"id": id, "session": session, "uri": url, "audience": with_audience, "status": "opening"}),
            )?;
            io::stdout().flush().map_err(internal)?;
            native::launch_presenter(state.clone(), url, with_audience);
            Ok(())
        }
        ["present", "list"] => {
            let mut sessions = Vec::new();
            if let Ok(entries) = fs::read_dir(state.data_dir.join("control")) {
                for entry in entries.flatten() {
                    if let Some(token) = entry.path().file_stem().and_then(|value| value.to_str()) {
                        if let Ok(status) = control(state, token, "status") {
                            sessions.push(status);
                        }
                    }
                }
            }
            sessions.sort_by_key(|item| {
                item.get("session")
                    .and_then(|value| value.as_str())
                    .unwrap_or_default()
                    .to_owned()
            });
            output(sessions)
        }
        ["present", "status" | "gpu" | "inspect" | "inspect-audience" | "next" | "prev" | "audience"
        | "audience-close" | "close", token] => output(control(state, token, words[1])?),
        ["present", "goto", token, position] => {
            let position: usize = position
                .parse()
                .map_err(|_| "Position must be a 1-based integer")?;
            if position == 0 {
                return Err("Position must be a 1-based integer".into());
            }
            output(control(state, token, &format!("goto {position}"))?)
        }
        ["slide", "add", deck_id] | ["slide", "add", deck_id, _] => {
            let mut deck = read_deck(state, deck_id)?;
            let slide = if document_input.is_some() {
                serde_json::from_slice(document_input.as_deref().unwrap()).map_err(internal)?
            } else {
                Slide {
                    id: id(),
                    layout: "statement".into(),
                    eyebrow: String::new(),
                    title: "New slide".into(),
                    body: String::new(),
                    notes: String::new(),
                    animation: default_animation(),
                    images: Vec::new(),
                }
            };
            deck.slides.push(slide.clone());
            save(state, &mut deck)?;
            output(slide)
        }
        ["slide", "duplicate", deck_id, slide_id] => {
            let mut deck = read_deck(state, deck_id)?;
            let index = slide_index(&deck, slide_id)?;
            let mut slide = deck.slides[index].clone();
            slide.id = id();
            deck.slides.insert(index + 1, slide.clone());
            save(state, &mut deck)?;
            output(slide)
        }
        ["slide", "set", deck_id, slide_id, file, "--if-revision", expected] => {
            let _ = file;
            if deck_revision(state, deck_id)? != *expected {
                return Err(
                    "Deck changed; fetch the latest slide and revision before replacing it".into(),
                );
            }
            let mut deck = read_deck(state, deck_id)?;
            let index = slide_index(&deck, slide_id)?;
            let slide: Slide =
                serde_json::from_slice(document_input.as_deref().unwrap()).map_err(internal)?;
            if slide.id != *slide_id {
                return Err("Replacement slide ID must match".into());
            }
            deck.slides[index] = slide.clone();
            save(state, &mut deck)?;
            output(slide)
        }
        ["slide", "move", deck_id, slide_id, position] => {
            let mut deck = read_deck(state, deck_id)?;
            let index = slide_index(&deck, slide_id)?;
            let position: usize = position
                .parse()
                .map_err(|_| "Position must be a 1-based integer")?;
            if position == 0 || position > deck.slides.len() {
                return Err("Position is out of range".into());
            }
            let slide = deck.slides.remove(index);
            deck.slides.insert(position - 1, slide);
            save(state, &mut deck)?;
            output(deck)
        }
        ["slide", "delete", deck_id, slide_id] => {
            let mut deck = read_deck(state, deck_id)?;
            if deck.slides.len() == 1 {
                return Err("Keep at least one slide".into());
            }
            let index = slide_index(&deck, slide_id)?;
            deck.slides.remove(index);
            save(state, &mut deck)?;
            output(deck)
        }
        ["slide", "animation", deck_id, slide_id, mode] => {
            if !["none", "fade", "rise", "zoom"].contains(mode) {
                return Err("Invalid slide animation".into());
            }
            let mut deck = read_deck(state, deck_id)?;
            let index = slide_index(&deck, slide_id)?;
            deck.slides[index].animation = (*mode).into();
            save(state, &mut deck)?;
            output(deck.slides[index].clone())
        }
        ["image", "add", deck_id, slide_id, file]
        | ["image", "add", deck_id, slide_id, file, _] => {
            let mut deck = read_deck(state, deck_id)?;
            let index = slide_index(&deck, slide_id)?;
            let alt = words.get(5).copied().unwrap_or("Picture");
            if alt.len() > 500 {
                return Err("Picture description is too long".into());
            }
            let _ = file;
            let image =
                add_image_uri_to_slide(&mut deck.slides[index], image_input.take().unwrap(), alt)?;
            save(state, &mut deck)?;
            output(image)
        }
        ["image", "remove", deck_id, slide_id, image_id] => {
            let mut deck = read_deck(state, deck_id)?;
            let index = slide_index(&deck, slide_id)?;
            let before = deck.slides[index].images.len();
            deck.slides[index]
                .images
                .retain(|image| image.id != *image_id);
            if deck.slides[index].images.len() == before {
                return Err("Picture not found".into());
            }
            save(state, &mut deck)?;
            output(deck)
        }
        ["image", "position", deck_id, slide_id, image_id, x, y, width, height] => {
            let parse = |value: &str| {
                value
                    .parse::<f32>()
                    .map_err(|_| "Picture coordinates must be numbers".to_string())
            };
            let (x, y, width, height) = (parse(x)?, parse(y)?, parse(width)?, parse(height)?);
            if ![x, y, width, height].iter().all(|value| value.is_finite())
                || x < 0.0
                || y < 0.0
                || width <= 0.0
                || height <= 0.0
                || x + width > 100.0
                || y + height > 100.0
            {
                return Err("Picture position must fit inside the slide".into());
            }
            let mut deck = read_deck(state, deck_id)?;
            let index = slide_index(&deck, slide_id)?;
            let image = deck.slides[index]
                .images
                .iter_mut()
                .find(|image| image.id == *image_id)
                .ok_or("Picture not found")?;
            image.x = x;
            image.y = y;
            image.width = width;
            image.height = height;
            let image = image.clone();
            save(state, &mut deck)?;
            output(image)
        }
        ["template", "header", deck_id, value] | ["template", "footer", deck_id, value] => {
            if value.len() > 500 {
                return Err("Template text is too long".into());
            }
            let mut deck = read_deck(state, deck_id)?;
            if words[1] == "header" {
                deck.template.header = (*value).into();
            } else {
                deck.template.footer = (*value).into();
            }
            save(state, &mut deck)?;
            output(deck.template)
        }
        ["template", "logo", deck_id, file] => {
            let _ = file;
            let mut deck = read_deck(state, deck_id)?;
            deck.template.logo = image_input.take();
            save(state, &mut deck)?;
            output(deck)
        }
        ["template", "logo-clear", deck_id] => {
            let mut deck = read_deck(state, deck_id)?;
            deck.template.logo = None;
            save(state, &mut deck)?;
            output(deck)
        }
        _ => Err("Unknown command or incorrect arguments; run hyperframe-slides --help".into()),
    })();
    drop(write_lock);
    DEFER_OUTPUT.with(|defer| defer.set(false));
    let pending = PENDING_OUTPUT.with(|pending| pending.borrow_mut().take());
    if result.is_ok() {
        if let Some(json) = pending {
            println!("{json}");
        }
    }
    result
}
