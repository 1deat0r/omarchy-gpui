use crate::config::ShellSnapshot;

pub const IPC_METHODS: &[&str] = &[
    "ping",
    "applyTheme",
    "summon",
    "hide",
    "toggle",
    "call",
    "rescanPlugins",
    "reloadConfig",
    "toggleBarTransparency",
    "setPluginEnabled",
    "enablePlugin",
    "putBarWidget",
    "moveBarWidget",
    "setBarWidget",
    "listPlugins",
    "listShellConfig",
    "debugBarGeometry",
    "togglePanelAt",
];

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ShellCommand {
    Ping,
    ApplyTheme {
        colors: String,
        shell: String,
    },
    ListPlugins,
    ReloadConfig,
    RescanPlugins,
    ToggleBarTransparency,
    EnablePlugin {
        id: String,
        placement: String,
    },
    PutBarWidget {
        id: String,
        placement: String,
    },
    MoveBarWidget {
        id: String,
        placement: String,
    },
    SetBarWidget {
        id: String,
        key: String,
        value: String,
        selector: String,
    },
    ListShellConfig,
    DebugBarGeometry,
    TogglePanelAt {
        section: String,
        index: String,
    },
    Call {
        id: String,
        method: String,
        arg: String,
    },
    Summon {
        id: String,
        payload: String,
    },
    Hide {
        id: String,
    },
    Toggle {
        id: String,
        payload: String,
    },
    SetPluginEnabled {
        id: String,
        enabled: bool,
    },
}

pub fn parse(args: &[String]) -> Result<ShellCommand, String> {
    if args.first().map(String::as_str) != Some("shell") {
        return Err("expected the `shell` target".to_string());
    }

    match args.get(1).map(String::as_str) {
        Some("ping") => Ok(ShellCommand::Ping),
        Some("applyTheme") => Ok(ShellCommand::ApplyTheme {
            colors: required(args, 2, "colors payload")?,
            shell: required(args, 3, "shell payload")?,
        }),
        Some("listPlugins") => Ok(ShellCommand::ListPlugins),
        Some("reloadConfig") => Ok(ShellCommand::ReloadConfig),
        Some("rescanPlugins") => Ok(ShellCommand::RescanPlugins),
        Some("toggleBarTransparency") => Ok(ShellCommand::ToggleBarTransparency),
        Some("enablePlugin") => Ok(ShellCommand::EnablePlugin {
            id: required(args, 2, "plugin id")?,
            placement: args.get(3).cloned().unwrap_or_else(|| "{}".to_string()),
        }),
        Some("putBarWidget") => Ok(ShellCommand::PutBarWidget {
            id: required(args, 2, "plugin id")?,
            placement: args.get(3).cloned().unwrap_or_else(|| "{}".to_string()),
        }),
        Some("moveBarWidget") => Ok(ShellCommand::MoveBarWidget {
            id: required(args, 2, "plugin id")?,
            placement: args.get(3).cloned().unwrap_or_else(|| "{}".to_string()),
        }),
        Some("setBarWidget") => Ok(ShellCommand::SetBarWidget {
            id: required(args, 2, "plugin id")?,
            key: required(args, 3, "widget setting")?,
            value: required(args, 4, "widget value")?,
            selector: args.get(5).cloned().unwrap_or_else(|| "{}".to_string()),
        }),
        Some("listShellConfig") => Ok(ShellCommand::ListShellConfig),
        Some("debugBarGeometry") => Ok(ShellCommand::DebugBarGeometry),
        Some("togglePanelAt") => Ok(ShellCommand::TogglePanelAt {
            section: required(args, 2, "bar section")?,
            index: required(args, 3, "bar index")?,
        }),
        Some("call") => Ok(ShellCommand::Call {
            id: required(args, 2, "plugin id")?,
            method: required(args, 3, "plugin method")?,
            arg: args.get(4).cloned().unwrap_or_else(|| "".to_string()),
        }),
        Some("summon") => Ok(ShellCommand::Summon {
            id: required(args, 2, "plugin id")?,
            payload: args.get(3).cloned().unwrap_or_else(|| "{}".to_string()),
        }),
        Some("hide") => Ok(ShellCommand::Hide {
            id: required(args, 2, "plugin id")?,
        }),
        Some("toggle") => Ok(ShellCommand::Toggle {
            id: required(args, 2, "plugin id")?,
            payload: args.get(3).cloned().unwrap_or_else(|| "{}".to_string()),
        }),
        Some("setPluginEnabled") => Ok(ShellCommand::SetPluginEnabled {
            id: required(args, 2, "plugin id")?,
            enabled: args.get(3).is_some_and(|value| value == "true"),
        }),
        Some(method) => Err(format!("unknown shell method: {method}")),
        None => Err("missing shell method".to_string()),
    }
}

