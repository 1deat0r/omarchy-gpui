mod config;
mod ipc;
mod menu;
mod system;
mod ui;

use std::{env, time::Duration};

use config::ShellSnapshot;
use gpui::{
    App, AppContext, Bounds, WindowBackgroundAppearance, WindowBounds, WindowKind, WindowOptions,
    layer_shell::{Anchor, KeyboardInteractivity, Layer, LayerShellOptions},
    point, px, size,
};
use gpui_platform::application;

fn main() {
    let args: Vec<String> = env::args().skip(1).collect();
    let snapshot = ShellSnapshot::load();

    match args.first().map(String::as_str) {
        Some("--help") | Some("-h") => print_help(),
        Some("--print-contract") => print_contract(&snapshot),
        Some("--print-system") => print_system(),
        Some("--print-menu") => print_menu(),
        Some("--smoke") => run_shell(snapshot, true),
        Some("shell") => match ipc::parse(&args) {
            Ok(command) => {
                match ipc::try_call_running(&args) {
                    Ok(Some(response)) => {
                        if !response.is_empty() {
                            println!("{response}");
                        }
                        return;
                    }
                    Ok(None) => {}
                    Err(error) => {
                        eprintln!("omarchy-gpui-shell: {error}");
                        std::process::exit(2);
                    }
                }
                let mut snapshot = snapshot;
                match ipc::dispatch(&command, &mut snapshot) {
                    Ok(response) => println!("{response}"),
                    Err(error) => {
                        eprintln!("omarchy-gpui-shell: {error}");
                        std::process::exit(2);
                    }
                }
            }
            Err(error) => {
                eprintln!("omarchy-gpui-shell: {error}");
                std::process::exit(2);
            }
        },
        Some(unknown) => {
            eprintln!("omarchy-gpui-shell: unknown argument `{unknown}`");
            print_help();
            std::process::exit(2);
        }
        None => run_shell(snapshot, false),
    }
}

fn print_help() {
    println!(
        "omarchy-gpui-shell\n\nUsage:\n  omarchy-gpui-shell                 run the GPUI layer-shell bar\n  omarchy-gpui-shell --smoke          render one Wayland frame and exit\n  omarchy-gpui-shell --print-contract print resolved Omarchy paths and contract\n  omarchy-gpui-shell --print-system   print live compositor and service state\n  omarchy-gpui-shell shell ping        run a shell IPC-compatible health command\n  omarchy-gpui-shell shell listPlugins list configured shell entries\n"
    );
}

fn print_system() {
    let system = system::SystemSnapshot::collect();
    println!("system={}", system::to_value(&system));
    println!("OMARCHY_GPUI_SYSTEM_RUNTIME_OK");
}

fn print_menu() {
    let menu = menu::MenuModel::load();
    println!("menu_items={}", menu.items.len());
    println!("menu_root_children={}", menu.children("root").len());
    println!("menu_power_route={}", menu.resolve_route("power-menu"));
    println!("OMARCHY_GPUI_MENU_RUNTIME_OK");
}

fn print_contract(snapshot: &ShellSnapshot) {
    println!("omarchy_path={}", snapshot.omarchy_path.display());
    println!("defaults_path={}", snapshot.defaults_path.display());
    println!("user_config_path={}", snapshot.user_config_path.display());
    println!("config_source={}", snapshot.source.as_str());
    println!(
        "config_version={}",
        snapshot
            .config
            .get("version")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or_default()
    );
    println!("bar_position={}", snapshot.bar_position);
    println!("bar_transparent={}", snapshot.transparent);
    println!(
        "bar_entries=left:{} center:{} right:{}",
        snapshot.left.len(),
        snapshot.center.len(),
        snapshot.right.len()
    );
    println!("discovered_plugins={}", snapshot.plugins.len());
    let discovered = snapshot
        .plugins
        .iter()
        .map(|plugin| {
            format!(
                "{}@{}:{}:{}:{}",
                plugin.id,
                plugin.version,
                plugin.source.as_str(),
                plugin.source_dir.display(),
                plugin.entry_points.len()
            )
        })
        .collect::<Vec<_>>();
    println!("discovered_plugin_records={}", discovered.join(","));
    println!("configured_plugins={}", snapshot.plugin_ids.join(","));
    println!("ipc_methods={}", ipc::IPC_METHODS.join(","));
    println!("OMARCHY_GPUI_CONTRACT_RUNTIME_OK");
}

fn run_shell(snapshot: ShellSnapshot, smoke: bool) {
    let bottom = snapshot.bar_position.eq_ignore_ascii_case("bottom");
    let anchor = if bottom {
        Anchor::BOTTOM | Anchor::LEFT | Anchor::RIGHT
    } else {
        Anchor::TOP | Anchor::LEFT | Anchor::RIGHT
    };
    let edge = if bottom { Anchor::BOTTOM } else { Anchor::TOP };
    let margin = if bottom {
        (px(0.0), px(12.0), px(8.0), px(12.0))
    } else {
        (px(8.0), px(12.0), px(0.0), px(12.0))
    };

    let (_ipc_server, ipc_events) =
        ipc::IpcServer::start(snapshot.clone()).unwrap_or_else(|error| {
            eprintln!("omarchy-gpui-shell: {error}");
            std::process::exit(2);
        });

    application().run(move |cx: &mut App| {
        let window_options = WindowOptions {
            titlebar: None,
            window_bounds: Some(WindowBounds::Windowed(Bounds {
                origin: point(px(0.0), px(0.0)),
                // A zero width asks layer-shell to resolve the width from the
                // left/right anchors and the configured margins on the target
                // output. A fixed width would make the bar float at its
                // initial size instead of spanning the monitor.
                size: size(px(0.0), px(44.0)),
            })),
            app_id: Some("omarchy-gpui-shell".to_string()),
            window_background: WindowBackgroundAppearance::Transparent,
            kind: WindowKind::LayerShell(LayerShellOptions {
                namespace: "omarchy-gpui".to_string(),
                layer: Layer::Top,
                anchor,
                exclusive_zone: Some(px(44.0)),
                exclusive_edge: Some(edge),
                margin: Some(margin),
                keyboard_interactivity: KeyboardInteractivity::None,
            }),
            focus: false,
            is_movable: false,
            is_resizable: false,
            is_minimizable: false,
            ..Default::default()
        };

        cx.open_window(window_options, move |window, cx| {
            if smoke {
                window
                    .spawn(cx, async move |cx| {
                        cx.background_executor()
                            .timer(Duration::from_millis(1200))
                            .await;
                        let _ = cx.update(|_, cx| cx.quit());
                    })
                    .detach();
            }
            cx.new(|cx| ui::ShellView::new(snapshot, smoke, ipc_events, cx))
        })
        .expect("open Omarchy GPUI layer-shell window");

        cx.activate(false);
    });
}
