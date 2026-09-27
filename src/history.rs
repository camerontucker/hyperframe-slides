use super::*;

const KEEP: usize = 30;

fn history_dir(state: &AppState, id: &str) -> Result<PathBuf, ApiError> {
    if !valid_id(id) {
        return Err("Invalid deck ID".into());
    }
    Ok(state.data_dir.join("history").join(id))
}

fn snapshots(state: &AppState, id: &str) -> Result<Vec<PathBuf>, ApiError> {
    let directory = history_dir(state, id)?;
    if !directory.exists() {
        return Ok(Vec::new());
    }
    let mut files = fs::read_dir(directory)
        .map_err(internal)?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.extension().and_then(|item| item.to_str()) == Some("json"))
        .collect::<Vec<_>>();
    files.sort();
    Ok(files)
}

pub(super) fn record_before_write(
    state: &AppState,
    id: &str,
    replacement: &[u8],
) -> Result<(), ApiError> {
    let current = deck_path(state, id)?;
    let old = match fs::read(&current) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(internal(error)),
    };
    if old == replacement {
        return Ok(());
    }
    let directory = history_dir(state, id)?;
    if !directory.exists() {
        fs::create_dir_all(&directory).map_err(internal)?;
    }
    fs::set_permissions(&directory, fs::Permissions::from_mode(0o700)).map_err(internal)?;
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(internal)?
        .as_nanos();
    let destination = directory.join(format!("{nonce}-{}.json", revision(&old)));
    if fs::hard_link(&current, &destination).is_err() {
        write_private(&destination, &old)?;
    }
    let files = snapshots(state, id)?;
    for path in files.iter().take(files.len().saturating_sub(KEEP)) {
        fs::remove_file(path).map_err(internal)?;
    }
    Ok(())
}

pub(super) fn list(state: &AppState, id: &str) -> Result<serde_json::Value, ApiError> {
    let mut entries = Vec::new();
    for path in snapshots(state, id)?.into_iter().rev() {
        let bytes = fs::read(&path).map_err(internal)?;
        let hash = revision(&bytes);
        if !path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.ends_with(&format!("-{hash}.json")))
        {
            continue;
        }
        if let Ok(deck) = parse_deck(id, &bytes, &state.data_dir.join("decks")) {
            entries.push(serde_json::json!({
                "revision": hash,
                "savedAt": deck.updated_at,
                "title": deck.title,
                "slideCount": deck.slides.len(),
            }));
        }
    }
    Ok(serde_json::json!({"id": id, "history": entries}))
}

pub(super) fn restore(
    state: &AppState,
    id: &str,
    revision_to_restore: &str,
    expected_current: &str,
) -> Result<Deck, ApiError> {
    let _lock = lock_deck_writes(state)?;
    if deck_revision(state, id)? != expected_current {
        return Err("Deck changed; fetch its current revision before restoring".into());
    }
    let mut restored = None;
    for path in snapshots(state, id)? {
        if !path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.ends_with(&format!("-{revision_to_restore}.json")))
        {
            continue;
        }
        let bytes = fs::read(path).map_err(internal)?;
        if revision(&bytes) == revision_to_restore {
            restored = Some(parse_deck(id, &bytes, &state.data_dir.join("decks"))?);
            break;
        }
    }
    let mut deck = restored.ok_or("History revision not found")?;
    deck.updated_at = now();
    write_deck_unlocked(state, &deck)?;
    Ok(deck)
}
