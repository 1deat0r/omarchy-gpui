use std::{
    collections::BTreeSet,
    env,
    fs::{self, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use serde_json::Value;

const BUILTIN_DEFAULT: &str = include_str!("../assets/default-shell.json");

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ConfigSource {
    User,
    OmarchyDefault,
    Builtin,
}

impl ConfigSource {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::User => "user",
            Self::OmarchyDefault => "omarchy-default",
            Self::Builtin => "builtin",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PluginSource {
    FirstParty,
    User,
}

impl PluginSource {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::FirstParty => "first-party",
            Self::User => "user",
        }
    }
}

#[derive(Clone, Debug)]
pub struct PluginManifest {
    pub id: String,
    pub name: String,
    pub version: String,
    pub kinds: Vec<String>,
    pub entry_points: std::collections::BTreeMap<String, String>,
    pub source_dir: PathBuf,
    pub source: PluginSource,
    pub raw: Value,
}

impl PluginManifest {
    pub fn has_kind(&self, kind: &str) -> bool {
        self.kinds.iter().any(|candidate| candidate == kind)
    }
}

#[derive(Clone, Debug)]
pub struct BarEntry {
    pub id: String,
    pub settings: Value,
}

#[derive(Clone, Debug)]
pub struct ShellSnapshot {
    pub config: Value,
    pub source: ConfigSource,
    pub omarchy_path: PathBuf,
    pub defaults_path: PathBuf,
    pub user_config_path: PathBuf,
    pub bar_position: String,
    pub transparent: bool,
    pub left: Vec<BarEntry>,
    pub center: Vec<BarEntry>,
    pub right: Vec<BarEntry>,
    pub plugin_ids: Vec<String>,
    pub plugins: Vec<PluginManifest>,
}

impl ShellSnapshot {
    pub fn load() -> Self {
        let omarchy_path = env::var_os("OMARCHY_PATH")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("/usr/share/omarchy"));
        let home = env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("/tmp"));
        Self::load_from_paths(&omarchy_path, &home)
    }

    pub fn load_from_paths(omarchy_path: &Path, home: &Path) -> Self {
        let defaults_path = omarchy_path.join("config/omarchy/shell.json");
        let user_config_path = home.join(".config/omarchy/shell.json");

        let builtin = parse_versioned(BUILTIN_DEFAULT)
            .expect("the bundled Omarchy GPUI fallback must be valid version-1 JSON");
        let (defaults, default_source) = fs::read_to_string(&defaults_path)
            .ok()
            .and_then(|raw| parse_versioned(&raw))
            .map(|config| (config, ConfigSource::OmarchyDefault))
            .unwrap_or((builtin, ConfigSource::Builtin));

        let (config, source) = fs::read_to_string(&user_config_path)
            .ok()
            .and_then(|raw| parse_versioned(&raw))
            .map(|config| (config, ConfigSource::User))
            .unwrap_or((defaults, default_source));
        let plugins = discover_plugins(omarchy_path, home);

        Self::from_config(
            config,
            source,
            omarchy_path.to_path_buf(),
            defaults_path,
            user_config_path,
            plugins,
        )
    }

    fn from_config(
        config: Value,
        source: ConfigSource,
        omarchy_path: PathBuf,
        defaults_path: PathBuf,
        user_config_path: PathBuf,
        plugins: Vec<PluginManifest>,
    ) -> Self {
        let left = entries_for(&config, "left");
        let center = entries_for(&config, "center");
        let right = entries_for(&config, "right");

        let mut ids = BTreeSet::new();
        for entry in left.iter().chain(center.iter()).chain(right.iter()) {
            ids.insert(entry.id.clone());
        }
        if let Some(plugins) = config.get("plugins").and_then(Value::as_array) {
            for plugin in plugins {
                if let Some(id) = plugin.get("id").and_then(Value::as_str) {
                    ids.insert(id.to_string());
                }
            }
        }
        for plugin in &plugins {
            ids.insert(plugin.id.clone());
        }

        let bar = config.get("bar");
        let bar_position = bar
            .and_then(|bar| bar.get("position"))
            .and_then(Value::as_str)
            .unwrap_or("top")
            .to_string();
        let transparent = bar
            .and_then(|bar| bar.get("transparent"))
            .and_then(Value::as_bool)
            .unwrap_or(false);

        Self {
            config,
            source,
            omarchy_path,
            defaults_path,
            user_config_path,
            bar_position,
            transparent,
            left,
            center,
            right,
            plugin_ids: ids.into_iter().collect(),
            plugins,
        }
    }

    pub fn plugin(&self, id: &str) -> Option<&PluginManifest> {
        self.plugins.iter().find(|plugin| plugin.id == id)
    }

    pub fn plugin_is_enabled(&self, id: &str) -> bool {
        let Some(plugin) = self.plugin(id) else {
            return self.plugin_ids.iter().any(|known| known == id);
        };

        if plugin.has_kind("bar") {
            let selected = self
                .config
                .get("bar")
                .and_then(|bar| bar.get("id"))
                .and_then(Value::as_str)
                .filter(|value| !value.is_empty())
                .unwrap_or("omarchy.bar");
            return selected == id;
        }

        if self
            .config
            .get("disabledPlugins")
            .and_then(Value::as_array)
            .is_some_and(|disabled| disabled.iter().any(|entry| entry.as_str() == Some(id)))
        {
            return false;
        }

        if plugin.source == PluginSource::FirstParty {
            return true;
        }

        config_contains_plugin(&self.config, id)
    }

    pub fn reload(&mut self) {
        let Some(home) = self
            .user_config_path
            .parent()
            .and_then(Path::parent)
            .and_then(Path::parent)
        else {
            return;
        };
        *self = Self::load_from_paths(&self.omarchy_path, home);
    }

    pub fn toggle_bar_transparency(&mut self) -> Result<(), String> {
        let mut next = self.config.clone();
        ensure_config_shape(&mut next)?;
        let bar = next
            .get_mut("bar")
            .and_then(Value::as_object_mut)
            .ok_or_else(|| "bar config is not an object".to_string())?;
        let current = bar
            .get("transparent")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        bar.insert("transparent".to_string(), Value::Bool(!current));
        self.persist_config(next)
    }

    pub fn set_plugin_enabled(
        &mut self,
        id: &str,
        enabled: bool,
        placement: Option<&Value>,
    ) -> Result<bool, String> {
        let manifest = self.plugin(id).cloned();
        if enabled && manifest.is_none() {
            return Ok(false);
        }

        let mut next = self.config.clone();
        ensure_config_shape(&mut next)?;
        let (is_bar, is_widget, is_first_party, cloned_from, default_section) = manifest
            .as_ref()
            .map(|manifest| {
                (
                    manifest.has_kind("bar"),
                    manifest.has_kind("bar-widget"),
                    manifest.source == PluginSource::FirstParty,
                    manifest
                        .raw
                        .get("omarchy")
                        .and_then(|metadata| metadata.get("clonedFrom"))
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string(),
                    manifest
                        .raw
                        .get("barWidget")
                        .and_then(|bar| bar.get("defaultSection"))
                        .and_then(Value::as_str)
                        .filter(|section| matches!(*section, "left" | "center" | "right"))
                        .unwrap_or("center")
                        .to_string(),
                )
            })
            .unwrap_or((false, false, false, String::new(), "center".to_string()));

        if is_bar {
            let bar = next
                .get_mut("bar")
                .and_then(Value::as_object_mut)
                .ok_or_else(|| "bar config is not an object".to_string())?;
            if enabled {
                bar.insert("id".to_string(), Value::String(id.to_string()));
            } else if bar.get("id").and_then(Value::as_str) == Some(id) {
                if cloned_from.is_empty() || cloned_from == "omarchy.bar" {
                    bar.remove("id");
                } else {
                    bar.insert("id".to_string(), Value::String(cloned_from));
                }
            }
            self.persist_config(next)?;
            return Ok(true);
        }

        if enabled {
            remove_disabled(&mut next, id);
            if is_widget {
                if bar_location(&next, id).is_none() {
                    let entry = Value::Object(
                        [("id".to_string(), Value::String(id.to_string()))]
                            .into_iter()
                            .collect(),
                    );
                    insert_bar_entry(&mut next, entry, default_section.as_str(), placement)?;
                }
            } else if !is_first_party && !plugin_list_contains(&next, id) {
                next.get_mut("plugins")
                    .and_then(Value::as_array_mut)
                    .ok_or_else(|| "plugins config is not an array".to_string())?
                    .push(Value::Object(
                        [("id".to_string(), Value::String(id.to_string()))]
                            .into_iter()
                            .collect(),
                    ));
            }
        } else {
            remove_bar_entry(&mut next, id);
            remove_plugin_entry(&mut next, id);
            if is_first_party && !is_widget {
                add_disabled(&mut next, id);
            }
        }

        self.persist_config(next)?;
        Ok(true)
    }

    pub fn put_bar_widget(
        &mut self,
        id: &str,
        placement: Option<&Value>,
    ) -> Result<String, String> {
        if bar_location(&self.config, id).is_some() {
            return Ok(String::new());
        }
        if self.plugin(id).is_none() {
            return Ok("unknown".to_string());
        }
        self.set_plugin_enabled(id, true, placement)?;
        Ok(String::new())
    }

    pub fn move_bar_widget(&mut self, id: &str, placement: &Value) -> Result<String, String> {
        let Some((section, index)) = bar_location(&self.config, id) else {
            return Ok(format!("could not find widget {id}"));
        };
        let mut next = self.config.clone();
        ensure_config_shape(&mut next)?;
        let entry = next
            .get_mut("bar")
            .and_then(Value::as_object_mut)
            .and_then(|bar| bar.get_mut("layout"))
            .and_then(Value::as_object_mut)
            .and_then(|layout| layout.get_mut(&section))
            .and_then(Value::as_array_mut)
            .and_then(|entries| (index < entries.len()).then(|| entries.remove(index)))
            .ok_or_else(|| format!("could not find widget {id}"))?;
        insert_bar_entry(&mut next, entry, &section, Some(placement))?;
        self.persist_config(next)?;
        Ok(String::new())
    }

    pub fn set_bar_widget(
        &mut self,
        id: &str,
        key: &str,
        value: Value,
        selector: Option<&Value>,
    ) -> Result<String, String> {
        let Some((section, index)) = selected_bar_location(&self.config, id, selector) else {
            return Ok(format!("could not find widget {id}"));
        };
        let mut next = self.config.clone();
        let entry = next
            .get_mut("bar")
            .and_then(Value::as_object_mut)
            .and_then(|bar| bar.get_mut("layout"))
            .and_then(Value::as_object_mut)
            .and_then(|layout| layout.get_mut(&section))
            .and_then(Value::as_array_mut)
            .and_then(|entries| entries.get_mut(index))
            .and_then(Value::as_object_mut)
            .ok_or_else(|| format!("could not find widget {id}"))?;
        entry.insert(key.to_string(), value);
        self.persist_config(next)?;
        Ok(String::new())
    }

    fn persist_config(&mut self, mut config: Value) -> Result<(), String> {
        let object = config
            .as_object_mut()
            .ok_or_else(|| "shell config must be an object".to_string())?;
        object.insert("version".to_string(), Value::from(1));
        let parent = self
            .user_config_path
            .parent()
            .ok_or_else(|| "user config has no parent directory".to_string())?;
        fs::create_dir_all(parent).map_err(|error| format!("create config directory: {error}"))?;
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let temp_path = parent.join(format!(".shell.json.{}.{}.tmp", std::process::id(), stamp));
        let result = (|| {
            let mut file = OpenOptions::new()
                .create_new(true)
                .write(true)
                .open(&temp_path)
                .map_err(|error| format!("open temporary config: {error}"))?;
            let bytes = serde_json::to_vec_pretty(&config)
                .map_err(|error| format!("serialize shell config: {error}"))?;
            file.write_all(&bytes)
                .and_then(|_| file.write_all(b"\n"))
                .and_then(|_| file.sync_all())
                .map_err(|error| format!("write shell config: {error}"))?;
            fs::rename(&temp_path, &self.user_config_path)
                .map_err(|error| format!("replace shell config: {error}"))?;
            Ok::<(), String>(())
        })();
        if result.is_err() {
            let _ = fs::remove_file(&temp_path);
        }
        result?;
        self.reload();
        Ok(())
    }
}

