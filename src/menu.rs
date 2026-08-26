//! JSONC menu loading and routing for the native GPUI menu surface.
//!
//! The installed Omarchy menu is an ordered object keyed by dotted ids. This
//! module preserves that order, infers the same parent/kind fields as the
//! reference menu model, and merges the user extension over the default
//! item-by-item.

use std::{collections::BTreeMap, fs, path::PathBuf, process::Command};

use serde_json::Value;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MenuModel {
    pub items: BTreeMap<String, MenuItem>,
    pub order: Vec<String>,
    pub default_path: PathBuf,
    pub user_path: PathBuf,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MenuItem {
    pub id: String,
    pub parent: String,
    pub kind: MenuItemKind,
    pub icon: String,
    pub icon_font: String,
    pub label: String,
    pub title: String,
    pub target: String,
    pub description: String,
    pub action: String,
    pub provider: String,
    pub aliases: Vec<String>,
    pub when: String,
    pub checked: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum MenuItemKind {
    #[default]
    Menu,
    Link,
    Action,
}

impl MenuModel {
    pub fn load() -> Self {
        let omarchy = std::env::var_os("OMARCHY_PATH")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("/usr/share/omarchy"));
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("/tmp"));
        let default_path = omarchy.join("default/omarchy/omarchy-menu.jsonc");
        let user_path = home.join(".config/omarchy/extensions/omarchy-menu.jsonc");
        let default_raw = fs::read_to_string(&default_path).unwrap_or_default();
        let user_raw = fs::read_to_string(&user_path).unwrap_or_default();
        let mut model = Self::from_sources(&default_raw, &user_raw);
        model.default_path = default_path;
        model.user_path = user_path;
        model
    }

    pub fn from_sources(default_raw: &str, user_raw: &str) -> Self {
        let mut model = Self::default();
        for source in [default_raw, user_raw] {
            for item in parse_source(source) {
                if !model.items.contains_key(&item.id) {
                    model.order.push(item.id.clone());
                }
                let merged = match model.items.get(&item.id).cloned() {
                    Some(prior) => prior.merge(item),
                    None => item,
                };
                model.items.insert(merged.id.clone(), merged);
            }
        }
        if !model.items.contains_key("root") {
            model.order.insert(0, "root".to_string());
            model.items.insert(
                "root".to_string(),
                MenuItem {
                    id: "root".to_string(),
                    label: "Go".to_string(),
                    ..Default::default()
                },
            );
        }
        model
    }

    pub fn item(&self, id: &str) -> Option<&MenuItem> {
        self.items.get(id)
    }

    pub fn resolve_route(&self, input: &str) -> String {
        let normalized = normalize_route(input);
        if normalized.is_empty() || normalized == "go" || normalized == "menu" {
            return "root".to_string();
        }
        if self.items.contains_key(&normalized) {
            return normalized;
        }
        self.order
            .iter()
            .filter_map(|id| self.items.get(id))
            .find(|item| {
                item.aliases
                    .iter()
                    .any(|alias| normalize_route(alias) == normalized)
            })
            .map(|item| item.id.clone())
            .unwrap_or(normalized)
    }

    pub fn children(&self, parent: &str) -> Vec<MenuItem> {
        self.order
            .iter()
            .filter_map(|id| self.items.get(id))
            .filter(|item| item.parent == parent)
            .cloned()
            .collect()
    }

    pub fn parent(&self, id: &str) -> Option<String> {
        self.item(id)
            .map(|item| item.parent.clone())
            .filter(|parent| !parent.is_empty())
    }

    pub fn run_action(action: &str) -> Result<(), String> {
        if action.trim().is_empty() {
            return Ok(());
        }
        let status = Command::new("bash")
            .args(["-lc", action])
            .status()
            .map_err(|error| format!("menu action: {error}"))?;
        if status.success() {
            Ok(())
        } else {
            Err(format!("menu action exited with {status}"))
        }
    }

    pub fn evaluate_guard(guard: &str) -> bool {
        if guard.trim().is_empty() {
            return true;
        }
        Command::new("bash")
            .args(["-lc", guard])
            .status()
            .is_ok_and(|status| status.success())
    }
}

impl MenuItem {
    fn merge(self, next: Self) -> Self {
        Self {
            id: next.id,
            parent: next.parent,
            kind: next.kind,
            icon: next.icon,
            icon_font: next.icon_font,
            label: next.label,
            title: next.title,
            target: next.target,
            description: next.description,
            action: next.action,
            provider: next.provider,
            aliases: next.aliases,
            when: next.when,
            checked: next.checked,
        }
    }
}

fn parse_source(raw: &str) -> Vec<MenuItem> {
    let stripped = strip_jsonc(raw);
    let Ok(value) = serde_json::from_str::<Value>(&stripped) else {
        return Vec::new();
    };
    let Some(object) = value.as_object() else {
        return Vec::new();
    };
    let source = object
        .get("items")
        .and_then(Value::as_object)
        .unwrap_or(object);
    source
        .iter()
        .filter_map(|(id, value)| value.as_object().map(|object| normalize_item(id, object)))
        .collect()
}

