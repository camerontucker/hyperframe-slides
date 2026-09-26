use gtk::prelude::*;
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    env, fs,
    net::TcpListener,
    path::PathBuf,
    process::{Child, Command, Stdio},
    sync::{Arc, Mutex},
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use webkit2gtk::WebViewExt;

const HYPERFRAMES_VERSION: &str = "0.8.78";

#[derive(Clone)]
struct AppState {
    data_dir: PathBuf,
    presentations: Arc<Mutex<HashMap<String, RunningPresentation>>>,
}

struct RunningPresentation {
    port: u16,
    child: Child,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Deck {
    id: String,
    title: String,
    theme: String,
    slides: Vec<Slide>,
    updated_at: u64,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Slide {
    id: String,
    layout: String,
    eyebrow: String,
    title: String,
    body: String,
    notes: String,
}

type ApiError = String;

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
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

fn valid_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 80 && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
}

fn deck_path(state: &AppState, id: &str) -> Result<PathBuf, ApiError> {
    if !valid_id(id) {
        return Err("Invalid deck ID".into());
    }
    Ok(state.data_dir.join("decks").join(format!("{id}.json")))
}

fn read_deck(state: &AppState, id: &str) -> Result<Deck, ApiError> {
    let path = deck_path(state, id)?;
    let data = fs::read(path).map_err(|_| "Deck not found".to_string())?;
    serde_json::from_slice(&data).map_err(|_| "Could not read deck".to_string())
}

fn write_deck(state: &AppState, deck: &Deck) -> Result<(), ApiError> {
    let path = deck_path(state, &deck.id)?;
    let json = serde_json::to_vec_pretty(deck).map_err(internal)?;
    let tmp = path.with_extension("json.tmp");
    fs::write(&tmp, json).map_err(internal)?;
    fs::rename(tmp, path).map_err(internal)?;
    Ok(())
}

fn internal(error: impl std::fmt::Display) -> ApiError {
    error.to_string()
}

fn validate_deck(deck: &Deck) -> Result<(), ApiError> {
    if !valid_id(&deck.id) || deck.title.trim().is_empty() || deck.title.chars().count() > 160 {
        return Err("Invalid deck title or ID".into());
    }
    if !["midnight", "paper", "cobalt", "sunset"].contains(&deck.theme.as_str()) {
        return Err("Invalid theme".into());
    }
    if deck.slides.is_empty() || deck.slides.len() > 100 {
        return Err("A deck needs 1–100 slides".into());
    }
    let mut ids = std::collections::HashSet::new();
    for slide in &deck.slides {
        if !valid_id(&slide.id)
            || !ids.insert(&slide.id)
            || !["title", "statement", "split", "quote"].contains(&slide.layout.as_str())
        {
            return Err("Invalid slide ID or layout".into());
        }
        if [&slide.eyebrow, &slide.title, &slide.body, &slide.notes]
            .iter()
            .any(|s| s.len() > 10_000)
        {
            return Err("Slide text is too long".into());
        }
    }
    Ok(())
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
        "paper" => ("#f7f4ec", "#171b28", "#665d56", "#cf5f3a"),
        "cobalt" => ("#10296a", "#f5f8ff", "#bbcbf6", "#90e3ff"),
        "sunset" => ("#351d35", "#fff4e9", "#f3c2b8", "#ffad75"),
        _ => ("#10151f", "#f5f7fb", "#aab7cb", "#9de8ca"),
    }
}