fn ensure_config_shape(config: &mut Value) -> Result<(), String> {
    let object = config
        .as_object_mut()
        .ok_or_else(|| "shell config must be an object".to_string())?;
    if !object.get("bar").is_some_and(Value::is_object) {
        object.insert(
            "bar".to_string(),
            serde_json::json!({
                "layout": {"left": [], "center": [], "right": []}
            }),
        );
    }
    let bar = object
        .get_mut("bar")
        .and_then(Value::as_object_mut)
        .ok_or_else(|| "bar config is not an object".to_string())?;
    if !bar.get("layout").is_some_and(Value::is_object) {
        bar.insert(
            "layout".to_string(),
            serde_json::json!({"left": [], "center": [], "right": []}),
        );
    }
    let layout = bar
        .get_mut("layout")
        .and_then(Value::as_object_mut)
        .ok_or_else(|| "bar layout is not an object".to_string())?;
    for section in ["left", "center", "right"] {
        if !layout.get(section).is_some_and(Value::is_array) {
            layout.insert(section.to_string(), Value::Array(Vec::new()));
        }
    }
    if !object.get("plugins").is_some_and(Value::is_array) {
        object.insert("plugins".to_string(), Value::Array(Vec::new()));
    }
    Ok(())
}

fn bar_location(config: &Value, id: &str) -> Option<(String, usize)> {
    for section in ["left", "center", "right"] {
        let Some(entries) = config
            .get("bar")
            .and_then(|bar| bar.get("layout"))
            .and_then(|layout| layout.get(section))
            .and_then(Value::as_array)
        else {
            continue;
        };
        for (index, entry) in entries.iter().enumerate() {
            if entry.get("id").and_then(Value::as_str) == Some(id) {
                return Some((section.to_string(), index));
            }
        }
    }
    None
}

