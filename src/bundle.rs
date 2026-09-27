use super::*;
use std::os::unix::fs::DirBuilderExt;

pub(super) const FORMAT: &str = "hyperframe-slides-bundle-v1";

fn private_dir(path: &Path) -> Result<(), ApiError> {
    fs::DirBuilder::new()
        .mode(0o700)
        .create(path)
        .map_err(internal)?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o700)).map_err(internal)
}

fn asset_name(uri: &str, folder: &str, root: &Path) -> Result<String, ApiError> {
    let (prefix, encoded) = uri.split_once(',').ok_or("Invalid embedded asset")?;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .map_err(|_| "Invalid embedded asset")?;
    let extension = match prefix {
        "data:image/png;base64" => "png",
        "data:image/jpeg;base64" => "jpg",
        "data:image/gif;base64" => "gif",
        "data:image/webp;base64" => "webp",
        "data:font/woff2;base64" => "woff2",
        _ => return Err("Unsupported embedded asset".into()),
    };
    if prefix == "data:font/woff2;base64" {
        validate_font_uri(uri)?;
    } else {
        validate_image_uri(uri, 8_000_000)?;
    }
    let digest = format!("{:x}", Sha256::digest(&bytes));
    let name = format!("{folder}/{digest}.{extension}");
    let path = root.join(&name);
    match fs::read(&path) {
        Ok(existing) if existing == bytes => {}
        Ok(_) => return Err("Stored asset does not match its checksum".into()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            write_private(&path, &bytes)?;
        }
        Err(error) => return Err(internal(error)),
    }
    Ok(name)
}

fn asset_uri(
    root: &Path,
    name: &str,
    expected_folder: &str,
    font: bool,
    require_digest: bool,
) -> Result<String, ApiError> {
    let (folder, file) = name.split_once('/').ok_or("Invalid bundle asset path")?;
    if file.len() > 128
        || file.starts_with('.')
        || !file
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_'))
    {
        return Err("Invalid bundle asset path".into());
    }
    if folder != expected_folder {
        return Err("Invalid bundle asset path".into());
    }
    let folder_metadata = fs::symlink_metadata(root.join(folder)).map_err(internal)?;
    if !folder_metadata.is_dir() || folder_metadata.file_type().is_symlink() {
        return Err("Bundle asset directory must be a regular directory".into());
    }
    let (digest, extension) = file.rsplit_once('.').ok_or("Invalid bundle asset name")?;
    let named_digest = digest.len() == 64 && digest.bytes().all(|byte| byte.is_ascii_hexdigit());
    if require_digest && !named_digest {
        return Err("Invalid bundle asset name".into());
    }
    let path = root.join(folder).join(file);
    let metadata = fs::symlink_metadata(&path).map_err(internal)?;
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Err("Bundle asset must be a regular file".into());
    }
    let limit = if font { 1_000_000 } else { 8_000_000 };
    if metadata.len() > limit {
        return Err("Bundle asset is too large".into());
    }
    let bytes = fs::read(path).map_err(internal)?;
    if bytes.len() > limit as usize {
        return Err("Bundle asset is too large".into());
    }
    if named_digest && format!("{:x}", Sha256::digest(&bytes)) != digest {
        return Err("Bundle asset checksum does not match its filename".into());
    }
    let mime = if font {
        if extension != "woff2" || !bytes.starts_with(b"wOF2") {
            return Err("Invalid bundle font".into());
        }
        "font/woff2"
    } else {
        let mime = image_mime(&bytes).ok_or("Invalid bundle picture")?;
        if extension
            != match mime {
                "image/jpeg" => "jpg",
                "image/png" => "png",
                "image/gif" => "gif",
                _ => "webp",
            }
        {
            return Err("Bundle picture extension does not match its contents".into());
        }
        mime
    };
    Ok(format!(
        "data:{mime};base64,{}",
        base64::engine::general_purpose::STANDARD.encode(bytes)
    ))
}

