use super::*;
use serde::Serialize;
use std::collections::{HashMap, HashSet};

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct SlideChange {
    pub id: String,
    pub title: String,
    pub kind: String,
    pub before_index: Option<usize>,
    pub after_index: Option<usize>,
    pub summary: Vec<String>,
    pub can_revert: bool,
}

pub(super) fn changes(before: &Deck, after: &Deck) -> Vec<SlideChange> {
    let before_positions: HashMap<&str, usize> = before
        .slides
        .iter()
        .enumerate()
        .map(|(index, slide)| (slide.id.as_str(), index))
        .collect();
    let after_positions: HashMap<&str, usize> = after
        .slides
        .iter()
        .enumerate()
        .map(|(index, slide)| (slide.id.as_str(), index))
        .collect();
    let shared_style = before.theme != after.theme || before.template != after.template;
    let mut ids = Vec::new();
    let mut seen = HashSet::new();
    for slide in &after.slides {
        if seen.insert(slide.id.as_str()) {
            ids.push(slide.id.as_str());
        }
    }
    for slide in &before.slides {
        if seen.insert(slide.id.as_str()) {
            ids.push(slide.id.as_str());
        }
    }
    ids.into_iter()
        .filter_map(|id| {
            let old = before_positions.get(id).copied();
            let new = after_positions.get(id).copied();
            let mut summary = Vec::new();
            let (kind, title, can_revert) = match (old, new) {
                (None, Some(index)) => {
                    summary.push("Slide added".into());
                    ("added", after.slides[index].title.clone(), true)
                }
                (Some(index), None) => {
                    summary.push("Slide removed".into());
                    ("removed", before.slides[index].title.clone(), true)
                }
                (Some(old_index), Some(new_index)) => {
                    let old_slide = &before.slides[old_index];
                    let new_slide = &after.slides[new_index];
                    if old_slide.title != new_slide.title {
                        summary.push("Headline changed".into());
                    }
                    if old_slide.body != new_slide.body {
                        summary.push("Body changed".into());
                    }
                    if old_slide.eyebrow != new_slide.eyebrow {
                        summary.push("Eyebrow changed".into());
                    }
                    if old_slide.layout != new_slide.layout {
                        summary.push("Layout changed".into());
                    }
                    if old_slide.animation != new_slide.animation {
                        summary.push("Animation changed".into());
                    }
                    if old_slide.images != new_slide.images {
                        summary.push("Pictures changed".into());
                    }
                    if old_slide.notes != new_slide.notes {
                        summary.push("Speaker notes changed".into());
                    }
                    if old_index != new_index {
                        summary.push("Position changed".into());
                    }
                    if shared_style {
                        summary.push("Shared theme or template changed".into());
                    }
                    let can_revert = old_slide != new_slide || old_index != new_index;
                    (
                        if can_revert {
                            "changed"
                        } else {
                            "shared-style"
                        },
                        new_slide.title.clone(),
                        can_revert,
                    )
                }
                (None, None) => return None,
            };
            if summary.is_empty() {
                return None;
            }
            Some(SlideChange {
                id: id.to_owned(),
                title,
                kind: kind.into(),
                before_index: old,
                after_index: new,
                summary,
                can_revert,
            })
        })
        .collect()
}

pub(super) fn describe(
    before: &Deck,
    before_revision: &str,
    after: &Deck,
    current_revision: &str,
) -> serde_json::Value {
    serde_json::json!({
        "deckId": after.id,
        "beforeRevision": before_revision,
        "currentRevision": current_revision,
        "deckTitleChanged": before.title != after.title,
        "themeChanged": before.theme != after.theme,
        "templateChanged": before.template != after.template,
        "changes": changes(before, after),
    })
}

fn image_for(report: &serde_json::Value, id: &str, prefix: &str) -> Option<String> {
    report["slides"]
        .as_array()?
        .iter()
        .find(|slide| slide["slideId"] == id)
        .and_then(|slide| slide["image"].as_str())
        .map(|image| format!("{prefix}/{image}"))
}

fn issues_for(report: &serde_json::Value, id: &str) -> serde_json::Value {
    report["slides"]
        .as_array()
        .and_then(|slides| slides.iter().find(|slide| slide["slideId"] == id))
        .map(|slide| slide["issues"].clone())
        .unwrap_or_else(|| serde_json::json!([]))
}

