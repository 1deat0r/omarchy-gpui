//! State models shared by the GPUI overlay surfaces.
//!
//! These functions intentionally mirror the small, deterministic model files
//! shipped with Omarchy. Process and window ownership stays in the renderer;
//! this module owns normalization, filtering, and payload rules so those
//! behaviors can be tested without a Wayland session.

use std::{
    fs,
    path::{Path, PathBuf},
};

use serde_json::Value;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum OverlayAction {
    ClipboardPasteText {
        index: usize,
    },
    ClipboardPasteImage {
        mime: String,
        path: String,
    },
    EmojiInsert(String),
    SelectImage {
        path: String,
        selection_file: String,
        done_file: String,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OverlayRow {
    pub id: String,
    pub label: String,
    pub detail: String,
    pub action: OverlayAction,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ClipboardEntry {
    Text(String),
    Image {
        path: String,
        mime: String,
        captured_at: String,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ImagePickerPayload {
    pub image_dirs: String,
    pub rows: String,
    pub selected_image: String,
    pub selection_file: String,
    pub done_file: String,
    pub show_labels: bool,
    pub filterable: bool,
}

impl Default for ImagePickerPayload {
    fn default() -> Self {
        Self {
            image_dirs: String::new(),
            rows: String::new(),
            selected_image: String::new(),
            selection_file: String::new(),
            done_file: String::new(),
            show_labels: false,
            filterable: false,
        }
    }
}

pub fn clipboard_rows_from_path(path: &Path, filter: &str) -> Vec<OverlayRow> {
    let raw = fs::read_to_string(path).unwrap_or_else(|_| "[]".to_string());
    clipboard_rows(&raw, filter)
}

pub fn clipboard_rows(raw: &str, filter: &str) -> Vec<OverlayRow> {
    let query = filter.trim().to_lowercase();
    parse_clipboard_history(raw)
        .into_iter()
        .enumerate()
        .filter_map(|(index, entry)| {
            let (label, detail, action, searchable) = match entry {
                ClipboardEntry::Text(text) => {
                    let preview = text
                        .lines()
                        .map(str::trim)
                        .filter(|line| !line.is_empty())
                        .collect::<Vec<_>>()
                        .join(" ");
                    let preview = if preview.is_empty() {
                        "Text".to_string()
                    } else {
                        truncate(&preview, 180)
                    };
                    (
                        preview,
                        format!("text #{index}"),
                        OverlayAction::ClipboardPasteText { index },
                        text,
                    )
                }
                ClipboardEntry::Image {
                    path,
                    mime,
                    captured_at,
                } => {
                    let label = if captured_at.is_empty() {
                        "Image".to_string()
                    } else if mime == "image/png" {
                        format!("Screenshot from {captured_at}")
                    } else {
                        format!("Image from {captured_at}")
                    };
                    (
                        label,
                        path.clone(),
                        OverlayAction::ClipboardPasteImage {
                            mime: mime.clone(),
                            path,
                        },
                        format!("{mime} {captured_at}"),
                    )
                }
            };
            if query.is_empty() || searchable.to_lowercase().contains(&query) {
                Some(OverlayRow {
                    id: format!("clipboard-{index}"),
                    label,
                    detail,
                    action,
                })
            } else {
                None
            }
        })
        .take(50)
        .collect()
}

pub fn parse_clipboard_history(raw: &str) -> Vec<ClipboardEntry> {
    let Ok(value) = serde_json::from_str::<Value>(raw) else {
        return Vec::new();
    };
    let Some(entries) = value.as_array() else {
        return Vec::new();
    };
    entries
        .iter()
        .filter_map(normalize_clipboard_entry)
        .collect()
}

fn normalize_clipboard_entry(value: &Value) -> Option<ClipboardEntry> {
    if let Some(text) = value.as_str() {
        return (!text.trim().is_empty()).then(|| ClipboardEntry::Text(text.to_string()));
    }
    let object = value.as_object()?;
    match object
        .get("type")
        .or_else(|| object.get("kind"))
        .and_then(Value::as_str)
    {
        Some("text") => {
            let text = object
                .get("text")
                .and_then(Value::as_str)
                .unwrap_or_default();
            (!text.trim().is_empty()).then(|| ClipboardEntry::Text(text.to_string()))
        }
        Some("image") => {
            let path = object
                .get("path")
                .and_then(Value::as_str)
                .unwrap_or_default();
            if path.is_empty() {
                return None;
            }
            Some(ClipboardEntry::Image {
                path: path.to_string(),
                mime: object
                    .get("mime")
                    .and_then(Value::as_str)
                    .unwrap_or("image/png")
                    .to_string(),
                captured_at: object
                    .get("capturedAt")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
            })
        }
        _ => None,
    }
}

pub fn emoji_rows_from_path(path: &Path, filter: &str) -> Vec<OverlayRow> {
    let raw = fs::read_to_string(path).unwrap_or_else(|_| "[]".to_string());
    emoji_rows(&raw, filter)
}

pub fn emoji_rows(raw: &str, filter: &str) -> Vec<OverlayRow> {
    let Ok(value) = serde_json::from_str::<Value>(raw) else {
        return Vec::new();
    };
    let Some(entries) = value.as_array() else {
        return Vec::new();
    };
    let query = filter.trim().to_lowercase();
    entries
        .iter()
        .enumerate()
        .filter_map(|(index, entry)| {
            let emoji = entry.get("e").and_then(Value::as_str)?;
            let keywords = entry.get("k").and_then(Value::as_str).unwrap_or_default();
            if emoji.is_empty() || (!query.is_empty() && !keywords.to_lowercase().contains(&query))
            {
                return None;
            }
            Some(OverlayRow {
                id: format!("emoji-{index}"),
                label: emoji.to_string(),
                detail: keywords.to_string(),
                action: OverlayAction::EmojiInsert(emoji.to_string()),
            })
        })
        .take(1000)
        .collect()
}

pub fn parse_image_picker_payload(payload: &str) -> ImagePickerPayload {
    let Ok(value) = serde_json::from_str::<Value>(payload) else {
        return ImagePickerPayload::default();
    };
    ImagePickerPayload {
        image_dirs: value
            .get("imageDirs")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        rows: value
            .get("imageRows")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        selected_image: value
            .get("selectedImage")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        selection_file: value
            .get("selectionFile")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        done_file: value
            .get("doneFile")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        show_labels: parse_bool(value.get("showLabels")),
        filterable: parse_bool(value.get("filterable")),
    }
}

pub fn image_rows(
    raw: &str,
    selected_image: &str,
    selection_file: &str,
    done_file: &str,
) -> Vec<OverlayRow> {
    let mut seen = std::collections::BTreeSet::new();
    raw.lines()
        .filter_map(|line| {
            let mut columns = line.split('\t');
            let path = columns.next()?.trim();
            if path.is_empty() {
                return None;
            }
            let file_name = Path::new(path).file_name()?.to_string_lossy().to_string();
            if !seen.insert(file_name) {
                return None;
            }
            let label = image_label(path);
            let selected = path == selected_image;
            Some(OverlayRow {
                id: format!("image-{}", seen.len()),
                label: if selected {
                    format!("✓ {label}")
                } else {
                    label
                },
                detail: path.to_string(),
                action: OverlayAction::SelectImage {
                    path: path.to_string(),
                    selection_file: selection_file.to_string(),
                    done_file: done_file.to_string(),
                },
            })
        })
        .collect()
}

pub fn image_rows_from_payload(payload: &ImagePickerPayload) -> Vec<OverlayRow> {
    image_rows(
        &payload.rows,
        &payload.selected_image,
        &payload.selection_file,
        &payload.done_file,
    )
}

pub fn valid_reminder_minutes(value: &str) -> Option<String> {
    let candidate = value.trim();
    if !candidate.is_empty()
        && candidate
            .chars()
            .all(|character| character.is_ascii_digit())
        && candidate.parse::<u64>().is_ok_and(|minutes| minutes > 0)
    {
        Some(candidate.to_string())
    } else {
        None
    }
}

pub fn reminder_args(minutes: &str, message: &str) -> Option<Vec<String>> {
    let minutes = valid_reminder_minutes(minutes)?;
    let mut args = vec![minutes];
    if !message.is_empty() {
        args.push(message.to_string());
    }
    Some(args)
}

pub fn default_clipboard_history_path() -> PathBuf {
    std::env::var_os("HOME")
        .map(|home| PathBuf::from(home).join(".local/state/omarchy/clipboard-history.json"))
        .unwrap_or_else(|| PathBuf::from("/tmp/omarchy/clipboard-history.json"))
}

fn parse_bool(value: Option<&Value>) -> bool {
    match value {
        Some(Value::Bool(value)) => *value,
        Some(Value::String(value)) => matches!(
            value.trim().to_ascii_lowercase().as_str(),
            "true" | "1" | "yes" | "on"
        ),
        _ => false,
    }
}

fn image_label(path: &str) -> String {
    let name = Path::new(path)
        .file_stem()
        .map(|value| value.to_string_lossy())
        .unwrap_or_default();
    name.replace(['-', '_'], " ")
        .split_whitespace()
        .map(|word| {
            let mut characters = word.chars();
            match characters.next() {
                Some(first) => first.to_uppercase().collect::<String>() + characters.as_str(),
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn truncate(value: &str, max: usize) -> String {
    let mut output = value.chars().take(max).collect::<String>();
    if value.chars().count() > max {
        output.push('…');
    }
    output
}

#[cfg(test)]
mod tests {
    use super::{
        ClipboardEntry, OverlayAction, clipboard_rows, emoji_rows, image_rows,
        parse_clipboard_history, parse_image_picker_payload, reminder_args, valid_reminder_minutes,
    };

    #[test]
    fn normalizes_and_filters_clipboard_history() {
        let history = r#"[{"type":"text","text":"  hello world  "},{"type":"image","path":"/tmp/a.png"},{"type":"bad"}]"#;
        assert_eq!(
            parse_clipboard_history(history),
            vec![
                ClipboardEntry::Text("  hello world  ".to_string()),
                ClipboardEntry::Image {
                    path: "/tmp/a.png".to_string(),
                    mime: "image/png".to_string(),
                    captured_at: String::new(),
                },
            ]
        );
        let rows = clipboard_rows(history, "world");
        assert_eq!(rows.len(), 1);
        assert!(matches!(
            rows[0].action,
            OverlayAction::ClipboardPasteText { index: 0 }
        ));
    }

    #[test]
    fn emoji_search_follows_keyword_matching() {
        let rows = emoji_rows(
            r#"[{"e":"😀","k":"grinning happy"},{"e":"🔥","k":"fire hot"}]"#,
            "happy",
        );
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].label, "😀");
    }

    #[test]
    fn image_rows_deduplicate_file_names_and_preserve_selection() {
        let rows = image_rows(
            "/tmp/one/my-wallpaper.png\t/tmp/thumb.jpg\n/tmp/two/my-wallpaper.png\n/tmp/other.jpg\t/tmp/other.jpg\n",
            "/tmp/other.jpg",
            "/tmp/selection",
            "/tmp/done",
        );
        assert_eq!(rows.len(), 2);
        assert!(rows[1].label.starts_with("✓ "));
    }

    #[test]
    fn image_payload_and_reminder_validation_are_stable() {
        let payload = parse_image_picker_payload(
            r#"{"imageDirs":"/tmp/images","imageRows":"a.png\tthumb.jpg","selectedImage":"a.png","showLabels":"true","filterable":true}"#,
        );
        assert_eq!(payload.image_dirs, "/tmp/images");
        assert_eq!(payload.rows, "a.png\tthumb.jpg");
        assert!(payload.show_labels);
        assert!(payload.filterable);
        assert_eq!(valid_reminder_minutes(" 5 "), Some("5".to_string()));
        assert_eq!(valid_reminder_minutes("0"), None);
        assert_eq!(
            reminder_args("5", "check the oven"),
            Some(vec!["5".to_string(), "check the oven".to_string()])
        );
    }
}