fn selected_bar_location(
    config: &Value,
    id: &str,
    selector: Option<&Value>,
) -> Option<(String, usize)> {
    let selector = selector.and_then(Value::as_object);
    let section = selector
        .and_then(|selector| {
            selector
                .get("fromSection")
                .or_else(|| selector.get("section"))
        })
        .and_then(Value::as_str);
    let index = selector
        .and_then(|selector| selector.get("fromIndex").or_else(|| selector.get("index")))
        .and_then(Value::as_i64);
    if let (Some(section), Some(index)) = (section, index)
        && matches!(section, "left" | "center" | "right")
        && index >= 0
    {
        let index = index as usize;
        let entries = config
            .get("bar")
            .and_then(|bar| bar.get("layout"))
            .and_then(|layout| layout.get(section))
            .and_then(Value::as_array)?;
        return (index < entries.len()
            && entries[index].get("id").and_then(Value::as_str) == Some(id))
        .then(|| (section.to_string(), index));
    }
    bar_location(config, id)
}

fn plugin_list_contains(config: &Value, id: &str) -> bool {
    config
        .get("plugins")
        .and_then(Value::as_array)
        .is_some_and(|plugins| {
            plugins
                .iter()
                .any(|plugin| plugin.get("id").and_then(Value::as_str) == Some(id))
        })
}