fn replace_assets(
    value: &mut serde_json::Value,
    root: &Path,
    exporting: bool,
) -> Result<(), ApiError> {
    let deck = value.get_mut("deck").ok_or("Bundle is missing deck")?;
    let template = deck
        .get_mut("template")
        .ok_or("Bundle is missing template")?;
    for (field, folder) in [
        ("logo", "assets"),
        ("headingFont", "fonts"),
        ("bodyFont", "fonts"),
    ] {
        if let Some(slot) = template.get_mut(field) {
            if let Some(text) = slot.as_str() {
                *slot = serde_json::Value::String(if exporting {
                    asset_name(text, folder, root)?
                } else {
                    asset_uri(root, text, folder, folder == "fonts", false)?
                });
            }
        }
    }
    let slides = deck
        .get_mut("slides")
        .and_then(|item| item.as_array_mut())
        .ok_or("Bundle is missing slides")?;
    for slide in slides {
        let images = slide
            .get_mut("images")
            .and_then(|item| item.as_array_mut())
            .ok_or("Invalid bundle pictures")?;
        for image in images {
            let object = image.as_object_mut().ok_or("Invalid bundle picture")?;
            if exporting {
                let uri = object
                    .remove("dataUri")
                    .and_then(|item| item.as_str().map(str::to_owned))
                    .ok_or("Picture is missing dataUri")?;
                object.insert("src".into(), asset_name(&uri, "assets", root)?.into());
            } else {
                let source = object
                    .remove("src")
                    .and_then(|item| item.as_str().map(str::to_owned))
                    .ok_or("Picture is missing src")?;
                object.insert(
                    "dataUri".into(),
                    asset_uri(root, &source, "assets", false, false)?.into(),
                );
            }
        }
    }
    Ok(())
}