fn normalize_item(id: &str, object: &serde_json::Map<String, Value>) -> MenuItem {
    let parent = object
        .get("parent")
        .and_then(Value::as_str)
        .map(str::to_string)
        .unwrap_or_else(|| {
            id.rsplit_once('.')
                .map(|(parent, _)| parent.to_string())
                .unwrap_or_else(|| "root".to_string())
        });
    let action = string_field(object, "action");
    let target = string_field(object, "target");
    let kind = if !action.is_empty() {
        MenuItemKind::Action
    } else if !target.is_empty() {
        MenuItemKind::Link
    } else {
        MenuItemKind::Menu
    };
    MenuItem {
        id: id.to_string(),
        parent: if id == "root" { String::new() } else { parent },
        kind,
        icon: string_field(object, "icon"),
        icon_font: string_field(object, "iconFont"),
        label: string_field(object, "label"),
        title: string_field(object, "title"),
        target,
        description: string_field(object, "description"),
        action,
        provider: string_field(object, "provider"),
        aliases: string_list(object.get("aliases")),
        when: string_field(object, "when"),
        checked: string_field(object, "checked"),
    }
}

fn string_field(object: &serde_json::Map<String, Value>, key: &str) -> String {
    object
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

fn string_list(value: Option<&Value>) -> Vec<String> {
    match value {
        Some(Value::Array(values)) => values
            .iter()
            .filter_map(Value::as_str)
            .map(str::to_string)
            .collect(),
        Some(Value::String(value)) if !value.is_empty() => vec![value.clone()],
        _ => Vec::new(),
    }
}

fn normalize_route(value: &str) -> String {
    value.trim().to_lowercase().replace('_', "-")
}

pub fn strip_jsonc(raw: &str) -> String {
    let mut output = String::with_capacity(raw.len());
    let chars = raw.chars().collect::<Vec<_>>();
    let mut index = 0;
    let mut in_string = false;
    let mut escaped = false;
    while index < chars.len() {
        let current = chars[index];
        if in_string {
            output.push(current);
            if escaped {
                escaped = false;
            } else if current == '\\' {
                escaped = true;
            } else if current == '"' {
                in_string = false;
            }
            index += 1;
            continue;
        }
        if current == '"' {
            in_string = true;
            output.push(current);
            index += 1;
        } else if current == '/' && chars.get(index + 1) == Some(&'/') {
            index += 2;
            while chars.get(index).is_some_and(|character| *character != '\n') {
                index += 1;
            }
        } else if current == '/' && chars.get(index + 1) == Some(&'*') {
            index += 2;
            while index + 1 < chars.len() && !(chars[index] == '*' && chars[index + 1] == '/') {
                index += 1;
            }
            index = (index + 2).min(chars.len());
        } else {
            output.push(current);
            index += 1;
        }
    }
    strip_trailing_commas(&output)
}

fn strip_trailing_commas(raw: &str) -> String {
    let chars = raw.chars().collect::<Vec<_>>();
    let mut output = String::with_capacity(raw.len());
    let mut index = 0;
    let mut in_string = false;
    let mut escaped = false;
    while index < chars.len() {
        let current = chars[index];
        if in_string {
            output.push(current);
            if escaped {
                escaped = false;
            } else if current == '\\' {
                escaped = true;
            } else if current == '"' {
                in_string = false;
            }
            index += 1;
            continue;
        }
        if current == '"' {
            in_string = true;
            output.push(current);
            index += 1;
            continue;
        }
        if current == ',' {
            let mut lookahead = index + 1;
            while chars
                .get(lookahead)
                .is_some_and(|character| character.is_whitespace())
            {
                lookahead += 1;
            }
            if matches!(chars.get(lookahead), Some('}') | Some(']')) {
                index += 1;
                continue;
            }
        }
        output.push(current);
        index += 1;
    }
    output
}

#[cfg(test)]
mod tests {
    use super::{MenuItemKind, MenuModel, strip_jsonc};

    #[test]
    fn parses_comments_trailing_commas_and_dotted_parents() {
        let model = MenuModel::from_sources(
            r#"{
                // root
                "system": {"label":"System",},
                "system.lock": {"label":"Lock", "action":"true",}
            }"#,
            "",
        );
        assert_eq!(model.item("system").unwrap().parent, "root");
        assert_eq!(model.item("system.lock").unwrap().parent, "system");
        assert_eq!(
            model.item("system.lock").unwrap().kind,
            MenuItemKind::Action
        );
    }

    #[test]
    fn user_source_overrides_an_item_without_reordering_it() {
        let model = MenuModel::from_sources(
            r#"{"one":{"label":"One"},"two":{"label":"Two"}}"#,
            r#"{"one":{"label":"Updated","aliases":"first"},"three":{"label":"Three"}}"#,
        );
        assert_eq!(model.order, vec!["root", "one", "two", "three"]);
        assert_eq!(model.item("one").unwrap().label, "Updated");
        assert_eq!(model.resolve_route("FIRST"), "one");
    }

    #[test]
    fn comments_inside_strings_survive() {
        let stripped = strip_jsonc(r#"{"url":"https://example.test/a//b"}// end"#);
        assert!(stripped.contains("https://example.test/a//b"));
    }
}