fn remove_bar_entry(config: &mut Value, id: &str) {
    let Some((section, index)) = bar_location(config, id) else {
        return;
    };
    if let Some(entries) = config
        .get_mut("bar")
        .and_then(Value::as_object_mut)
        .and_then(|bar| bar.get_mut("layout"))
        .and_then(Value::as_object_mut)
        .and_then(|layout| layout.get_mut(&section))
        .and_then(Value::as_array_mut)
        && index < entries.len()
    {
        entries.remove(index);
    }
}

fn remove_plugin_entry(config: &mut Value, id: &str) {
    if let Some(plugins) = config.get_mut("plugins").and_then(Value::as_array_mut)
        && let Some(index) = plugins
            .iter()
            .position(|plugin| plugin.get("id").and_then(Value::as_str) == Some(id))
    {
        plugins.remove(index);
    }
}

fn remove_disabled(config: &mut Value, id: &str) {
    let Some(disabled) = config
        .get_mut("disabledPlugins")
        .and_then(Value::as_array_mut)
    else {
        return;
    };
    disabled.retain(|entry| entry.as_str() != Some(id));
    if disabled.is_empty() {
        config
            .as_object_mut()
            .expect("config object was checked before mutation")
            .remove("disabledPlugins");
    }
}

fn add_disabled(config: &mut Value, id: &str) {
    let object = config
        .as_object_mut()
        .expect("config object was checked before mutation");
    let disabled = object
        .entry("disabledPlugins")
        .or_insert_with(|| Value::Array(Vec::new()))
        .as_array_mut()
        .expect("disabledPlugins must be an array");
    if !disabled.iter().any(|entry| entry.as_str() == Some(id)) {
        disabled.push(Value::String(id.to_string()));
    }
}

