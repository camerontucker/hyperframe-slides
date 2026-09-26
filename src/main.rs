use base64::Engine;
use gtk::prelude::*;
use pulldown_cmark::{Event, Parser, Tag, TagEnd};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    env, fs,
    io::Write,
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::{SystemTime, UNIX_EPOCH},
};
use webkit2gtk::WebViewExt;

#[derive(Clone)]
struct AppState {
    data_dir: PathBuf,
    presentations: Arc<Mutex<HashMap<String, PresentationSession>>>,
}

struct PresentationSession {
    title: String,
    html: String,
    island: String,
}

#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Deck {
    #[serde(default = "schema_version")]
    schema_version: u32,
    id: String,
    title: String,
    theme: String,
    #[serde(default)]
    template: SlideTemplate,
    slides: Vec<Slide>,
    updated_at: u64,
}

#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Slide {
    id: String,
    layout: String,
    eyebrow: String,
    title: String,
    body: String,
    notes: String,
    #[serde(default = "default_animation")]
    animation: String,
    #[serde(default)]
    images: Vec<SlideImage>,
}

#[derive(Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
#[serde(rename_all = "camelCase")]
struct SlideTemplate {
    header: String,
    footer: String,
    #[serde(default)]
    logo: Option<String>,
    show_outline: bool,
    heading_font: Option<String>,
    body_font: Option<String>,
}

#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SlideImage {
    id: String,
    alt: String,
    data_uri: String,
    x: f32,
    y: f32,
    width: f32,
    height: f32,
}

type ApiError = String;

fn schema_version() -> u32 {
    1
}

fn default_animation() -> String {
    "rise".into()
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn duplicate_deck(source: &Deck, title: &str) -> Deck {
    let mut deck = source.clone();
    let id = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    deck.id = format!("deck-{id}");
    deck.title = title.into();
    deck.updated_at = now();
    deck
}

fn data_dir() -> PathBuf {
    if let Ok(dir) = env::var("HYPERFRAME_SLIDES_DATA_DIR") {
        return PathBuf::from(dir);
    }
    let base = env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(env::var_os("HOME").unwrap_or_default()).join(".local/share")
        });
    base.join("hyperframe-slides")
}

fn prepare_data_dir(root: &Path) -> std::io::Result<()> {
    for path in [
        root.to_path_buf(),
        root.join("decks"),
        root.join("presentations"),
    ] {
        fs::create_dir_all(&path)?;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700))?;
    }
    for entry in fs::read_dir(root.join("decks"))? {
        let entry = entry?;
        if entry.path().extension().and_then(|value| value.to_str()) == Some("json")
            && entry.file_type()?.is_file()
        {
            fs::set_permissions(entry.path(), fs::Permissions::from_mode(0o600))?;
        }
    }
    Ok(())
}

fn valid_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 80 && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
}

fn deck_path(state: &AppState, id: &str) -> Result<PathBuf, ApiError> {
    if !valid_id(id) {
        return Err("Invalid deck ID".into());
    }
    Ok(state.data_dir.join("decks").join(format!("{id}.json")))
}

fn control_path(state: &AppState, token: &str) -> Result<PathBuf, ApiError> {
    if !token.starts_with("session-") || !valid_id(token) {
        return Err("Invalid presentation session ID".into());
    }
    Ok(state.data_dir.join("control").join(format!("{token}.sock")))
}

fn read_deck(state: &AppState, id: &str) -> Result<Deck, ApiError> {
    let data = fs::read(deck_path(state, id)?).map_err(|_| "Deck not found".to_string())?;
    parse_deck(id, &data)
}

fn read_deck_snapshot(state: &AppState, id: &str) -> Result<(Deck, String), ApiError> {
    let data = fs::read(deck_path(state, id)?).map_err(|_| "Deck not found".to_string())?;
    Ok((parse_deck(id, &data)?, revision(&data)))
}

fn parse_deck(id: &str, data: &[u8]) -> Result<Deck, ApiError> {
    let deck: Deck = serde_json::from_slice(data).map_err(|_| "Could not read deck".to_string())?;
    validate_draft(&deck)?;
    if deck.id != id {
        return Err("Deck ID does not match its file name".into());
    }
    Ok(deck)
}

fn import_deck_file(state: &AppState, path: &Path) -> Result<Deck, ApiError> {
    let bytes = fs::read(path).map_err(internal)?;
    let mut deck: Deck =
        serde_json::from_slice(&bytes).map_err(|_| "Could not read deck JSON".to_string())?;
    validate_draft(&deck)?;
    let destination = deck_path(state, &deck.id)?;
    if fs::canonicalize(path).ok() == fs::canonicalize(&destination).ok() && destination.exists() {
        return read_deck(state, &deck.id);
    }
    let _write_lock = lock_deck_writes(state)?;
    while deck_path(state, &deck.id)?.exists() {
        deck = duplicate_deck(&deck, &deck.title.clone());
    }
    deck.updated_at = now();
    write_deck_unlocked(state, &deck)?;
    Ok(deck)
}

fn write_deck(state: &AppState, deck: &Deck) -> Result<(), ApiError> {
    let _write_lock = lock_deck_writes(state)?;
    write_deck_unlocked(state, deck).map(|_| ())
}

fn write_deck_unlocked(state: &AppState, deck: &Deck) -> Result<Vec<u8>, ApiError> {
    validate_draft(deck)?;
    let path = deck_path(state, &deck.id)?;
    let json = serde_json::to_vec_pretty(deck).map_err(internal)?;
    write_private(&path, &json)?;
    Ok(json)
}

// All writers, including the editor, hold this lock from their disk read through
// the atomic rename. The lock file itself is stable across deck replacements.
fn lock_deck_writes(state: &AppState) -> Result<fs::File, ApiError> {
    let path = state.data_dir.join("decks/.write.lock");
    let file = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .open(path)
        .map_err(internal)?;
    file.lock().map_err(internal)?;
    Ok(file)
}

fn revision(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn deck_revision(state: &AppState, id: &str) -> Result<String, ApiError> {
    fs::read(deck_path(state, id)?)
        .map(|bytes| revision(&bytes))
        .map_err(|_| "Deck not found".into())
}

fn write_private(path: &Path, bytes: &[u8]) -> Result<(), ApiError> {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(internal)?
        .as_nanos();
    let tmp = path.with_extension(format!("tmp-{}-{nonce}", std::process::id()));
    let result = (|| {
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&tmp)?;
        file.write_all(bytes)?;
        fs::rename(&tmp, path)
    })();
    if let Err(error) = result {
        let _ = fs::remove_file(tmp);
        return Err(internal(error));
    }
    Ok(())
}

