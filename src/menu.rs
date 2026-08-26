//! JSONC menu loading and routing for the native GPUI menu surface.
//!
//! The installed Omarchy menu is an ordered object keyed by dotted ids. This
//! module preserves that order, infers the same parent/kind fields as the
//! reference menu model, and merges the user extension over the default
//! item-by-item.

use std::{
    collections::BTreeMap,
    env, fs,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

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

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DmenuOption {
    pub icon: String,
    pub label: String,
    pub detail: String,
}

impl DmenuOption {
    pub fn selection_value(&self) -> String {
        if self.detail.is_empty() {
            self.label.clone()
        } else {
            format!("{}\t{}", self.label, self.detail)
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DmenuRequest {
    pub input: bool,
    pub prompt: String,
    pub options: Vec<DmenuOption>,
    pub selection_file: String,
    pub done_file: String,
    pub width: u32,
    pub max_height: u32,
}

impl DmenuRequest {
    pub fn parse(payload: &str) -> Option<Self> {
        let value = serde_json::from_str::<Value>(payload).ok()?;
        let mode = value.get("mode").and_then(Value::as_str)?;
        let input = match mode {
            "input" => true,
            "select" => false,
            _ => return None,
        };
        let prompt = value
            .get("prompt")
            .and_then(Value::as_str)
            .filter(|prompt| !prompt.is_empty())
            .unwrap_or(if input { "Input" } else { "Select" })
            .to_string();
        let options = value
            .get("options")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .map(parse_dmenu_option)
            .collect();
        let selection_file = value
            .get("selectionFile")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let done_file = value
            .get("doneFile")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let width = positive_u32(value.get("width")).unwrap_or(300);
        let max_height = positive_u32(value.get("maxHeight")).unwrap_or(0);
        Some(Self {
            input,
            prompt,
            options,
            selection_file,
            done_file,
            width,
            max_height,
        })
    }

    pub fn result_for(&self, option: Option<&DmenuOption>, input: &str) -> String {
        if self.input {
            input.to_string()
        } else {
            option.map(DmenuOption::selection_value).unwrap_or_default()
        }
    }
}

fn parse_dmenu_option(raw: &str) -> DmenuOption {
    let mut fields = raw.split('\t');
    let first = fields.next().unwrap_or_default();
    let second = fields.next();
    let (icon, label) = match second {
        Some(label) => (first.to_string(), label.to_string()),
        None => (String::new(), first.to_string()),
    };
    DmenuOption {
        icon,
        label,
        detail: fields.collect::<Vec<_>>().join("\t"),
    }
}

fn positive_u32(value: Option<&Value>) -> Option<u32> {
    value
        .and_then(Value::as_f64)
        .filter(|value| value.is_finite() && *value > 0.0)
        .map(|value| value.min(u32::MAX as f64) as u32)
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

    pub fn children_with_providers(&self, parent: &str) -> Vec<MenuItem> {
        let mut children = self.children(parent);
        let Some(provider) = self.item(parent).map(|item| item.provider.as_str()) else {
            return children;
        };
        let mut provided = match provider {
            "apps" => discover_desktop_entries(),
            "fonts" => discover_fonts(),
            _ => Vec::new(),
        };
        children.append(&mut provided);
        children
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
        Command::new("bash")
            .args(["-lc", action])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map(|_| ())
            .map_err(|error| format!("menu action: {error}"))
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

fn discover_desktop_entries() -> Vec<MenuItem> {
    let home = env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/tmp"));
    let data_home = env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join(".local/share"));
    let data_dirs = env::var_os("XDG_DATA_DIRS")
        .and_then(|value| value.into_string().ok())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "/usr/local/share:/usr/share".to_string());

    let mut by_id = BTreeMap::new();
    let mut roots = vec![data_home.join("applications")];
    roots.extend(
        data_dirs
            .split(':')
            .filter(|directory| !directory.is_empty())
            .map(|directory| PathBuf::from(directory).join("applications")),
    );
    for root in roots {
        let Ok(entries) = fs::read_dir(root) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let Some(item) = parse_desktop_entry(&path) else {
                continue;
            };
            by_id.entry(item.id.clone()).or_insert(item);
        }
    }

    let mut items = by_id.into_values().collect::<Vec<_>>();
    items.sort_by(|left, right| {
        left.label
            .to_lowercase()
            .cmp(&right.label.to_lowercase())
            .then_with(|| left.id.cmp(&right.id))
    });
    items
}

fn parse_desktop_entry(path: &Path) -> Option<MenuItem> {
    if path.extension().and_then(|extension| extension.to_str()) != Some("desktop") {
        return None;
    }
    let raw = fs::read_to_string(path).ok()?;
    let mut in_desktop_entry = false;
    let mut fields = BTreeMap::new();
    for line in raw.lines() {
        let line = line.trim();
        if line.starts_with('[') && line.ends_with(']') {
            in_desktop_entry = line == "[Desktop Entry]";
            continue;
        }
        if !in_desktop_entry || line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        fields.insert(key.trim().to_string(), value.trim().to_string());
    }
    if fields
        .get("Type")
        .is_some_and(|value| value != "Application")
        || fields
            .get("Hidden")
            .is_some_and(|value| value.eq_ignore_ascii_case("true"))
        || fields
            .get("NoDisplay")
            .is_some_and(|value| value.eq_ignore_ascii_case("true"))
    {
        return None;
    }
    let label = fields.get("Name")?.clone();
    if label.is_empty() {
        return None;
    }
    let id = path.file_stem()?.to_str()?.to_string();
    let mut aliases = fields
        .get("Keywords")
        .into_iter()
        .flat_map(|keywords| keywords.split(';'))
        .filter(|keyword| !keyword.is_empty())
        .map(str::to_string)
        .collect::<Vec<_>>();
    if let Some(generic_name) = fields.get("GenericName")
        && !generic_name.is_empty()
    {
        aliases.push(generic_name.clone());
    }
    Some(MenuItem {
        id: format!("apps.{id}"),
        parent: "apps".to_string(),
        kind: MenuItemKind::Action,
        label: label.clone(),
        description: fields.get("Comment").cloned().unwrap_or_default(),
        action: format!(
            "uwsm-app -- gtk-launch {}",
            shell_quote(&format!("{id}.desktop"))
        ),
        aliases,
        ..Default::default()
    })
}

fn discover_fonts() -> Vec<MenuItem> {
    let Ok(raw) = Command::new("omarchy-font-list").output() else {
        return Vec::new();
    };
    if !raw.status.success() {
        return Vec::new();
    }
    let current = Command::new("omarchy-font-current")
        .output()
        .ok()
        .filter(|output| output.status.success())
        .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_string())
        .unwrap_or_default();
    String::from_utf8_lossy(&raw.stdout)
        .lines()
        .map(str::trim)
        .filter(|font| !font.is_empty())
        .map(|font| MenuItem {
            id: format!("style.font.{}", slugify(font)),
            parent: "style.font".to_string(),
            kind: MenuItemKind::Action,
            icon: "".to_string(),
            label: font.to_string(),
            action: format!("omarchy-font-set {}", shell_quote(font)),
            checked: if current == font {
                "true".to_string()
            } else {
                String::new()
            },
            ..Default::default()
        })
        .collect()
}

fn slugify(value: &str) -> String {
    let mut slug = String::new();
    for character in value.chars() {
        if character.is_ascii_alphanumeric() {
            slug.push(character.to_ascii_lowercase());
        } else if !slug.ends_with('-') {
            slug.push('-');
        }
    }
    slug.trim_matches('-').to_string()
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
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
    use std::{
        env, fs,
        time::{SystemTime, UNIX_EPOCH},
    };

    use super::{
        DmenuRequest, MenuItemKind, MenuModel, parse_desktop_entry, shell_quote, strip_jsonc,
    };

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
    fn dmenu_request_preserves_display_fields_and_result_contract() {
        let request = DmenuRequest::parse(
            r#"{"mode":"select","prompt":"Format","options":["jpg","🖼\tPNG\tPortable Network Graphics"],"selectionFile":"/tmp/selection","doneFile":"/tmp/done","width":400,"maxHeight":500}"#,
        )
        .expect("select request");
        assert_eq!(request.prompt, "Format");
        assert_eq!(request.width, 400);
        assert_eq!(request.max_height, 500);
        assert_eq!(request.options[0].selection_value(), "jpg");
        assert_eq!(request.options[1].icon, "🖼");
        assert_eq!(
            request.options[1].selection_value(),
            "PNG\tPortable Network Graphics"
        );

        let input =
            DmenuRequest::parse(r#"{"mode":"input","prompt":"Name"}"#).expect("input request");
        assert_eq!(input.result_for(None, "new name"), "new name");
    }

    #[test]
    fn comments_inside_strings_survive() {
        let stripped = strip_jsonc(r#"{"url":"https://example.test/a//b"}// end"#);
        assert!(stripped.contains("https://example.test/a//b"));
    }

    #[test]
    fn preserves_source_order_for_root_menu_entries() {
        let model = MenuModel::from_sources(
            r#"{"zeta":{"label":"Zeta"},"alpha":{"label":"Alpha"},"middle":{"label":"Middle"}}"#,
            "",
        );
        assert_eq!(
            model
                .children("root")
                .iter()
                .map(|item| item.id.as_str())
                .collect::<Vec<_>>(),
            vec!["zeta", "alpha", "middle"]
        );
    }

    #[test]
    fn parses_desktop_application_metadata_and_builds_safe_launcher() {
        let suffix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock before epoch")
            .as_nanos();
        let directory =
            env::temp_dir().join(format!("omarchy-gpui-menu-{}-{suffix}", std::process::id()));
        fs::create_dir(&directory).expect("create desktop fixture directory");
        let path = directory.join("example.desktop");
        fs::write(
            &path,
            "[Desktop Entry]\nType=Application\nName=Example App\nGenericName=Demo\nKeywords=alpha;beta;\nComment=Example description\nExec=ignored %U\n",
        )
        .expect("write desktop fixture");

        let item = parse_desktop_entry(&path).expect("parse desktop fixture");
        assert_eq!(item.id, "apps.example");
        assert_eq!(item.parent, "apps");
        assert_eq!(item.label, "Example App");
        assert_eq!(item.description, "Example description");
        assert_eq!(item.aliases, vec!["alpha", "beta", "Demo"]);
        assert_eq!(item.action, "uwsm-app -- gtk-launch 'example.desktop'");
        fs::remove_dir_all(directory).expect("remove desktop fixture directory");
    }

    #[test]
    fn ignores_hidden_and_non_application_desktop_entries() {
        let suffix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock before epoch")
            .as_nanos();
        let hidden = env::temp_dir().join(format!(
            "omarchy-gpui-menu-hidden-{}-{suffix}.desktop",
            std::process::id()
        ));
        fs::write(
            &hidden,
            "[Desktop Entry]\nType=Application\nName=Hidden\nHidden=true\n",
        )
        .expect("write hidden fixture");
        assert!(parse_desktop_entry(&hidden).is_none());
        fs::remove_file(hidden).expect("remove hidden fixture");

        let link = env::temp_dir().join(format!(
            "omarchy-gpui-menu-link-{}-{suffix}.desktop",
            std::process::id()
        ));
        fs::write(&link, "[Desktop Entry]\nType=Link\nName=Link\n").expect("write link fixture");
        assert!(parse_desktop_entry(&link).is_none());
        fs::remove_file(link).expect("remove link fixture");
    }

    #[test]
    fn quotes_shell_values_without_interpreting_single_quotes() {
        assert_eq!(shell_quote("font 'special'"), "'font '\\''special'\\'''");
    }
}
