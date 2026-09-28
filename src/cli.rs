use super::*;

const LEGACY_SKILL_HASH: &str = "4a39f7d3431ea883718ebe4d85f23df6c4bd961c018ac820538d9de818165c21";

fn install_skill(directory: &Path) -> Result<PathBuf, ApiError> {
    fs::create_dir_all(directory).map_err(internal)?;
    let path = directory.join("SKILL.md");
    let marker = directory.join(".hyperframe-slides-managed");
    let content = include_bytes!("../assets/agent-skill.md");
    let current_hash = format!("{:x}", Sha256::digest(content));
    let managed_hash = match fs::symlink_metadata(&marker) {
        Ok(metadata) if metadata.is_file() && !metadata.file_type().is_symlink() => {
            Some(fs::read_to_string(&marker).map_err(internal)?)
        }
        Ok(_) => return Err("Skill ownership marker must be a regular file".into()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => None,
        Err(error) => return Err(internal(error)),
    };
    let previous = match fs::symlink_metadata(&path) {
        Ok(metadata) if metadata.is_file() && !metadata.file_type().is_symlink() => {
            Some(fs::read(&path).map_err(internal)?)
        }
        Ok(_) => return Err("Skill path must be a regular file".into()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => None,
        Err(error) => return Err(internal(error)),
    };
    if let Some(previous) = &previous {
        let previous_hash = format!("{:x}", Sha256::digest(previous));
        if previous_hash != current_hash
            && previous_hash != LEGACY_SKILL_HASH
            && managed_hash.as_deref() != Some(previous_hash.as_str())
        {
            return Err("Skill file was changed outside HyperFrames Slides; keep that copy or choose another directory".into());
        }
    }
    if previous.as_deref() != Some(content) {
        write_private(&path, content)?;
    }
    write_private(&marker, current_hash.as_bytes())?;
    Ok(path)
}
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
            outline_group: String::new(),
            title: "New slide".into(),
            body: String::new(),
            notes: String::new(),
            reveal_options: Vec::new(),
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
  hyperframe-slides schema json             Print JSON Schema for deck files\n\
  hyperframe-slides schema source           Print JSON Schema for file-backed source manifests\n\
  hyperframe-slides deck list               List local decks\n\
  hyperframe-slides deck new [TITLE]        Create a deck\n\
  hyperframe-slides deck new TITLE --from ID  Create from a template deck\n\
  hyperframe-slides deck get ID             Print a complete deck\n\
  hyperframe-slides deck import-bundle DIR   Import a folder with presentation.json and assets\n\
  hyperframe-slides deck apply-bundle ID DIR --if-revision HASH  Update a deck from its bundle\n\
  hyperframe-slides deck bundle ID DIR      Export an editable presentation folder\n\
  hyperframe-slides deck migrate ID         Move a legacy deck to file-backed media\n\
  hyperframe-slides deck snapshot ID        Print a deck and its matching revision\n\
  hyperframe-slides deck source ID          Read the file-backed manifest without embedded media\n\
  hyperframe-slides deck put FILE|- [--if-revision HASH]  Create or replace a deck\n\
  hyperframe-slides deck put-source FILE --if-revision HASH  Safely replace a file-backed manifest\n\
  hyperframe-slides deck revision ID        Print the current content revision\n\
  hyperframe-slides deck history ID         List prior saved deck revisions\n\
  hyperframe-slides deck restore ID HISTORY_HASH --if-revision CURRENT_HASH\n\
  hyperframe-slides deck diff ID HISTORY_HASH [DIR]  Compare with history; optionally render images\n\
  hyperframe-slides deck revert-slide ID HISTORY_HASH SLIDE_ID --if-revision CURRENT_HASH\n\
  hyperframe-slides deck validate ID        Validate for presentation\n\
  hyperframe-slides deck review ID DIR      Render slide PNGs, contact sheet, and JSON findings\n\
  hyperframe-slides deck render ID SLIDE_ID DIR  Render and inspect one slide\n\
  hyperframe-slides deck export ID DIR      Export HyperFrames index.html\n\
  hyperframe-slides deck export-audience ID DIR  Export without speaker notes\n\
  hyperframe-slides deck present ID         Open the Zoom audience window\n\
  hyperframe-slides deck present ID --audience  Compatibility alias\n\
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
  hyperframe-slides image background ID SLIDE_ID IMAGE_ID on|off\n\
  hyperframe-slides template header ID TEXT\n\
  hyperframe-slides template footer ID TEXT\n\
  hyperframe-slides template outline ID on|off\n\
  hyperframe-slides template theme ID midnight|paper|cobalt|sunset|regent\n\
  hyperframe-slides template logo ID FILE\n\
  hyperframe-slides template logo-clear ID\n\
  hyperframe-slides template font ID heading|body FILE.woff2\n\
  hyperframe-slides template font-clear ID heading|body\n\
  hyperframe-slides skill [install [DIR]]  Show or install the agent authoring skill\n\
  hyperframe-slides help authoring         Show the agent authoring guide\n\
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
        | ["deck", "put-source", file, ..]
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
    let mut font_input = match words.as_slice() {
        ["template", "font", _, _, file] => Some(font_data_uri(std::path::Path::new(file))?),
        _ => None,
    };
    // Hold one lock across each CLI read, mutation, validation, and replacement.
    // Scoped commands apply to the latest disk version while holding it.
    let mutates = matches!(words.first(), Some(&"slide" | &"image" | &"template"))
        || matches!(
            words.as_slice(),
            ["deck", "new", ..] | ["deck", "put", ..] | ["deck", "put-source", ..]
        );
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
        ["help", "authoring"] => {
            print!("{}", include_str!("../docs/agent-authoring.md"));
            Ok(())
        }
        ["skill"] => {
            print!("{}", include_str!("../assets/agent-skill.md"));
            Ok(())
        }
        ["skill", "install"] | ["skill", "install", _] => {
            let directory = if let Some(path) = words.get(2) {
                PathBuf::from(path)
            } else {
                PathBuf::from(std::env::var_os("HOME").ok_or("HOME is not set")?)
                    .join(".agents/skills/hyperframe-slides")
            };
            let path = install_skill(&directory)?;
            output(serde_json::json!({"path": path, "installed": true}))
        }
        ["schema", "json"] => output(
            serde_json::from_str::<serde_json::Value>(include_str!("../schema/deck.schema.json"))
                .map_err(internal)?,
        ),
        ["schema", "source"] => output(
            serde_json::from_str::<serde_json::Value>(include_str!(
                "../schema/storage.schema.json"
            ))
            .map_err(internal)?,
        ),
        ["schema"] => {
            let mut description = serde_json::json!({
                "format": "HyperFrames Slides embedded JSON v1 and file-backed local source v2",
                "commands": ["schema json", "deck list", "deck new [TITLE]", "deck new TITLE --from ID", "deck get ID", "deck import-bundle DIR", "deck apply-bundle ID DIR --if-revision HASH", "deck bundle ID DIR", "deck snapshot ID", "deck revision ID", "deck put FILE|- [--if-revision HASH]", "deck validate ID", "deck review ID DIR", "deck render ID SLIDE_ID DIR", "deck diff ID HISTORY_HASH [DIR]", "deck revert-slide ID HISTORY_HASH SLIDE_ID --if-revision CURRENT_HASH", "deck export ID DIR", "deck export-audience ID DIR", "deck present ID [--audience]", "present list", "present status SESSION", "present gpu SESSION", "present inspect SESSION", "present inspect-audience SESSION", "present next SESSION", "present prev SESSION", "present goto SESSION POSITION", "present audience SESSION", "present audience-close SESSION", "present close SESSION", "slide add ID [FILE|-]", "slide duplicate ID SLIDE_ID", "slide set ID SLIDE_ID FILE|- --if-revision HASH", "slide move ID SLIDE_ID POSITION", "slide delete ID SLIDE_ID", "slide animation ID SLIDE_ID MODE", "image add ID SLIDE_ID FILE [ALT]", "image remove ID SLIDE_ID IMAGE_ID", "image position ID SLIDE_ID IMAGE_ID X Y WIDTH HEIGHT", "image background ID SLIDE_ID IMAGE_ID on|off", "template header ID TEXT", "template footer ID TEXT", "template outline ID on|off", "template theme ID THEME", "template logo ID FILE", "template logo-clear ID", "template font ID heading|body FILE.woff2", "template font-clear ID heading|body", "skill", "skill install [DIR]", "help authoring"],
                "deckTemplate": new_deck("Untitled presentation"),
                "notes": "Use scoped commands for small edits and editable bundles with apply-bundle for substantial edits. deck source/put-source is an advanced local-storage API; deck snapshot emits self-contained JSON. Replacements require a revision. Run deck validate and deck render or deck review after editing."
            });
            description["commands"].as_array_mut().unwrap().extend(
                [
                    "schema source",
                    "deck migrate ID",
                    "deck source ID",
                    "deck put-source FILE --if-revision HASH",
                    "deck history ID",
                    "deck restore ID HISTORY_HASH --if-revision CURRENT_HASH",
                ]
                .into_iter()
                .map(serde_json::Value::from),
            );
            output(description)
        }
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
        ["deck", "new", title, "--from", source_id] => {
            let source = read_deck(state, source_id)?;
            let mut deck = duplicate_deck(&source, title);
            save(state, &mut deck)?;
            output(deck)
        }
        ["deck", "get", id] => output(read_deck(state, id)?),
        ["deck", "import-bundle", directory] => {
            output(bundle::import(state, Path::new(directory))?)
        }
        ["deck", "apply-bundle", id, directory, "--if-revision", expected] => {
            output(bundle::apply(state, id, Path::new(directory), expected)?)
        }
        ["deck", "bundle", id, directory] => {
            let deck = read_deck(state, id)?;
            output(bundle::export(&deck, Path::new(directory))?)
        }
        ["deck", "migrate", id] => {
            let _lock = lock_deck_writes(state)?;
            let path = deck_path(state, id)?;
            let before = fs::read(&path).map_err(internal)?;
            let deck = parse_deck(id, &before, &state.data_dir.join("decks"))?;
            let version = serde_json::from_slice::<serde_json::Value>(&before).map_err(internal)?
                ["schemaVersion"]
                .as_u64()
                .unwrap_or(1);
            let after = if version == 2 {
                before.clone()
            } else {
                write_deck_unlocked(state, &deck)?
            };
            output(
                serde_json::json!({"id": id, "migrated": version != 2, "oldBytes": before.len(), "newBytes": after.len(), "revision": revision(&after)}),
            )
        }
        ["deck", "snapshot", id] => {
            let (deck, revision) = read_deck_snapshot(state, id)?;
            output(serde_json::json!({"revision": revision, "deck": deck}))
        }
        ["deck", "source", id] => {
            let bytes = fs::read(deck_path(state, id)?).map_err(internal)?;
            let deck: serde_json::Value = serde_json::from_slice(&bytes).map_err(internal)?;
            output(serde_json::json!({"revision": revision(&bytes), "deck": deck}))
        }
        ["deck", "revision", id] => {
            output(serde_json::json!({"id": id, "revision": deck_revision(state, id)?}))
        }
        ["deck", "history", id] => output(history::list(state, id)?),
        ["deck", "restore", id, historical, "--if-revision", expected] => {
            output(history::restore(state, id, historical, expected)?)
        }
        ["deck", "diff", id, historical] => {
            let (current, revision) = read_deck_snapshot(state, id)?;
            let before = history::load(state, id, historical)?;
            output(diff::describe(&before, historical, &current, &revision))
        }
        ["deck", "diff", id, historical, directory] => {
            let (current, revision) = read_deck_snapshot(state, id)?;
            let before = history::load(state, id, historical)?;
            output(diff::render(
                &before,
                historical,
                &current,
                &revision,
                Path::new(directory),
            )?)
        }
        ["deck", "revert-slide", id, historical, slide_id, "--if-revision", expected] => output(
            diff::revert_slide(state, id, historical, slide_id, expected)?,
        ),
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
        ["deck", "put-source", _, "--if-revision", expected] => {
            let value: serde_json::Value =
                serde_json::from_slice(document_input.as_deref().unwrap()).map_err(internal)?;
            let id = value
                .get("id")
                .and_then(|item| item.as_str())
                .ok_or("Deck is missing ID")?
                .to_owned();
            if deck_revision(state, &id)? != *expected {
                return Err(
                    "Deck changed; fetch the latest source and revision before replacing it".into(),
                );
            }
            let value = bundle::hydrate_local(value, &id, &state.data_dir.join("decks"))?;
            let mut deck: Deck = serde_json::from_value(value).map_err(internal)?;
            save(state, &mut deck)?;
            output(serde_json::json!({"id": id, "revision": deck_revision(state, &id)?}))
        }
        ["deck", "validate", id] => {
            let deck = read_deck(state, id)?;
            validate_deck(&deck)?;
            output(serde_json::json!({"id": id, "valid": true, "slideCount": deck.slides.len()}))
        }
        ["deck", "review", id, directory] => {
            let (deck, current_revision) = read_deck_snapshot(state, id)?;
            output(review::run(&deck, &current_revision, Path::new(directory))?)
        }
        ["deck", "render", id, slide_id, directory] => {
            let (deck, current_revision) = read_deck_snapshot(state, id)?;
            let index = slide_index(&deck, slide_id)?;
            output(review::run_selected(
                &deck,
                &current_revision,
                Path::new(directory),
                vec![index],
            )?)
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
            let url = present(state, id)?;
            let session = url
                .strip_prefix("hyperframe://app/")
                .and_then(|value| value.split('/').next())
                .ok_or("Invalid presentation URI")?;
            output(
                serde_json::json!({"id": id, "session": session, "uri": url, "audience": true, "status": "opening"}),
            )?;
            io::stdout().flush().map_err(internal)?;
            native::launch_audience(state.clone(), url);
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
        ["present", "notes", token, value @ ("on" | "off")] => {
            output(control(state, token, &format!("notes {value}"))?)
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
                    outline_group: String::new(),
                    title: "New slide".into(),
                    body: String::new(),
                    notes: String::new(),
                    reveal_options: Vec::new(),
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
            if image.background {
                return Err(
                    "Background picture fills the slide; turn off background mode to position it"
                        .into(),
                );
            }
            image.x = x;
            image.y = y;
            image.width = width;
            image.height = height;
            let image = image.clone();
            save(state, &mut deck)?;
            output(image)
        }
        ["image", "background", deck_id, slide_id, image_id, mode] => {
            let enabled = match *mode {
                "on" => true,
                "off" => false,
                _ => return Err("Background mode must be on or off".into()),
            };
            let mut deck = read_deck(state, deck_id)?;
            let index = slide_index(&deck, slide_id)?;
            let images = &mut deck.slides[index].images;
            let selected = images
                .iter()
                .position(|image| image.id == *image_id)
                .ok_or("Picture not found")?;
            for (position, image) in images.iter_mut().enumerate() {
                if position == selected {
                    image.background = enabled;
                    (image.x, image.y, image.width, image.height) = if enabled {
                        (0.0, 0.0, 100.0, 100.0)
                    } else {
                        (54.0, 28.0, 38.0, 50.0)
                    };
                } else if enabled && image.background {
                    image.background = false;
                    (image.x, image.y, image.width, image.height) = (54.0, 28.0, 38.0, 50.0);
                }
            }
            let image = images[selected].clone();
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
        ["template", "outline", deck_id, value] => {
            let enabled = match *value {
                "on" => true,
                "off" => false,
                _ => return Err("Outline must be on or off".into()),
            };
            let mut deck = read_deck(state, deck_id)?;
            deck.template.show_outline = enabled;
            save(state, &mut deck)?;
            output(deck.template)
        }
        ["template", "theme", deck_id, value] => {
            if !["midnight", "paper", "cobalt", "sunset", "regent"].contains(value) {
                return Err("Invalid theme".into());
            }
            let mut deck = read_deck(state, deck_id)?;
            deck.theme = (*value).into();
            save(state, &mut deck)?;
            output(serde_json::json!({"id":deck.id,"theme":deck.theme}))
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
        ["template", "font", deck_id, role, _] => {
            let mut deck = read_deck(state, deck_id)?;
            match *role {
                "heading" => deck.template.heading_font = font_input.take(),
                "body" => deck.template.body_font = font_input.take(),
                _ => return Err("Font role must be heading or body".into()),
            }
            save(state, &mut deck)?;
            output(serde_json::json!({"id":deck.id,"role":role,"embedded":true}))
        }
        ["template", "font-clear", deck_id, role] => {
            let mut deck = read_deck(state, deck_id)?;
            match *role {
                "heading" => deck.template.heading_font = None,
                "body" => deck.template.body_font = None,
                _ => return Err("Font role must be heading or body".into()),
            }
            save(state, &mut deck)?;
            output(serde_json::json!({"id":deck.id,"role":role,"embedded":false}))
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

#[cfg(test)]
mod skill_tests {
    use super::*;

    #[test]
    fn installer_updates_only_managed_skill_files() {
        let directory = env::temp_dir().join(format!(
            "hyperframe-slides-skill-test-{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let path = install_skill(&directory).unwrap();
        assert_eq!(
            fs::read(&path).unwrap(),
            include_bytes!("../assets/agent-skill.md")
        );
        install_skill(&directory).unwrap();
        let older_managed = b"older application-supplied skill";
        fs::write(&path, older_managed).unwrap();
        fs::write(
            directory.join(".hyperframe-slides-managed"),
            format!("{:x}", Sha256::digest(older_managed)),
        )
        .unwrap();
        install_skill(&directory).unwrap();
        assert_eq!(
            fs::read(&path).unwrap(),
            include_bytes!("../assets/agent-skill.md")
        );
        fs::write(&path, "personal changes").unwrap();
        assert!(install_skill(&directory).is_err());
        assert_eq!(fs::read_to_string(&path).unwrap(), "personal changes");
        fs::remove_dir_all(directory).unwrap();
    }
}