fn insert_bar_entry(
    config: &mut Value,
    entry: Value,
    fallback_section: &str,
    placement: Option<&Value>,
) -> Result<(), String> {
    let placement = placement.and_then(Value::as_object);
    let section = placement
        .and_then(|placement| placement.get("section"))
        .and_then(Value::as_str)
        .filter(|section| matches!(*section, "left" | "center" | "right"))
        .unwrap_or(fallback_section);

    let index = if let Some(relative_id) = placement
        .and_then(|placement| placement.get("before").or_else(|| placement.get("after")))
        .and_then(Value::as_str)
    {
        let (relative_section, relative_index) = bar_location(config, relative_id)
            .ok_or_else(|| format!("could not find target widget {relative_id}"))?;
        if placement
            .and_then(|placement| placement.get("section"))
            .and_then(Value::as_str)
            .is_some_and(|requested| requested != relative_section)
        {
            return Err(format!(
                "target widget {relative_id} is not in section {section}"
            ));
        }
        relative_index
            + usize::from(
                placement
                    .and_then(|placement| placement.get("after"))
                    .is_some(),
            )
    } else if let Some(requested) = placement
        .and_then(|placement| placement.get("index"))
        .and_then(Value::as_i64)
    {
        requested.max(0) as usize
    } else {
        let anchor = match section {
            "left" => "omarchy.workspaces",
            "center" => "omarchy.weather",
            "right" => "omarchy.tray",
            _ => "",
        };
        bar_location(config, anchor)
            .filter(|(anchor_section, _)| anchor_section == section)
            .map(|(_, index)| index + 1)
            .unwrap_or(usize::MAX)
    };

    let entries = config
        .get_mut("bar")
        .and_then(Value::as_object_mut)
        .and_then(|bar| bar.get_mut("layout"))
        .and_then(Value::as_object_mut)
        .and_then(|layout| layout.get_mut(section))
        .and_then(Value::as_array_mut)
        .ok_or_else(|| format!("bar section {section} is not an array"))?;
    let index = index.min(entries.len());
    entries.insert(index, entry);
    Ok(())
}

fn config_contains_plugin(config: &Value, id: &str) -> bool {
    let in_layout = ["left", "center", "right"].iter().any(|section| {
        config
            .get("bar")
            .and_then(|bar| bar.get("layout"))
            .and_then(|layout| layout.get(section))
            .and_then(Value::as_array)
            .is_some_and(|entries| {
                entries
                    .iter()
                    .any(|entry| entry.get("id").and_then(Value::as_str) == Some(id))
            })
    });
    let in_plugins = config
        .get("plugins")
        .and_then(Value::as_array)
        .is_some_and(|plugins| {
            plugins
                .iter()
                .any(|plugin| plugin.get("id").and_then(Value::as_str) == Some(id))
        });
    let selected_bar = config
        .get("bar")
        .and_then(|bar| bar.get("id"))
        .and_then(Value::as_str)
        == Some(id);
    in_layout || in_plugins || selected_bar
}

fn discover_plugins(omarchy_path: &Path, home: &Path) -> Vec<PluginManifest> {
    let first_party_root = omarchy_path.join("shell/plugins");
    let user_root = home.join(".config/omarchy/plugins");
    let mut by_id = std::collections::BTreeMap::<String, PluginManifest>::new();

    for path in manifest_paths(&first_party_root, true) {
        if let Some(manifest) = read_manifest(&path, PluginSource::FirstParty) {
            by_id.insert(manifest.id.clone(), manifest);
        }
    }

    // The reference shell reserves the complete `omarchy.*` namespace and
    // never lets a user checkout shadow a first-party manifest.
    for path in manifest_paths(&user_root, false) {
        if let Some(manifest) = read_manifest(&path, PluginSource::User)
            && !manifest.id.starts_with("omarchy.")
            && !by_id.contains_key(&manifest.id)
        {
            by_id.insert(manifest.id.clone(), manifest);
        }
    }

    by_id.into_values().collect()
}