pub(super) fn local_json(deck: &Deck, decks_directory: &Path) -> Result<Vec<u8>, ApiError> {
    let folder = format!("{}.assets", deck.id);
    let assets = decks_directory.join(&folder);
    match fs::symlink_metadata(&assets) {
        Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {}
        Ok(_) => return Err("Stored asset directory must be a regular directory".into()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => private_dir(&assets)?,
        Err(error) => return Err(internal(error)),
    }
    let mut value = serde_json::to_value(deck).map_err(internal)?;
    if let Some(template) = value.get_mut("template") {
        for field in ["logo", "headingFont", "bodyFont"] {
            if let Some(slot) = template.get_mut(field) {
                if let Some(uri) = slot.as_str() {
                    *slot = asset_name(uri, &folder, decks_directory)?.into();
                }
            }
        }
    }
    let slides = value
        .get_mut("slides")
        .and_then(|item| item.as_array_mut())
        .ok_or("Invalid slides")?;
    for slide in slides {
        let images = slide
            .get_mut("images")
            .and_then(|item| item.as_array_mut())
            .ok_or("Invalid pictures")?;
        for image in images {
            let object = image.as_object_mut().ok_or("Invalid picture")?;
            let uri = object
                .remove("dataUri")
                .and_then(|item| item.as_str().map(str::to_owned))
                .ok_or("Invalid picture")?;
            object.insert(
                "src".into(),
                asset_name(&uri, &folder, decks_directory)?.into(),
            );
        }
    }
    value["schemaVersion"] = 2.into();
    serde_json::to_vec_pretty(&value).map_err(internal)
}

pub(super) fn hydrate_local(
    mut value: serde_json::Value,
    id: &str,
    decks_directory: &Path,
) -> Result<serde_json::Value, ApiError> {
    if value.get("schemaVersion").and_then(|item| item.as_u64()) != Some(2) {
        return Ok(value);
    }
    let folder = format!("{id}.assets");
    let template = value.get_mut("template").ok_or("Invalid deck template")?;
    for field in ["logo", "headingFont", "bodyFont"] {
        if let Some(slot) = template.get_mut(field) {
            if let Some(source) = slot.as_str() {
                *slot = asset_uri(decks_directory, source, &folder, field != "logo", true)?.into();
            }
        }
    }
    let slides = value
        .get_mut("slides")
        .and_then(|item| item.as_array_mut())
        .ok_or("Invalid slides")?;
    for slide in slides {
        let images = slide
            .get_mut("images")
            .and_then(|item| item.as_array_mut())
            .ok_or("Invalid pictures")?;
        for image in images {
            let object = image.as_object_mut().ok_or("Invalid picture")?;
            let source = object
                .remove("src")
                .and_then(|item| item.as_str().map(str::to_owned))
                .ok_or("Invalid picture source")?;
            object.insert(
                "dataUri".into(),
                asset_uri(decks_directory, &source, &folder, false, true)?.into(),
            );
        }
    }
    value["schemaVersion"] = 1.into();
    Ok(value)
}

pub(super) fn export(deck: &Deck, directory: &Path) -> Result<serde_json::Value, ApiError> {
    validate_draft(deck)?;
    ensure_vacant(directory)?;
    let parent = directory.parent().ok_or("Invalid bundle destination")?;
    fs::create_dir_all(parent).map_err(internal)?;
    let temporary = parent.join(format!(
        ".hyperframe-bundle-{}-{}",
        std::process::id(),
        now()
    ));
    private_dir(&temporary)?;
    let result = (|| {
        private_dir(&temporary.join("assets"))?;
        private_dir(&temporary.join("fonts"))?;
        let mut value = serde_json::json!({"format": FORMAT, "deck": deck});
        replace_assets(&mut value, &temporary, true)?;
        write_private(
            &temporary.join("presentation.json"),
            &serde_json::to_vec_pretty(&value).map_err(internal)?,
        )?;
        ensure_vacant(directory)?;
        fs::rename(&temporary, directory).map_err(internal)?;
        Ok::<(), ApiError>(())
    })();
    if result.is_err() {
        let _ = fs::remove_dir_all(&temporary);
    }
    result?;
    Ok(
        serde_json::json!({"id": deck.id, "directory": directory, "manifest": directory.join("presentation.json")}),
    )
}

fn ensure_vacant(path: &Path) -> Result<(), ApiError> {
    match fs::symlink_metadata(path) {
        Ok(_) => Err("Bundle destination already exists".into()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(internal(error)),
    }
}

fn read(directory: &Path) -> Result<Deck, ApiError> {
    let manifest = directory.join("presentation.json");
    let metadata = fs::symlink_metadata(&manifest).map_err(internal)?;
    if !metadata.is_file() || metadata.file_type().is_symlink() || metadata.len() > 10_000_000 {
        return Err("Invalid bundle manifest".into());
    }
    let mut value: serde_json::Value =
        serde_json::from_slice(&fs::read(&manifest).map_err(internal)?).map_err(internal)?;
    if value.get("format").and_then(|item| item.as_str()) != Some(FORMAT) {
        return Err("Unsupported presentation bundle format".into());
    }
    replace_assets(&mut value, directory, false)?;
    let deck: Deck = serde_json::from_value(value["deck"].take()).map_err(internal)?;
    validate_draft(&deck)?;
    Ok(deck)
}

pub(super) fn import(state: &AppState, directory: &Path) -> Result<Deck, ApiError> {
    let mut deck = read(directory)?;
    let _lock = lock_deck_writes(state)?;
    while deck_path(state, &deck.id)?.exists() {
        deck = duplicate_deck(&deck, &deck.title.clone());
    }
    deck.updated_at = now();
    write_deck_unlocked(state, &deck)?;
    Ok(deck)
}

pub(super) fn apply(
    state: &AppState,
    id: &str,
    directory: &Path,
    expected_revision: &str,
) -> Result<serde_json::Value, ApiError> {
    if !valid_id(id) {
        return Err("Invalid deck ID".into());
    }
    let _lock = lock_deck_writes(state)?;
    if deck_revision(state, id)? != expected_revision {
        return Err("Deck changed; export a fresh bundle and retry".into());
    }
    let mut deck = read(directory)?;
    if deck.id != id {
        return Err("Bundle deck ID does not match the target deck".into());
    }
    deck.updated_at = now();
    let saved = write_deck_unlocked(state, &deck)?;
    Ok(serde_json::json!({"id": id, "revision": revision(&saved), "slideCount": deck.slides.len()}))
}