pub fn response(command: &ShellCommand, snapshot: &ShellSnapshot) -> String {
    match command {
        ShellCommand::Ping
        | ShellCommand::ApplyTheme { .. }
        | ShellCommand::ReloadConfig
        | ShellCommand::RescanPlugins
        | ShellCommand::ToggleBarTransparency
        | ShellCommand::EnablePlugin { .. }
        | ShellCommand::PutBarWidget { .. }
        | ShellCommand::MoveBarWidget { .. }
        | ShellCommand::SetBarWidget { .. }
        | ShellCommand::Hide { .. } => "ok".to_string(),
        ShellCommand::ListPlugins => {
            let mut plugins = snapshot
                .plugins
                .iter()
                .map(|plugin| {
                    let active = plugin.has_kind("bar") && snapshot.plugin_is_enabled(&plugin.id);
                    let cloned_from = plugin
                        .raw
                        .get("omarchy")
                        .and_then(|metadata| metadata.get("clonedFrom"))
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or_default();
                    serde_json::json!({
                        "id": plugin.id,
                        "name": plugin.name,
                        "kinds": plugin.kinds,
                        "enabled": if plugin.has_kind("bar") { active } else if plugin.has_kind("bar-widget") { snapshot.left.iter().chain(snapshot.center.iter()).chain(snapshot.right.iter()).any(|entry| entry.id == plugin.id) } else { snapshot.plugin_is_enabled(&plugin.id) },
                        "active": active,
                        "canDisable": !plugin.has_kind("bar"),
                        "firstParty": plugin.source == crate::config::PluginSource::FirstParty,
                        "clonedFrom": cloned_from,
                    })
                })
                .collect::<Vec<_>>();
            plugins.sort_by(|left, right| {
                left["name"]
                    .as_str()
                    .unwrap_or_default()
                    .cmp(right["name"].as_str().unwrap_or_default())
                    .then_with(|| {
                        left["id"]
                            .as_str()
                            .unwrap_or_default()
                            .cmp(right["id"].as_str().unwrap_or_default())
                    })
            });
            serde_json::to_string(&plugins).expect("plugin list is serializable")
        }
        ShellCommand::ListShellConfig => snapshot.config.to_string(),
        ShellCommand::DebugBarGeometry => "[]".to_string(),
        ShellCommand::TogglePanelAt { .. } => "unknown".to_string(),
        ShellCommand::Call { .. } => "unknown".to_string(),
        ShellCommand::Summon { id, .. } | ShellCommand::Toggle { id, .. } => {
            if snapshot.plugin(id).is_some() {
                "ok".to_string()
            } else {
                "unknown".to_string()
            }
        }
        ShellCommand::SetPluginEnabled { id, enabled } => {
            let _requested_state = enabled;
            if snapshot.plugin(id).is_some() {
                "ok".to_string()
            } else {
                "unknown".to_string()
            }
        }
    }
}

fn required(args: &[String], index: usize, label: &str) -> Result<String, String> {
    args.get(index)
        .filter(|value| !value.is_empty())
        .cloned()
        .ok_or_else(|| format!("missing {label}"))
}

#[cfg(test)]
mod tests {
    use super::{ShellCommand, parse};

    #[test]
    fn parses_omarchy_shell_commands() {
        assert_eq!(parse(&args(&["shell", "ping"])), Ok(ShellCommand::Ping));
        assert_eq!(
            parse(&args(&["shell", "toggle", "omarchy.menu"])),
            Ok(ShellCommand::Toggle {
                id: "omarchy.menu".to_string(),
                payload: "{}".to_string(),
            })
        );
        assert_eq!(
            parse(&args(&["shell", "setPluginEnabled", "omarchy.clock"])),
            Ok(ShellCommand::SetPluginEnabled {
                id: "omarchy.clock".to_string(),
                enabled: false,
            })
        );
    }

    #[test]
    fn rejects_unknown_methods() {
        assert!(parse(&args(&["shell", "nope"])).is_err());
    }

    fn args(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_string()).collect()
    }
}