fn manifest_paths(root: &Path, recursive: bool) -> Vec<PathBuf> {
    if !root.is_dir() {
        return Vec::new();
    }

    if !recursive {
        let Ok(entries) = fs::read_dir(root) else {
            return Vec::new();
        };
        let mut paths = entries
            .filter_map(Result::ok)
            .filter_map(|entry| {
                if !entry.file_type().ok()?.is_dir() {
                    return None;
                }
                let manifest = entry.path().join("manifest.json");
                manifest.is_file().then_some(manifest)
            })
            .collect::<Vec<_>>();
        paths.sort();
        return paths;
    }

    let mut pending = vec![root.to_path_buf()];
    let mut paths = Vec::new();
    while let Some(dir) = pending.pop() {
        let Ok(entries) = fs::read_dir(dir) else {
            continue;
        };
        let mut entries = entries.filter_map(Result::ok).collect::<Vec<_>>();
        entries.sort_by_key(|entry| entry.file_name());
        for entry in entries {
            let path = entry.path();
            let Ok(file_type) = entry.file_type() else {
                continue;
            };
            if file_type.is_dir() && recursive {
                pending.push(path);
            } else if file_type.is_file() && is_manifest_path(&path) {
                paths.push(path);
            }
        }
    }
    paths.sort();
    paths
}

fn is_manifest_path(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name == "manifest.json" || name.ends_with(".manifest.json"))
}

fn read_manifest(path: &Path, source: PluginSource) -> Option<PluginManifest> {
    let raw = fs::read_to_string(path).ok()?;
    let value = serde_json::from_str::<Value>(&raw).ok()?;
    validate_manifest(value, path.parent()?, source)
}

fn validate_manifest(
    value: Value,
    source_dir: &Path,
    source: PluginSource,
) -> Option<PluginManifest> {
    let object = value.as_object()?;
    if object.get("schemaVersion").and_then(Value::as_u64) != Some(1) {
        return None;
    }

    let id = object.get("id").and_then(Value::as_str)?.to_string();
    if id.is_empty() || id.starts_with('/') || id.contains('/') || id.contains("..") {
        return None;
    }
    let name = object.get("name").and_then(Value::as_str)?.to_string();
    let version = object.get("version").and_then(Value::as_str)?.to_string();
    let kinds = object
        .get("kinds")
        .and_then(Value::as_array)?
        .iter()
        .map(Value::as_str)
        .collect::<Option<Vec<_>>>()?
        .into_iter()
        .map(str::to_string)
        .collect::<Vec<_>>();
    if kinds.is_empty() {
        return None;
    }

    let entry_points = object
        .get("entryPoints")
        .and_then(Value::as_object)?
        .iter()
        .map(|(key, value)| {
            let value = value.as_str()?;
            if value.is_empty() || value.starts_with('/') || value.contains("..") {
                return None;
            }
            Some((key.clone(), value.to_string()))
        })
        .collect::<Option<std::collections::BTreeMap<_, _>>>()?;

    if let Some(default_section) = object
        .get("barWidget")
        .and_then(|bar| bar.get("defaultSection"))
        .and_then(Value::as_str)
        && !matches!(default_section, "left" | "center" | "right")
    {
        return None;
    }

    Some(PluginManifest {
        id,
        name,
        version,
        kinds,
        entry_points,
        source_dir: source_dir.to_path_buf(),
        source,
        raw: value,
    })
}

fn parse_versioned(raw: &str) -> Option<Value> {
    let value = serde_json::from_str::<Value>(raw).ok()?;
    (value.get("version").and_then(Value::as_u64) == Some(1)).then_some(value)
}