fn validate_draft(deck: &Deck) -> Result<(), ApiError> {
    if deck.schema_version != 1 {
        return Err(format!(
            "Unsupported deck schema version: {}",
            deck.schema_version
        ));
    }
    if !valid_id(&deck.id) || deck.slides.is_empty() {
        return Err("A deck needs a valid ID and at least one slide".into());
    }
    let mut ids = std::collections::HashSet::new();
    if deck
        .slides
        .iter()
        .any(|slide| !valid_id(&slide.id) || !ids.insert(&slide.id))
    {
        return Err("Slide IDs must be valid and unique".into());
    }
    // Drafts may exceed presentation text/slide limits, but everything loaded
    // into the editor preview must already be safe to render.
    if !["midnight", "paper", "cobalt", "sunset", "regent"].contains(&deck.theme.as_str()) {
        return Err("Invalid theme".into());
    }
    if let Some(logo) = &deck.template.logo {
        validate_image_uri(logo, 4_000_000)?;
    }
    for font in [&deck.template.heading_font, &deck.template.body_font]
        .into_iter()
        .flatten()
    {
        validate_font_uri(font)?;
    }
    for slide in &deck.slides {
        if !["title", "statement", "split", "quote"].contains(&slide.layout.as_str()) {
            return Err("Invalid slide layout".into());
        }
        if !["none", "fade", "rise", "zoom"].contains(&slide.animation.as_str()) {
            return Err("Invalid slide animation".into());
        }
        if slide.images.len() > 8 {
            return Err("A slide can hold at most eight pictures".into());
        }
        let mut image_ids = std::collections::HashSet::new();
        for image in &slide.images {
            if !valid_id(&image.id) || !image_ids.insert(&image.id) {
                return Err("Picture IDs must be valid and unique".into());
            }
            if ![image.x, image.y, image.width, image.height]
                .iter()
                .all(|value| value.is_finite())
                || image.x < 0.0
                || image.y < 0.0
                || image.width <= 0.0
                || image.height <= 0.0
                || image.x + image.width > 100.0
                || image.y + image.height > 100.0
            {
                return Err("Picture position must fit inside the slide".into());
            }
            validate_image_uri(&image.data_uri, 8_000_000)?;
        }
    }
    Ok(())
}

fn internal(error: impl std::fmt::Display) -> ApiError {
    error.to_string()
}

fn validate_deck(deck: &Deck) -> Result<(), ApiError> {
    validate_draft(deck)?;
    if !valid_id(&deck.id) || deck.title.trim().is_empty() || deck.title.chars().count() > 160 {
        return Err("Invalid deck title or ID".into());
    }
    if deck.slides.is_empty() || deck.slides.len() > 100 {
        return Err("A deck needs 1–100 slides".into());
    }
    if deck.template.header.len() > 500 || deck.template.footer.len() > 500 {
        return Err("Template text is too long".into());
    }
    for slide in &deck.slides {
        if [&slide.eyebrow, &slide.title, &slide.body, &slide.notes]
            .iter()
            .any(|s| s.len() > 10_000)
        {
            return Err("Slide text is too long".into());
        }
        for image in &slide.images {
            if image.alt.len() > 500 {
                return Err("Picture description is too long".into());
            }
        }
    }
    Ok(())
}

fn image_mime(bytes: &[u8]) -> Option<&'static str> {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some("image/png")
    } else if bytes.starts_with(b"\xff\xd8\xff") {
        Some("image/jpeg")
    } else if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        Some("image/gif")
    } else if bytes.len() >= 12 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        Some("image/webp")
    } else {
        None
    }
}

fn image_data_uri(path: &std::path::Path, limit: usize) -> Result<String, ApiError> {
    let bytes = fs::read(path).map_err(internal)?;
    if bytes.len() > limit {
        return Err("Picture file is too large".into());
    }
    let mime = image_mime(&bytes).ok_or("Use a PNG, JPEG, GIF, or WebP picture")?;
    Ok(format!(
        "data:{mime};base64,{}",
        base64::engine::general_purpose::STANDARD.encode(bytes)
    ))
}

fn validate_image_uri(uri: &str, limit: usize) -> Result<(), ApiError> {
    let (prefix, data) = uri.split_once(',').ok_or("Invalid picture data URI")?;
    let mime = prefix
        .strip_prefix("data:")
        .and_then(|value| value.strip_suffix(";base64"))
        .ok_or("Invalid picture data URI")?;
    if data.len() > limit.saturating_mul(4) / 3 + 8 {
        return Err("Picture is too large".into());
    }
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(data)
        .map_err(|_| "Invalid picture base64")?;
    if bytes.len() > limit || image_mime(&bytes) != Some(mime) {
        return Err("Picture format or size is invalid".into());
    }
    Ok(())
}

fn font_data_uri(path: &std::path::Path) -> Result<String, ApiError> {
    let bytes = fs::read(path).map_err(internal)?;
    if bytes.len() > 1_000_000 || !bytes.starts_with(b"wOF2") {
        return Err("Use a WOFF2 font under 1 MB".into());
    }
    Ok(format!(
        "data:font/woff2;base64,{}",
        base64::engine::general_purpose::STANDARD.encode(bytes)
    ))
}

fn validate_font_uri(uri: &str) -> Result<(), ApiError> {
    let data = uri
        .strip_prefix("data:font/woff2;base64,")
        .ok_or("Invalid WOFF2 font data URI")?;
    if data.len() > 1_333_344 {
        return Err("Font is too large".into());
    }
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(data)
        .map_err(|_| "Invalid font base64")?;
    if bytes.len() > 1_000_000 || !bytes.starts_with(b"wOF2") {
        return Err("Invalid WOFF2 font".into());
    }
    Ok(())
}

fn add_image_to_slide(
    slide: &mut Slide,
    path: &std::path::Path,
    alt: &str,
) -> Result<SlideImage, ApiError> {
    let uri = image_data_uri(path, 8_000_000)?;
    add_image_uri_to_slide(slide, uri, alt)
}

fn add_image_uri_to_slide(
    slide: &mut Slide,
    uri: String,
    alt: &str,
) -> Result<SlideImage, ApiError> {
    if slide.images.len() >= 8 {
        return Err("A slide can hold at most eight pictures".into());
    }
    if alt.len() > 500 {
        return Err("Picture description is too long".into());
    }
    let image = SlideImage {
        id: format!(
            "image-{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_err(internal)?
                .as_nanos()
        ),
        alt: alt.into(),
        data_uri: uri,
        x: 54.0,
        y: 28.0,
        width: 38.0,
        height: 50.0,
    };
    slide.images.push(image.clone());
    if slide.images.len() > 1 {
        let rows = slide.images.len().div_ceil(2) as f32;
        for (index, item) in slide.images.iter_mut().enumerate() {
            item.x = 53.0 + (index % 2) as f32 * 22.0;
            item.y = 24.0 + (index / 2) as f32 * (68.0 / rows);
            item.width = 20.0;
            item.height = 68.0 / rows - 2.0;
        }
    }
    Ok(slide.images.last().expect("inserted picture").clone())
}