pub(super) fn render(
    before: &Deck,
    before_revision: &str,
    after: &Deck,
    current_revision: &str,
    directory: &Path,
) -> Result<serde_json::Value, ApiError> {
    let change_list = changes(before, after);
    let existed = directory.exists();
    fs::create_dir_all(directory).map_err(internal)?;
    if !existed {
        fs::set_permissions(directory, fs::Permissions::from_mode(0o700)).map_err(internal)?;
    }
    let directory = fs::canonicalize(directory).map_err(internal)?;
    let before_indices: Vec<usize> = change_list
        .iter()
        .filter_map(|change| change.before_index)
        .collect();
    let after_indices: Vec<usize> = change_list
        .iter()
        .filter_map(|change| change.after_index)
        .collect();
    let old_review = if before_indices.is_empty() {
        None
    } else {
        Some(review::run_selected(
            before,
            before_revision,
            &directory.join("before"),
            before_indices,
        )?)
    };
    let new_review = if after_indices.is_empty() {
        None
    } else {
        Some(review::run_selected(
            after,
            current_revision,
            &directory.join("after"),
            after_indices,
        )?)
    };
    let mut report = describe(before, before_revision, after, current_revision);
    let rendered = change_list
        .iter()
        .map(|change| {
            let mut entry = serde_json::to_value(change).map_err(internal)?;
            entry["beforeImage"] = old_review
                .as_ref()
                .and_then(|review| image_for(review, &change.id, "before"))
                .into();
            entry["afterImage"] = new_review
                .as_ref()
                .and_then(|review| image_for(review, &change.id, "after"))
                .into();
            entry["beforeIssues"] = old_review
                .as_ref()
                .map(|review| issues_for(review, &change.id))
                .unwrap_or_else(|| serde_json::json!([]));
            entry["afterIssues"] = new_review
                .as_ref()
                .map(|review| issues_for(review, &change.id))
                .unwrap_or_else(|| serde_json::json!([]));
            Ok::<_, ApiError>(entry)
        })
        .collect::<Result<Vec<_>, _>>()?;
    report["changes"] = rendered.into();
    let path = directory.join("diff.json");
    write_private(
        &path,
        &serde_json::to_vec_pretty(&report).map_err(internal)?,
    )?;
    report["directory"] = serde_json::json!(directory);
    report["report"] = serde_json::json!(path);
    Ok(report)
}

pub(super) fn revert_slide(
    state: &AppState,
    id: &str,
    historical: &str,
    slide_id: &str,
    expected_current: &str,
) -> Result<serde_json::Value, ApiError> {
    if !valid_id(slide_id) {
        return Err("Invalid slide ID".into());
    }
    let _lock = lock_deck_writes(state)?;
    if deck_revision(state, id)? != expected_current {
        return Err("Deck changed; review its current revision before reverting".into());
    }
    let before = history::load(state, id, historical)?;
    let mut current = read_deck(state, id)?;
    let old_index = before.slides.iter().position(|slide| slide.id == slide_id);
    let current_index = current.slides.iter().position(|slide| slide.id == slide_id);
    let action = match (old_index, current_index) {
        (Some(old), Some(now_index)) => {
            if before.slides[old] == current.slides[now_index] && old == now_index {
                return Err("This slide has no changes to revert".into());
            }
            current.slides.remove(now_index);
            current
                .slides
                .insert(old.min(current.slides.len()), before.slides[old].clone());
            "restored"
        }
        (Some(old), None) => {
            current
                .slides
                .insert(old.min(current.slides.len()), before.slides[old].clone());
            "reinserted"
        }
        (None, Some(now_index)) => {
            if current.slides.len() == 1 {
                return Err("Cannot remove the deck's only slide".into());
            }
            current.slides.remove(now_index);
            "removed"
        }
        (None, None) => return Err("Slide not found in either revision".into()),
    };
    current.updated_at = now();
    let saved = write_deck_unlocked(state, &current)?;
    Ok(serde_json::json!({
        "id": id,
        "slideId": slide_id,
        "action": action,
        "revision": revision(&saved),
    }))
}