fn export_html(deck: &Deck) -> Result<String, ApiError> {
    validate_deck(deck)?;
    let (bg, fg, muted, accent) = theme_colors(&deck.theme);
    let slides: Vec<serde_json::Value> = deck
        .slides
        .iter()
        .enumerate()
        .map(|(index, slide)| serde_json::json!({"sceneId": slide.id, "startTime": index * 6, "endTime": index * 6 + 6, "notes": slide.notes}))
        .collect();
    let island =
        serde_json::to_string_pretty(&serde_json::json!({"slides": slides, "slideSequences": []}))
            .map_err(internal)?
            .replace('<', "\\u003c");
    let mut html = format!(
        r#"<!doctype html><html lang="en"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1"><title>{}</title><style>
*{{box-sizing:border-box}}html,body{{margin:0;height:100%;background:{};font-family:system-ui,-apple-system,BlinkMacSystemFont,'Segoe UI',sans-serif}}.slide{{position:relative;width:100%;height:100%;overflow:hidden;background:{};color:{}}}.clip{{position:absolute;margin:0}}.eyebrow{{left:7%;top:24%;font-size:28px;font-weight:800;letter-spacing:.18em;text-transform:uppercase;color:{}}}.heading{{left:7%;top:31%;font-size:92px;line-height:1.08;letter-spacing:-.045em;width:86%;font-weight:800;white-space:pre-wrap}}.body{{left:7%;top:58%;font-size:46px;line-height:1.25;color:{};width:82%;white-space:pre-wrap}}.rule{{left:7%;top:18%;width:96px;height:9px;background:{};border-radius:8px}}.layout-statement .heading{{font-size:104px}}.layout-quote .heading{{font-size:84px;font-weight:600;font-style:italic}}.layout-split .heading{{width:48%;font-size:77px}}.layout-split .body{{left:55%;top:31%;width:38%;border-left:8px solid {};padding-left:55px;color:{};font-size:50px}}.slide-number{{right:7%;bottom:8%;font-size:28px;color:{};letter-spacing:.12em}}
</style><script src="https://cdn.jsdelivr.net/npm/gsap@3/dist/gsap.min.js"></script></head><body><script type="application/hyperframes-slideshow+json">{}</script>
"#,
        escape_html(&deck.title),
        bg,
        bg,
        fg,
        accent,
        muted,
        accent,
        accent,
        muted,
        muted,
        island
    );
    html.push_str(&format!(
        "<div data-composition-id=\"deck-anchor\" data-start=\"0\" data-duration=\"{}\" data-width=\"1920\" data-height=\"1080\" style=\"position:absolute;width:100%;height:100%;pointer-events:none\"></div>\n",
        deck.slides.len() * 6
    ));
    for (index, slide) in deck.slides.iter().enumerate() {
        let start = index * 6;
        let title = escape_html(&slide.title);
        let eyebrow = escape_html(&slide.eyebrow);
        let body = escape_html(&slide.body);
        html.push_str(&format!(r#"<div id="{}-scene" class="slide layout-{}" data-composition-id="{}" data-start="{}" data-duration="6" data-label="{}" data-width="1920" data-height="1080"><div id="{}-rule" class="clip rule" data-start="{}" data-duration="6" data-track-index="1"></div><div id="{}-eyebrow" class="clip eyebrow" data-start="{}" data-duration="6" data-track-index="2">{}</div><h1 id="{}-heading" class="clip heading" data-start="{}" data-duration="6" data-track-index="3">{}</h1>{}<div id="{}-number" class="clip slide-number" data-start="{}" data-duration="6" data-track-index="5">{:02} / {:02}</div></div>
"#, slide.id, slide.layout, slide.id, start, title, slide.id, start, slide.id, start, eyebrow, slide.id, start, title, if body.is_empty() { String::new() } else { format!("<div id=\"{}-body\" class=\"clip body\" data-start=\"{}\" data-duration=\"6\" data-track-index=\"4\">{body}</div>", slide.id, start) }, slide.id, start, index + 1, deck.slides.len()));
    }
    html.push_str("<script>window.__timelines=window.__timelines||{};window.__timelines['deck-anchor']=gsap.timeline({paused:true});for(const scene of document.querySelectorAll('.slide')){const tl=gsap.timeline({paused:true});tl.fromTo(scene.querySelector('.heading'),{opacity:0,y:32},{opacity:1,y:0,duration:0.8,ease:'power2.out'},0);window.__timelines[scene.dataset.compositionId]=tl}</script></body></html>");
    Ok(html)
}

fn free_port() -> Result<u16, ApiError> {
    let listener = TcpListener::bind("127.0.0.1:0").map_err(internal)?;
    Ok(listener.local_addr().map_err(internal)?.port())
}

fn present(state: &AppState, id: &str) -> Result<String, ApiError> {
    let deck = read_deck(&state, &id)?;
    let html = export_html(&deck)?;
    let dir = state.data_dir.join("presentations").join(id);
    fs::create_dir_all(&dir).map_err(internal)?;
    fs::write(dir.join("index.html"), html).map_err(internal)?;
    let mut running = state.presentations.lock().map_err(internal)?;
    if let Some(existing) = running.get_mut(id) {
        if existing.child.try_wait().map_err(internal)?.is_none() {
            return Ok(format!("http://127.0.0.1:{}/", existing.port));
        }
    }
    let port = free_port()?;
    let log = fs::File::create(dir.join("present.log")).map_err(internal)?;
    let err = log.try_clone().map_err(internal)?;
    let child = Command::new(env::var("HYPERFRAMES_NPX").unwrap_or_else(|_| "npx".into()))
        .arg(format!("hyperframes@{HYPERFRAMES_VERSION}"))
        .arg("present")
        .arg(&dir)
        .arg(format!("--port={port}"))
        .arg("--no-open")
        .stdin(Stdio::null())
        .stdout(Stdio::from(log))
        .stderr(Stdio::from(err))
        .spawn()
        .map_err(internal)?;
    running.insert(id.to_string(), RunningPresentation { port, child });
    for _ in 0..300 {
        if std::net::TcpStream::connect(("127.0.0.1", port)).is_ok() {
            return Ok(format!("http://127.0.0.1:{port}/"));
        }
        if let Some(status) = running
            .get_mut(id)
            .unwrap()
            .child
            .try_wait()
            .map_err(internal)?
        {
            return Err(format!(
                "HyperFrames presenter exited with {status}; see {}",
                dir.join("present.log").display()
            ));
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    Err(format!(
        "HyperFrames presenter did not become ready; see {}",
        dir.join("present.log").display()
    ))
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
    fs::create_dir_all(data_dir.join("decks"))?;
    fs::create_dir_all(data_dir.join("presentations"))?;
    let state = AppState {
        data_dir,
        presentations: Arc::new(Mutex::new(HashMap::new())),
    };
    let args: Vec<String> = env::args().collect();
    if args.len() == 4 && args[1] == "--export" {
        let deck = read_deck(&state, &args[2])?;
        let path = PathBuf::from(&args[3]);
        fs::create_dir_all(&path)?;
        fs::write(path.join("index.html"), export_html(&deck)?)?;
        println!("Exported {}", path.join("index.html").display());
        return Ok(());
    }
    native::launch_gui(state);
    Ok(())
}

mod native;

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn export_escapes_content_and_has_slideshow_island() {
        let deck = Deck {
            id: "test".into(),
            title: "<bad>".into(),
            theme: "midnight".into(),
            updated_at: 0,
            slides: vec![Slide {
                id: "one".into(),
                layout: "title".into(),
                eyebrow: "Test".into(),
                title: "Hello <world>".into(),
                body: "</script>".into(),
                notes: "</script> secret".into(),
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
}