fn render_markdown(text: &str, inline: bool) -> String {
    let mut output = String::new();
    let events = Parser::new(text).filter_map(|event| match event {
        Event::Html(value) | Event::InlineHtml(value) => Some(Event::Text(value)),
        Event::Start(Tag::Link { .. }) | Event::Start(Tag::Image { .. }) => None,
        Event::End(TagEnd::Link) | Event::End(TagEnd::Image) => None,
        Event::HardBreak => Some(Event::SoftBreak),
        Event::Start(Tag::Paragraph) | Event::End(TagEnd::Paragraph) if inline => None,
        other => Some(other),
    });
    pulldown_cmark::html::push_html(&mut output, events);
    output
}

fn escape_html(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

fn theme_colors(theme: &str) -> (&'static str, &'static str, &'static str, &'static str) {
    match theme {
        "regent" => ("#f6f5f7", "#004c45", "#586f73", "#00b3d4"),
        "paper" => ("#f7f4ec", "#171b28", "#665d56", "#cf5f3a"),
        "cobalt" => ("#10296a", "#f5f8ff", "#bbcbf6", "#90e3ff"),
        "sunset" => ("#351d35", "#fff4e9", "#f3c2b8", "#ffad75"),
        _ => ("#10151f", "#f5f7fb", "#aab7cb", "#9de8ca"),
    }
}

fn slide_css(deck: &Deck) -> String {
    let (bg, fg, muted, accent) = theme_colors(&deck.theme);
    let mut css = format!(
        r#"*{{box-sizing:border-box}}html,body{{margin:0;height:100%;background:{bg};font-family:system-ui,-apple-system,BlinkMacSystemFont,'Segoe UI',sans-serif}}.slide{{position:absolute;inset:0;width:100%;height:100%;overflow:hidden;background:{bg};color:{fg}}}.slide-main{{position:absolute;inset:0;width:100%;height:100%}}.has-outline .slide-main{{width:80%}}.deck-outline{{position:absolute;right:0;top:0;width:20%;height:100%;padding:48px 24px;background:{bg};border-left:3px solid {accent};overflow:auto}}.deck-outline-heading{{margin:0 0 22px;color:{accent};font-size:21px;font-weight:800;letter-spacing:.14em;text-transform:uppercase}}.deck-outline ol{{list-style:none;margin:0;padding:0}}.deck-outline li{{display:flex;align-items:baseline;gap:12px;min-height:65px;padding:10px 9px;border-left:5px solid transparent;color:{muted};font-size:23px;line-height:1.25}}.deck-outline li.active{{border-left-color:{accent};color:{fg};font-weight:700;background:color-mix(in srgb,{accent} 10%,transparent)}}.outline-number{{flex:none;color:{accent};font-size:20px;font-variant-numeric:tabular-nums}}.outline-label{{overflow:hidden;text-overflow:ellipsis;white-space:nowrap}}.clip{{position:absolute;margin:0}}.motion{{display:block;width:100%}}.template-header{{left:7%;top:5%;max-width:72%;font-size:27px;letter-spacing:.08em;color:{muted}}}.template-footer{{left:7%;bottom:5%;max-width:72%;font-size:25px;color:{muted}}}.template-logo{{right:7%;top:4%;width:13%;height:11%;object-fit:contain;object-position:right center}}.eyebrow{{left:7%;top:24%;font-size:28px;font-weight:800;letter-spacing:.18em;text-transform:uppercase;color:{accent}}}.heading{{left:7%;top:31%;font-size:92px;line-height:1.08;letter-spacing:-.045em;width:86%;font-weight:800;white-space:pre-wrap}}.body{{left:7%;top:58%;font-size:46px;line-height:1.25;color:{muted};width:82%;white-space:normal}}.body p{{margin:0 0 .35em}}.body ul,.body ol{{margin:.1em 0;padding-left:1.2em}}.body li{{padding-left:.1em}}.rule{{left:7%;top:18%;width:96px;height:9px;background:{accent};border-radius:8px}}.layout-statement .heading{{font-size:104px}}.layout-quote .heading{{font-size:84px;font-weight:600;font-style:italic}}.layout-split .heading{{width:48%;font-size:77px}}.layout-split .body{{left:55%;top:31%;width:38%;border-left:8px solid {accent};padding-left:55px;color:{muted};font-size:50px}}.has-image .heading{{width:44%;font-size:76px}}.has-image .body{{width:43%;font-size:41px}}.layout-split.has-image .body{{left:7%;top:67%;width:40%;border:0;padding:0;font-size:38px}}.slide-image{{object-fit:contain;object-position:center;border-radius:14px}}.slide-number{{right:7%;bottom:5%;font-size:28px;color:{muted};letter-spacing:.12em}}"#
    );
    if deck.template.show_outline {
        css.push_str(&format!(".has-outline .slide-main{{left:20%;right:auto}}.deck-outline{{left:0;right:auto;border-left:0;border-right:3px solid {accent}}}.deck-outline li{{border-left:0;border-right:5px solid transparent}}.deck-outline li.active{{border-right-color:{accent}}}.has-outline.has-image .body{{top:64%;font-size:38px}}.deck-outline li{{font-size:21px;gap:9px;padding-left:5px}}"));
    }
    if let Some(font) = &deck.template.heading_font {
        css.push_str(&format!("@font-face{{font-family:'Deck Heading';src:url('{font}') format('woff2');font-weight:400 900;font-display:block}}.heading,.eyebrow,.template-header,.template-footer,.deck-outline{{font-family:'Deck Heading',system-ui,sans-serif}}"));
    }
    if let Some(font) = &deck.template.body_font {
        css.push_str(&format!("@font-face{{font-family:'Deck Body';src:url('{font}') format('woff2');font-weight:400;font-display:block}}.body{{font-family:'Deck Body',Georgia,serif}}"));
    }
    if deck.theme == "regent" {
        css.push_str(".template-logo{width:16%;height:12%;right:6%;top:3%}.heading{letter-spacing:-.035em}.deck-outline{background:#eeebee}.deck-outline li.active{background:#d9f2f5}");
    }
    css
}

fn slide_html(deck: &Deck, slide: &Slide, index: usize, total: usize) -> String {
    let start = index * 6;
    let clip_start = 0;
    let title = render_markdown(&slide.title, true);
    let eyebrow = escape_html(&slide.eyebrow);
    let body = render_markdown(&slide.body, false);
    let id = escape_html(&slide.id);
    let mut html = format!(
        "<div id=\"{id}-scene\" class=\"slide layout-{}{}{}\" data-animation=\"{}\" data-composition-id=\"{id}\" data-start=\"{start}\" data-duration=\"6\" data-label=\"{}\" data-width=\"1920\" data-height=\"1080\"><div class=\"slide-main\">",
        escape_html(&slide.layout),
        if slide.images.is_empty() { "" } else { " has-image" },
        if deck.template.show_outline { " has-outline" } else { "" },
        escape_html(&slide.animation),
        escape_html(&slide.title)
    );
    html.push_str(&format!("<div id=\"{id}-rule\" class=\"clip rule\" data-start=\"{clip_start}\" data-duration=\"6\" data-track-index=\"1\"></div>"));
    if !deck.template.header.is_empty() {
        html.push_str(&format!("<div id=\"{id}-header\" class=\"clip template-header\" data-start=\"{clip_start}\" data-duration=\"6\" data-track-index=\"2\">{}</div>", escape_html(&deck.template.header)));
    }
    if let Some(logo) = &deck.template.logo {
        let alt = if deck.theme == "regent" {
            "Regent College logo"
        } else {
            "Presentation logo"
        };
        html.push_str(&format!("<img id=\"{id}-logo\" class=\"clip template-logo\" data-start=\"{clip_start}\" data-duration=\"6\" data-track-index=\"3\" src=\"{}\" alt=\"{alt}\">", escape_html(logo)));
    }
    html.push_str(&format!("<div id=\"{id}-eyebrow\" class=\"clip eyebrow\" data-start=\"{clip_start}\" data-duration=\"6\" data-track-index=\"4\">{eyebrow}</div>"));
    html.push_str(&format!("<h1 id=\"{id}-heading\" class=\"clip heading\" data-start=\"{clip_start}\" data-duration=\"6\" data-track-index=\"5\"><span class=\"motion\">{title}</span></h1>"));
    if !slide.body.is_empty() {
        html.push_str(&format!("<div id=\"{id}-body\" class=\"clip body\" data-start=\"{clip_start}\" data-duration=\"6\" data-track-index=\"6\"><div class=\"motion\">{body}</div></div>"));
    }
    for image in &slide.images {
        html.push_str(&format!("<img id=\"{id}-{}\" data-image-id=\"{}\" class=\"clip slide-image\" data-start=\"{clip_start}\" data-duration=\"6\" data-track-index=\"9\" style=\"left:{:.2}%;top:{:.2}%;width:{:.2}%;height:{:.2}%\" src=\"{}\" alt=\"{}\">", escape_html(&image.id), escape_html(&image.id), image.x, image.y, image.width, image.height, escape_html(&image.data_uri), escape_html(&image.alt)));
    }
    if !deck.template.footer.is_empty() {
        html.push_str(&format!("<div id=\"{id}-footer\" class=\"clip template-footer\" data-start=\"{clip_start}\" data-duration=\"6\" data-track-index=\"7\">{}</div>", escape_html(&deck.template.footer)));
    }
    html.push_str(&format!("<div id=\"{id}-number\" class=\"clip slide-number\" data-start=\"{clip_start}\" data-duration=\"6\" data-track-index=\"8\">{:02} / {:02}</div></div>", index + 1, total));
    if deck.template.show_outline {
        html.push_str("<aside class=\"deck-outline\" aria-label=\"Presentation outline\"><div class=\"deck-outline-heading\">Presentation outline</div><ol>");
        for (position, item) in deck.slides.iter().enumerate() {
            let label = item.eyebrow.split('·').next().unwrap_or("").trim();
            let label = if label.is_empty() {
                item.title.lines().next().unwrap_or("Slide")
            } else {
                label
            };
            html.push_str(&format!("<li{}><span class=\"outline-number\">{:02}</span><span class=\"outline-label\">{}</span></li>", if position == index { " class=\"active\" aria-current=\"step\"" } else { "" }, position + 1, escape_html(label)));
        }
        html.push_str("</ol></aside>");
    }
    html.push_str("</div>\n");
    html
}

const PREVIEW_DRAG_SCRIPT: &str = r#"<script>for(const img of document.querySelectorAll('.slide-image')){img.draggable=false;img.style.cursor='move';let origin=null;img.addEventListener('pointerdown',e=>{e.preventDefault();const bounds=img.offsetParent.getBoundingClientRect();origin={x:e.clientX,y:e.clientY,left:parseFloat(img.style.left),top:parseFloat(img.style.top),width:bounds.width,height:bounds.height};img.setPointerCapture(e.pointerId)});img.addEventListener('pointermove',e=>{if(!origin)return;const width=parseFloat(img.style.width),height=parseFloat(img.style.height);img.style.left=Math.max(0,Math.min(100-width,origin.left+(e.clientX-origin.x)/origin.width*100))+'%';img.style.top=Math.max(0,Math.min(100-height,origin.top+(e.clientY-origin.y)/origin.height*100))+'%'});img.addEventListener('pointerup',()=>{if(!origin)return;origin=null;window.webkit?.messageHandlers?.imagePosition?.postMessage(JSON.stringify({id:img.dataset.imageId,x:parseFloat(img.style.left),y:parseFloat(img.style.top)}))})}</script>"#;

const OUTLINE_SCROLL_SCRIPT: &str = r#"<script>for(const outline of document.querySelectorAll('.deck-outline')){const active=outline.querySelector('.active');if(active)outline.scrollTop=Math.max(0,active.offsetTop-outline.clientHeight/2+active.clientHeight/2)}</script>"#;

const PREVIEW_FIT_SCRIPT: &str = r#"<script>
function fit(){const scale=Math.min(innerWidth/1920,innerHeight/1080);const frame=document.getElementById('preview-frame');frame.style.transform=`translate(${(innerWidth-1920*scale)/2}px,${(innerHeight-1080*scale)/2}px) scale(${scale})`}
function checkFit(){
  const nodes=[...document.querySelectorAll('.template-header,.template-footer,.template-logo,.eyebrow,.heading,.body,.slide-image,.slide-number')];
  const label=el=>el.classList.contains('heading')?'headline':el.classList.contains('body')?'supporting text':el.classList.contains('slide-image')?'picture':el.classList.contains('template-logo')?'logo':el.classList.contains('template-header')?'header':el.classList.contains('template-footer')?'footer':el.classList.contains('slide-number')?'slide number':'eyebrow';
  const overflow=nodes.filter(el=>el.offsetTop<0||el.offsetLeft<0||el.offsetTop+el.scrollHeight>1050||el.offsetLeft+el.scrollWidth>1900).map(el=>({id:el.id,kind:label(el)}));
  const overlap=[];
  for(let i=0;i<nodes.length;i++)for(let j=i+1;j<nodes.length;j++){
    const a=nodes[i].getBoundingClientRect(),b=nodes[j].getBoundingClientRect();
    if(Math.min(a.right,b.right)-Math.max(a.left,b.left)>8 && Math.min(a.bottom,b.bottom)-Math.max(a.top,b.top)>8)overlap.push({firstId:nodes[i].id,secondId:nodes[j].id,firstKind:label(nodes[i]),secondKind:label(nodes[j])});
  }
  const missingImages=[...document.images].filter(img=>!img.complete||!img.naturalWidth).map(img=>({id:img.id,kind:label(img)}));
  window.__hfReview={overflow,overlap,missingImages};
  document.title=overflow.length||overlap.length||missingImages.length?`Check slide: ${overflow.length?'clipped '+[...new Set(overflow.map(item=>item.kind))].join(', '):''}${overflow.length&&overlap.length?'; ':''}${overlap.length?'overlap '+[...new Set(overlap.map(item=>item.firstKind+' / '+item.secondKind))].join(', '):''}${missingImages.length?'; missing picture':''}`:'No clipping or overlap detected';
}
addEventListener('resize',fit);fit();requestAnimationFrame(checkFit);document.fonts?.ready.then(checkFit)
</script>"#;

const REVIEW_READY_SCRIPT: &str = r#"<script>Promise.all([document.fonts.ready,...[...document.images].map(img=>img.decode?img.decode().catch(()=>{}):Promise.resolve())]).then(()=>requestAnimationFrame(()=>{checkFit();document.title='HyperFrames review ready'}))</script>"#;

fn preview_html(deck: &Deck, index: usize) -> Result<String, ApiError> {
    validate_draft(deck)?;
    let slide = deck.slides.get(index).ok_or("Slide not found")?;
    let css = slide_css(deck);
    let markup = slide_html(deck, slide, index, deck.slides.len());
    Ok(format!(
        r#"<!doctype html><html><head><meta charset="utf-8"><style>{css}html,body{{width:100%;height:100%;overflow:hidden}}#preview-frame{{position:absolute;left:0;top:0;width:1920px;height:1080px;transform-origin:top left}}</style></head><body><div id="preview-frame">{markup}</div>{PREVIEW_FIT_SCRIPT}{PREVIEW_DRAG_SCRIPT}{OUTLINE_SCROLL_SCRIPT}</body></html>"#
    ))
}

fn review_html(deck: &Deck, index: usize) -> Result<String, ApiError> {
    Ok(preview_html(deck, index)?.replace(
        "</body></html>",
        &format!("{REVIEW_READY_SCRIPT}</body></html>"),
    ))
}

fn export_html(deck: &Deck) -> Result<String, ApiError> {
    export_html_with_notes(deck, true)
}

fn export_html_with_notes(deck: &Deck, include_notes: bool) -> Result<String, ApiError> {
    validate_deck(deck)?;
    let island = slideshow_island(deck, include_notes)?;
    let mut html = format!(
        "<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\"><meta name=\"viewport\" content=\"width=device-width,initial-scale=1\"><title>{}</title><style>{}</style><script src=\"https://cdn.jsdelivr.net/npm/gsap@3/dist/gsap.min.js\"></script></head><body><script type=\"application/hyperframes-slideshow+json\">{island}</script>\n",
        escape_html(&deck.title),
        slide_css(deck),
    );
    html.push_str(&format!(
        "<div data-composition-id=\"deck-anchor\" data-start=\"0\" data-duration=\"{}\" data-width=\"1920\" data-height=\"1080\" style=\"position:absolute;width:100%;height:100%;pointer-events:none\"></div>\n",
        deck.slides.len() * 6
    ));
    for (index, slide) in deck.slides.iter().enumerate() {
        html.push_str(&slide_html(deck, slide, index, deck.slides.len()));
    }
    html.push_str("<script>window.__timelines=window.__timelines||{};window.__timelines['deck-anchor']=gsap.timeline({paused:true});for(const scene of document.querySelectorAll('.slide')){const tl=gsap.timeline({paused:true});const heading=scene.querySelector('.heading .motion');const body=scene.querySelector('.body .motion');switch(scene.dataset.animation){case 'fade':if(heading)tl.fromTo(heading,{opacity:0},{opacity:1,duration:.65,ease:'power2.out'},0);if(body)tl.fromTo(body,{opacity:0,y:16},{opacity:1,y:0,duration:.95,ease:'power3.out'},.18);break;case 'rise':if(heading)tl.fromTo(heading,{opacity:0,y:28},{opacity:1,y:0,duration:.75,ease:'power2.out'},0);if(body)tl.fromTo(body,{opacity:0,y:34},{opacity:1,y:0,duration:1.05,ease:'power3.out'},.18);break;case 'zoom':if(heading)tl.fromTo(heading,{opacity:0,scale:.92},{opacity:1,scale:1,duration:.75,ease:'power2.out'},0);if(body)tl.fromTo(body,{opacity:0,y:20},{opacity:1,y:0,duration:1.05,ease:'power3.out'},.18);break;default:break}window.__timelines[scene.dataset.compositionId]=tl}</script>");
    html.push_str(OUTLINE_SCROLL_SCRIPT);
    html.push_str("</body></html>");
    Ok(html)
}

fn slideshow_island(deck: &Deck, include_notes: bool) -> Result<String, ApiError> {
    let slides: Vec<serde_json::Value> = deck
        .slides
        .iter()
        .enumerate()
        .map(|(index, slide)| {
            serde_json::json!({
                "sceneId": slide.id, "startTime": index * 6,
                "endTime": index * 6 + 6, "notes": if include_notes { slide.notes.as_str() } else { "" }
            })
        })
        .collect();
    Ok(
        serde_json::to_string(&serde_json::json!({"slides": slides, "slideSequences": []}))
            .map_err(internal)?
            .replace('<', "\\u003c"),
    )
}

fn present(state: &AppState, id: &str) -> Result<String, ApiError> {
    let deck = read_deck(state, id)?;
    let html = export_html(&deck)?;
    let island = slideshow_island(&deck, true)?;
    let token = format!(
        "session-{}",
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(internal)?
            .as_nanos()
    );
    state.presentations.lock().map_err(internal)?.insert(
        token.clone(),
        PresentationSession {
            title: deck.title,
            html,
            island,
        },
    );
    Ok(format!("hyperframe://app/{token}/presenter.html"))
}

fn presenter_page(token: &str, session: &PresentationSession) -> String {
    let title = escape_html(&session.title);
    let island = &session.island;
    format!(
        r#"<!doctype html><html lang="en"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1"><title>{title} — Presenter</title><style>*{{box-sizing:border-box}}html,body{{margin:0;width:100%;height:100%;overflow:hidden;background:#0a0a0a}}hyperframes-slideshow{{display:block;position:relative;width:100vw;height:100vh}}hyperframes-player{{position:absolute;inset:0}}hyperframes-slideshow:not([data-hf-show-notes="true"]) hyperframes-player{{bottom:0!important;height:100%!important}}hyperframes-slideshow:not([data-hf-show-notes="true"]) [data-hf-presenter]{{display:none!important}}hyperframes-slideshow:not([data-hf-show-notes="true"]) [data-hf-nav-cluster]{{bottom:28px!important}}</style><script src="/assets/player.js"></script><script src="/assets/slideshow.js"></script><script src="/assets/text-entrance.js"></script></head><body><hyperframes-slideshow tabindex="0" sound><hyperframes-player interactive src="/{token}/composition/index.html"></hyperframes-player><script type="application/hyperframes-slideshow+json">{island}</script></hyperframes-slideshow></body></html>"#
    )
}

fn resource_for_uri(state: &AppState, uri: &str) -> Option<(Vec<u8>, &'static str)> {
    let path = uri.strip_prefix("hyperframe://app/")?.split('?').next()?;
    match path {
        "assets/player.js" => {
            return Some((
                include_bytes!("../assets/vendor/player.js").to_vec(),
                "application/javascript",
            ))
        }
        "assets/slideshow.js" => {
            return Some((
                include_bytes!("../assets/vendor/slideshow.js").to_vec(),
                "application/javascript",
            ))
        }
        "assets/text-entrance.js" => {
            return Some((
                include_bytes!("../assets/text-entrance.js").to_vec(),
                "application/javascript",
            ))
        }
        "assets/runtime.js" => {
            return Some((
                include_bytes!("../assets/vendor/runtime.js").to_vec(),
                "application/javascript",
            ))
        }
        "assets/gsap.js" => {
            return Some((
                include_bytes!("../assets/vendor/gsap.min.js").to_vec(),
                "application/javascript",
            ))
        }
        _ => {}
    }
    let (token, file) = path.split_once('/')?;
    if !token.starts_with("session-") || !token[8..].bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let sessions = state.presentations.lock().ok()?;
    let session = sessions.get(token)?;
    match file {
        "presenter.html" => Some((presenter_page(token, session).into_bytes(), "text/html")),
        "composition/index.html" => {
            let html = session.html.replace(
                "<script src=\"https://cdn.jsdelivr.net/npm/gsap@3/dist/gsap.min.js\"></script>",
                "<script src=\"/assets/runtime.js\"></script><script src=\"/assets/gsap.js\"></script>",
            );
            Some((html.into_bytes(), "text/html"))
        }
        _ => None,
    }
}

fn configure_popup(view: &webkit2gtk::WebView, app: &gtk::Application) {
    let app = app.clone();
    view.connect_create(move |parent, _| {
        let child = webkit2gtk::WebView::with_related_view(parent);
        let window = gtk::ApplicationWindow::new(&app);
        window.set_title("HyperFrames Presentation");
        window.set_default_size(1280, 720);
        window.add(&child);
        configure_popup(&child, &app);
        window.show_all();
        Some(child.upcast::<gtk::Widget>())
    });
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let data_dir = data_dir();
    prepare_data_dir(&data_dir)?;
    let state = AppState {
        data_dir,
        presentations: Arc::new(Mutex::new(HashMap::new())),
    };
    let args: Vec<String> = env::args().collect();
    if args.len() > 1 {
        if let Err(error) = cli::run(&state, &args[1..]) {
            eprintln!("{}", serde_json::json!({"error": error}));
            std::process::exit(1);
        }
        return Ok(());
    }
    native::launch_gui(state);
    Ok(())
}

mod cli;
mod native;
mod review;

#[cfg(test)]
mod tests {
    use super::*;

    fn deck() -> Deck {
        Deck {
            schema_version: 1,
            id: "test".into(),
            title: "Test".into(),
            theme: "midnight".into(),
            template: SlideTemplate::default(),
            updated_at: 0,
            slides: vec![Slide {
                id: "one".into(),
                layout: "title".into(),
                eyebrow: "Eyebrow".into(),
                title: "Line one\nLine two".into(),
                body: "Body".into(),
                notes: "private note".into(),
                animation: default_animation(),
                images: Vec::new(),
            }],
        }
    }

    fn state() -> AppState {
        let data_dir = env::temp_dir().join(format!(
            "hyperframe-slides-test-{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(data_dir.join("decks")).unwrap();
        AppState {
            data_dir,
            presentations: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    #[test]
    fn export_escapes_content_and_has_slideshow_island() {
        let deck = Deck {
            schema_version: 1,
            id: "test".into(),
            title: "<bad>".into(),
            theme: "midnight".into(),
            template: SlideTemplate::default(),
            updated_at: 0,
            slides: vec![Slide {
                id: "one".into(),
                layout: "title".into(),
                eyebrow: "Test".into(),
                title: "Hello <world>".into(),
                body: "</script>".into(),
                notes: "</script> secret".into(),
                animation: default_animation(),
                images: Vec::new(),
            }],
        };
        let html = export_html(&deck).unwrap();
        assert!(html.contains("application/hyperframes-slideshow+json"));
        assert!(html.contains("Hello &lt;world&gt;"));
        assert!(html.contains("\\u003c/script> secret"));
        assert!(!html.contains("<bad>"));
    }
    #[test]
    fn invalid_ids_are_rejected() {
        assert!(!valid_id("../bad"));
        assert!(valid_id("deck-1"));
    }

    #[test]
    fn preview_and_export_share_slide_markup_for_every_layout_and_theme() {
        for theme in ["midnight", "paper", "cobalt", "sunset"] {
            for layout in ["title", "statement", "split", "quote"] {
                let mut deck = deck();
                deck.theme = theme.into();
                deck.slides[0].layout = layout.into();
                let markup = slide_html(&deck, &deck.slides[0], 0, 1);
                let css = slide_css(&deck);
                assert!(preview_html(&deck, 0).unwrap().contains(&markup));
                assert!(export_html(&deck).unwrap().contains(&markup));
                assert!(preview_html(&deck, 0).unwrap().contains(&css));
                assert!(export_html(&deck).unwrap().contains(&css));
            }
        }
    }

    #[test]
    fn audience_export_omits_speaker_notes() {
        let html = export_html_with_notes(&deck(), false).unwrap();
        assert!(!html.contains("private note"));
        assert!(export_html(&deck()).unwrap().contains("private note"));
    }

    #[test]
    fn presentation_sessions_are_immutable_and_current() {
        let state = state();
        let mut deck = deck();
        write_deck(&state, &deck).unwrap();
        let first = present(&state, &deck.id).unwrap();
        deck.slides[0].notes = "new note".into();
        write_deck(&state, &deck).unwrap();
        let second = present(&state, &deck.id).unwrap();
        assert_ne!(first, second);
        let old = String::from_utf8(resource_for_uri(&state, &first).unwrap().0).unwrap();
        let new = String::from_utf8(resource_for_uri(&state, &second).unwrap().0).unwrap();
        assert!(old.contains("private note"));
        assert!(!old.contains("new note"));
        assert!(new.contains("new note"));
        assert!(new.contains("/assets/text-entrance.js"));
        assert!(!new.contains("hf-slide-transition"));
        let script = String::from_utf8(
            resource_for_uri(&state, "hyperframe://app/assets/text-entrance.js")
                .unwrap()
                .0,
        )
        .unwrap();
        assert!(script.contains("controller.player.play()"));
        assert!(script.contains("hfTextEntranceActive"));
        assert!(resource_for_uri(&state, "hyperframe://app/../../etc/passwd").is_none());
        fs::remove_dir_all(state.data_dir).unwrap();
    }

    #[test]
    fn empty_deck_is_rejected_on_load() {
        let state = state();
        let mut deck = deck();
        deck.slides.clear();
        fs::write(
            state.data_dir.join("decks/test.json"),
            serde_json::to_vec(&deck).unwrap(),
        )
        .unwrap();
        assert!(read_deck(&state, "test").is_err());
        fs::remove_dir_all(state.data_dir).unwrap();
    }

    #[test]
    fn opening_external_deck_imports_without_overwriting_existing_id() {
        let state = state();
        let original = deck();
        write_deck(&state, &original).unwrap();
        let mut external = original.clone();
        external.title = "Imported presentation".into();
        let external_path = state.data_dir.join("external.json");
        fs::write(&external_path, serde_json::to_vec(&external).unwrap()).unwrap();

        let imported = import_deck_file(&state, &external_path).unwrap();
        assert_ne!(imported.id, original.id);
        assert_eq!(imported.title, external.title);
        assert_eq!(
            read_deck(&state, &original.id).unwrap().title,
            original.title
        );
        assert_eq!(
            read_deck(&state, &imported.id).unwrap().title,
            external.title
        );
        assert_eq!(
            import_deck_file(&state, &deck_path(&state, &imported.id).unwrap())
                .unwrap()
                .id,
            imported.id
        );
        fs::write(&external_path, b"not JSON").unwrap();
        assert!(import_deck_file(&state, &external_path).is_err());
        fs::remove_dir_all(state.data_dir).unwrap();
    }

    #[test]
    fn deck_storage_and_exports_are_private() {
        let state = state();
        let deck = deck();
        write_deck(&state, &deck).unwrap();
        let path = deck_path(&state, &deck.id).unwrap();
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
        fs::set_permissions(&state.data_dir, fs::Permissions::from_mode(0o755)).unwrap();
        prepare_data_dir(&state.data_dir).unwrap();
        for directory in [&state.data_dir, &state.data_dir.join("decks")] {
            assert_eq!(
                fs::metadata(directory).unwrap().permissions().mode() & 0o777,
                0o700
            );
        }
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        let export = state.data_dir.join("export.html");
        fs::write(&export, "old").unwrap();
        fs::set_permissions(&export, fs::Permissions::from_mode(0o644)).unwrap();
        write_private(&export, b"new").unwrap();
        assert_eq!(fs::read(&export).unwrap(), b"new");
        assert_eq!(
            fs::metadata(&export).unwrap().permissions().mode() & 0o777,
            0o600
        );
        fs::remove_dir_all(state.data_dir).unwrap();
    }

    #[test]
    fn old_decks_load_as_version_one_and_future_versions_are_rejected() {
        let mut old = serde_json::to_value(deck()).unwrap();
        old.as_object_mut().unwrap().remove("schemaVersion");
        let loaded: Deck = serde_json::from_value(old).unwrap();
        assert_eq!(loaded.schema_version, 1);
        let mut future = deck();
        future.schema_version = 2;
        assert!(validate_draft(&future).is_err());
    }

    #[test]
    fn unsafe_drafts_cannot_reach_the_preview_or_disk() {
        let state = state();
        let mut deck = deck();
        deck.slides[0].layout = "title\" onmouseover=\"alert(1)".into();
        assert!(preview_html(&deck, 0).is_err());
        assert!(write_deck(&state, &deck).is_err());
        deck.slides[0].layout = "title".into();
        deck.slides[0].animation = "rise\" onload=\"alert(1)".into();
        assert!(preview_html(&deck, 0).is_err());
        deck.slides[0].animation = "rise".into();
        deck.slides[0].images.push(SlideImage {
            id: "picture\" onclick=\"alert(1)".into(),
            alt: String::new(),
            data_uri: "https://example.com/tracker.png".into(),
            x: 54.0,
            y: 28.0,
            width: 38.0,
            height: 50.0,
        });
        assert!(preview_html(&deck, 0).is_err());
        deck.slides[0].images[0].id = "picture".into();
        assert!(preview_html(&deck, 0).is_err());
        deck.slides.clear();
        fs::remove_dir_all(state.data_dir).unwrap();
    }

    #[test]
    fn replacement_requires_the_current_revision() {
        let state = state();
        let mut original = deck();
        write_deck(&state, &original).unwrap();
        let old_revision = deck_revision(&state, &original.id).unwrap();
        let input = state.data_dir.join("replacement.json");
        original.title = "First writer".into();
        fs::write(&input, serde_json::to_vec(&original).unwrap()).unwrap();
        let put = |revision: &str| {
            cli::run(
                &state,
                &[
                    "deck".into(),
                    "put".into(),
                    input.to_string_lossy().into_owned(),
                    "--if-revision".into(),
                    revision.into(),
                ],
            )
        };
        put(&old_revision).unwrap();
        let new_revision = deck_revision(&state, &original.id).unwrap();
        assert_ne!(old_revision, new_revision);
        original.title = "Second stale writer".into();
        fs::write(&input, serde_json::to_vec(&original).unwrap()).unwrap();
        assert!(put(&old_revision).is_err());
        assert_eq!(
            read_deck(&state, &original.id).unwrap().title,
            "First writer"
        );
        fs::remove_dir_all(state.data_dir).unwrap();
    }

    #[test]
    fn simultaneous_replacements_have_one_winner() {
        let state = state();
        let original = deck();
        write_deck(&state, &original).unwrap();
        let expected = deck_revision(&state, &original.id).unwrap();
        let barrier = Arc::new(std::sync::Barrier::new(3));
        let mut jobs = Vec::new();
        for title in ["Writer A", "Writer B"] {
            let state = state.clone();
            let barrier = barrier.clone();
            let expected = expected.clone();
            let mut candidate = original.clone();
            candidate.title = title.into();
            let file = state.data_dir.join(format!("{title}.json"));
            fs::write(&file, serde_json::to_vec(&candidate).unwrap()).unwrap();
            jobs.push(std::thread::spawn(move || {
                barrier.wait();
                cli::run(
                    &state,
                    &[
                        "deck".into(),
                        "put".into(),
                        file.to_string_lossy().into_owned(),
                        "--if-revision".into(),
                        expected,
                    ],
                )
            }));
        }
        barrier.wait();
        let outcomes: Vec<_> = jobs.into_iter().map(|job| job.join().unwrap()).collect();
        assert_eq!(outcomes.iter().filter(|result| result.is_ok()).count(), 1);
        assert!(matches!(
            read_deck(&state, &original.id).unwrap().title.as_str(),
            "Writer A" | "Writer B"
        ));
        fs::remove_dir_all(state.data_dir).unwrap();
    }

    #[test]
    fn markdown_formats_text_and_escapes_untrusted_html() {
        let html = render_markdown(
            "- **Bold** and *emphasis*\n- <script>alert(1)</script>",
            false,
        );
        assert!(html.contains("<ul>"));
        assert!(html.contains("<strong>Bold</strong>"));
        assert!(html.contains("<em>emphasis</em>"));
        assert!(!html.contains("<script>"));
        assert!(html.contains("&lt;script&gt;"));
    }

    #[test]
    fn template_picture_and_animation_render_on_all_slides() {
        let mut deck = deck();
        deck.template.header = "Project header".into();
        deck.template.footer = "Project footer".into();
        let png = base64::engine::general_purpose::STANDARD.encode(b"\x89PNG\r\n\x1a\nexample");
        deck.template.logo = Some(format!("data:image/png;base64,{png}"));
        deck.slides[0].animation = "zoom".into();
        deck.slides[0].images.push(SlideImage {
            id: "picture-1".into(),
            alt: "Sample".into(),
            data_uri: format!("data:image/png;base64,{png}"),
            x: 54.0,
            y: 28.0,
            width: 38.0,
            height: 50.0,
        });
        deck.slides.push(Slide {
            id: "two".into(),
            ..deck.slides[0].clone()
        });
        let html = export_html(&deck).unwrap();
        assert_eq!(html.matches("Project header").count(), 2);
        assert_eq!(html.matches("Project footer").count(), 2);
        assert_eq!(html.matches("class=\"clip template-logo\"").count(), 2);
        assert_eq!(html.matches("class=\"clip slide-image\"").count(), 2);
        assert!(!html.contains("slide-image motion"));
        assert!(!html.contains("slide-image.motion"));
        assert!(html.contains("data-animation=\"zoom\""));
        assert!(html.contains("id=\"two-scene\" class=\"slide layout-title has-image\" data-animation=\"zoom\" data-composition-id=\"two\" data-start=\"6\""));
        assert!(html.contains("id=\"two-heading\" class=\"clip heading\" data-start=\"0\""));
        assert!(preview_html(&deck, 0).unwrap().contains("Project header"));
    }

    #[test]
    fn outline_marks_current_slide_and_body_has_eased_motion() {
        let mut deck = deck();
        deck.template.show_outline = true;
        deck.slides[0].eyebrow = "Opening · 00:00".into();
        deck.slides[0].animation = "rise".into();
        deck.slides.push(Slide {
            id: "second".into(),
            eyebrow: "Questions <next>".into(),
            ..deck.slides[0].clone()
        });
        let first = preview_html(&deck, 0).unwrap();
        let second = preview_html(&deck, 1).unwrap();
        assert!(first.contains("class=\"slide layout-title has-outline\""));
        assert!(first.contains(".has-outline .slide-main{left:20%;right:auto}"));
        assert!(
            first.contains(".deck-outline{left:0;right:auto;border-left:0;border-right:3px solid")
        );
        assert!(first.contains(".deck-outline li.active{border-right-color:"));
        assert_eq!(first.matches("class=\"deck-outline\"").count(), 1);
        assert!(first
            .contains("class=\"active\" aria-current=\"step\"><span class=\"outline-number\">01"));
        assert!(second
            .contains("class=\"active\" aria-current=\"step\"><span class=\"outline-number\">02"));
        assert!(first.contains("Questions &lt;next&gt;"));
        let export = export_html(&deck).unwrap();
        assert!(export.contains("ease:'power3.out'"));
        assert!(export.contains("const body=scene.querySelector('.body .motion')"));
    }

    #[test]
    fn regent_theme_embeds_validated_fonts() {
        let mut deck = deck();
        deck.theme = "regent".into();
        let font = format!(
            "data:font/woff2;base64,{}",
            base64::engine::general_purpose::STANDARD.encode(b"wOF2font")
        );
        deck.template.heading_font = Some(font.clone());
        deck.template.body_font = Some(font);
        let html = export_html(&deck).unwrap();
        assert!(html.contains("background:#f6f5f7"));
        assert!(html.contains("font-family:'Deck Heading'"));
        assert!(html.contains("font-family:'Deck Body'"));
        deck.template.body_font = Some("data:font/woff2;base64,PHNjcmlwdD4=".into());
        assert!(export_html(&deck).is_err());
    }

    #[test]
    fn template_copy_preserves_brand_and_gets_its_own_id() {
        let mut source = deck();
        source.theme = "regent".into();
        source.template.header = "Regent College".into();
        let copy = duplicate_deck(&source, "New Regent talk");
        assert_ne!(copy.id, source.id);
        assert_eq!(copy.title, "New Regent talk");
        assert_eq!(copy.theme, "regent");
        assert!(copy.template == source.template);
        assert!(copy.slides == source.slides);
    }
}