fn entries_for(config: &Value, section: &str) -> Vec<BarEntry> {
    config
        .get("bar")
        .and_then(|bar| bar.get("layout"))
        .and_then(|layout| layout.get(section))
        .and_then(Value::as_array)
        .map(|entries| {
            entries
                .iter()
                .filter_map(|entry| {
                    let id = entry.get("id").and_then(Value::as_str)?.to_string();
                    Some(BarEntry {
                        id,
                        settings: entry.clone(),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use std::{fs, path::PathBuf};

    use serde_json::json;

    use super::{ConfigSource, PluginSource, ShellSnapshot, parse_versioned};

    #[test]
    fn only_version_one_is_accepted() {
        assert!(parse_versioned(r#"{"version":1}"#).is_some());
        assert!(parse_versioned(r#"{"version":2}"#).is_none());
        assert!(parse_versioned(r#"{"bar":{}}"#).is_none());
    }

    #[test]
    fn user_config_has_precedence_without_merging() {
        let defaults = json!({"version": 1, "bar": {"position": "top"}});
        let user = json!({"version": 1, "bar": {"position": "bottom"}});
        let selected = if user.get("version").and_then(|v| v.as_u64()) == Some(1) {
            (user, ConfigSource::User)
        } else {
            (defaults, ConfigSource::OmarchyDefault)
        };
        assert_eq!(selected.1, ConfigSource::User);
        assert_eq!(selected.0["bar"]["position"], "bottom");
        assert!(selected.0["bar"].get("missing-default-field").is_none());
    }

    #[test]
    fn plugin_registry_discovers_first_party_and_user_plugins() {
        let root = test_root("registry");
        let first_party = root.join("omarchy/shell/plugins/panels/audio");
        let user_plugin = root.join("home/.config/omarchy/plugins/example.clock");
        let reserved_user_plugin = root.join("home/.config/omarchy/plugins/omarchy.fake");
        fs::create_dir_all(&first_party).expect("create first-party fixture");
        fs::create_dir_all(&user_plugin).expect("create user fixture");
        fs::create_dir_all(&reserved_user_plugin).expect("create reserved fixture");
        fs::write(
            first_party.join("manifest.json"),
            r#"{
              "schemaVersion": 1,
              "id": "omarchy.audio",
              "name": "Audio",
              "version": "1.0.0",
              "kinds": ["bar-widget"],
              "entryPoints": {"barWidget": "Panel.qml"}
            }"#,
        )
        .expect("write first-party fixture");
        fs::write(
            user_plugin.join("manifest.json"),
            r#"{
              "schemaVersion": 1,
              "id": "example.clock",
              "name": "Example Clock",
              "version": "1.0.0",
              "kinds": ["bar-widget"],
              "entryPoints": {"barWidget": "Widget.qml"}
            }"#,
        )
        .expect("write user fixture");
        fs::write(
            reserved_user_plugin.join("manifest.json"),
            r#"{
              "schemaVersion": 1,
              "id": "omarchy.fake",
              "name": "Reserved",
              "version": "1.0.0",
              "kinds": ["bar-widget"],
              "entryPoints": {"barWidget": "Widget.qml"}
            }"#,
        )
        .expect("write reserved fixture");

        let snapshot = ShellSnapshot::load_from_paths(&root.join("omarchy"), &root.join("home"));
        assert_eq!(
            snapshot.plugin("omarchy.audio").unwrap().source,
            PluginSource::FirstParty
        );
        assert_eq!(
            snapshot.plugin("example.clock").unwrap().source,
            PluginSource::User
        );
        assert!(snapshot.plugin("omarchy.fake").is_none());

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn config_mutations_are_atomic_and_reload_the_snapshot() {
        let root = test_root("mutation");
        let omarchy = root.join("omarchy");
        let home = root.join("home");
        let manifest_dir = omarchy.join("shell/plugins/panels/audio");
        fs::create_dir_all(&manifest_dir).expect("create manifest fixture");
        fs::create_dir_all(omarchy.join("config/omarchy")).expect("create defaults fixture");
        fs::write(
            omarchy.join("config/omarchy/shell.json"),
            r#"{"version":1,"bar":{"position":"top","layout":{"left":[],"center":[],"right":[]}},"plugins":[]}"#,
        )
        .expect("write defaults fixture");
        fs::write(
            manifest_dir.join("manifest.json"),
            r#"{
              "schemaVersion": 1,
              "id": "omarchy.audio",
              "name": "Audio",
              "version": "1.0.0",
              "kinds": ["bar-widget"],
              "entryPoints": {"barWidget": "Panel.qml"}
            }"#,
        )
        .expect("write manifest fixture");

        let mut snapshot = ShellSnapshot::load_from_paths(&omarchy, &home);
        assert!(
            snapshot
                .set_plugin_enabled("omarchy.audio", true, None)
                .unwrap()
        );
        assert_eq!(snapshot.center[0].id, "omarchy.audio");
        assert!(snapshot.user_config_path.is_file());
        assert!(snapshot.toggle_bar_transparency().is_ok());
        assert!(snapshot.transparent);
        assert!(
            snapshot
                .set_plugin_enabled("omarchy.audio", false, None)
                .unwrap()
        );
        assert!(snapshot.center.is_empty());

        let _ = fs::remove_dir_all(root);
    }

    fn test_root(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("omarchy-gpui-{name}-{}", std::process::id()))
    }
}
