#![deny(clippy::all)]
#![warn(rust_2018_idioms)]
// Prevents additional console window on Windows in release, DO NOT REMOVE!!
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::thread;

use lazy_static::lazy_static;
use serde::Serialize;
use tauri::api::notification::Notification;
use tauri::regex::Regex;
use tauri::{
    CustomMenuItem, Manager, SystemTray, SystemTrayEvent, SystemTrayMenu, SystemTrayMenuItem,
    Window, WindowEvent,
};
use tracing::{error, info, warn};

use ethos_core::tauri::State;
use ethos_core::{clients, msg::LongtailMsg, utils, utils::logging};
use friendshipper::state::FrontendOp;
use friendshipper::APP_NAME;

use crate::command::*;
use ethos_core::tauri::command::*;

pub static VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(Clone, Debug, Serialize)]
struct LongtailProgressCaptures {
    progress: String,
    elapsed: String,
    remaining: String,
}

mod command;

// see test_longtail_regex() for examples of matches
lazy_static! {
    static ref LONGTAIL_PROGRESS_REGEX: Regex =
        Regex::new(r"(\d{1,3}%).*?((?:\d+[a-z])+):?((?:\d+[a-z])+)?").unwrap();
    static ref ANSI_REGEX: Regex =
        Regex::new(r"[\u001b\u009b]\[[()#;?]*(?:[0-9]{1,4}(?:;[0-9]{0,4})*)?[0-9A-ORZcf-nqry=><]")
            .unwrap();
}

fn initialize_tray() -> SystemTray {
    let quit = CustomMenuItem::new("quit".to_string(), "Quit");
    let show = CustomMenuItem::new("show".to_string(), "Show UI");

    let open_sln = CustomMenuItem::new("open-sln".to_string(), "Open .sln");
    let generate_and_open_sln = CustomMenuItem::new(
        "generate-and-open-sln".to_string(),
        "Generate and open .sln",
    );

    let tray_menu = SystemTrayMenu::new()
        .add_item(show)
        .add_native_item(SystemTrayMenuItem::Separator)
        .add_item(open_sln)
        .add_item(generate_and_open_sln)
        .add_native_item(SystemTrayMenuItem::Separator)
        .add_item(quit);

    SystemTray::new().with_menu(tray_menu)
}

fn force_window_to_front(window: Window) {
    if window.is_minimized().unwrap() {
        window.unminimize().unwrap();
    } else {
        window.show().unwrap();
        window.set_focus().unwrap();
    }
}

const PORT: u16 = 8484;

fn main() {
    let arg = std::env::args().nth(1).unwrap_or_default();
    if arg.starts_with("friendshipper://") {
        tauri_plugin_deep_link::prepare("com.believer.friendshipper");
    } else {
        let _ = tauri_plugin_deep_link::set_identifier("com.believer.friendshipper");
    }

    let (log_path, otel_reload_handle) = match logging::init(VERSION, APP_NAME) {
        Ok(path) => path,
        Err(e) => {
            error!("Failed to initialize logging: {:?}", e);
            std::process::exit(1);
        }
    };

    match utils::process::check_for_process(APP_NAME) {
        Ok(_) => {}
        Err(e) => {
            error!("Failed to check for existing process: {:?}", e);
            std::process::exit(1);
        }
    }

    match utils::process::wait_for_port(PORT) {
        Ok(_) => {}
        Err(e) => {
            error!("Failed to wait for port: {:?}", e);
            std::process::exit(1);
        }
    }

    let server_url = format!("http://localhost:{}", PORT);
    info!(
        version = VERSION,
        address = &server_url,
        app = APP_NAME,
        "Starting up"
    );

    let tray = initialize_tray();

    let client = match clients::command::new_reqwest_client() {
        Ok(client) => client,
        Err(e) => {
            error!("Failed to create reqwest client: {:?}", e);
            std::process::exit(1);
        }
    };

    let (shutdown_tx, shutdown_rx) = tokio::sync::mpsc::channel::<()>(1);

    let url_clone = server_url.clone();
    tauri::Builder::default()
        .manage(State {
            server_url: server_url.clone(),
            log_path: log_path.clone(),
            client: client.clone(),
            shutdown_tx,
        })
        .invoke_handler(tauri::generate_handler![
            assign_user_to_group,
            check_login_required,
            checkout_trunk,
            checkout_commit,
            clone_repo,
            configure_git_user,
            create_playtest,
            delete_playtest,
            delete_snapshot,
            download_server_logs,
            fix_rebase,
            get_builds,
            get_commits,
            get_dynamic_config,
            get_app_config,
            get_log_path,
            update_app_config,
            get_latest_version,
            get_logs,
            get_playtests,
            get_project_config,
            get_pull_request,
            get_pull_requests,
            get_rebase_status,
            get_repo_config,
            get_repo_status,
            get_server,
            get_servers,
            get_system_status,
            get_workflows,
            get_workflow_node_logs,
            install_git,
            launch_server,
            list_snapshots,
            open_logs_folder,
            open_system_logs_folder,
            open_terminal_to_path,
            get_unrealversionselector_status,
            open_url,
            quick_submit,
            rebase,
            refresh_login,
            acquire_locks,
            release_locks,
            reset_config,
            restore_snapshot,
            revert_files,
            force_download_dlls,
            force_download_engine,
            get_merge_queue,
            reinstall_git_hooks,
            save_snapshot,
            stop_workflow,
            sync_engine_commit_with_uproject,
            sync_uproject_commit_with_engine,
            reset_repo,
            restart,
            run_update,
            generate_sln,
            open_sln,
            reset_longtail,
            show_commit_files,
            start_gameserver_log_tail,
            stop_gameserver_log_tail,
            sync_client,
            sync_latest,
            open_project,
            terminate_server,
            unassign_user_from_playtest,
            update_playtest,
            verify_build,
            wipe_client_data,
        ])
        .setup(move |app| {
            let handle = app.handle();
            let (notification_tx, notification_rx) = std::sync::mpsc::channel();

            let identifier = app.config().tauri.bundle.identifier.clone();
            thread::spawn(move || {
                while let Ok(notification) = notification_rx.recv() {
                    let _ = Notification::new(&identifier)
                        .title(APP_NAME)
                        .body(&notification)
                        .show();
                }
            });

            let (frontend_op_tx, frontend_op_rx) = std::sync::mpsc::channel();
            {
                let frontend_op_handle = handle.clone();
                thread::spawn(move || {
                    while let Ok(op) = frontend_op_rx.recv() {
                        match op {
                            FrontendOp::ShowUI => {
                                let window = frontend_op_handle.get_window("main").unwrap();
                                force_window_to_front(window);
                            }
                        }
                    }
                });
            }

            let (gameserver_log_tx, gameserver_log_rx) = std::sync::mpsc::channel::<String>();
            let gameserver_handle = handle.clone();
            tauri::async_runtime::spawn(async move {
                while let Ok(msg) = gameserver_log_rx.recv() {
                    gameserver_handle.emit_all("gameserver-log", &msg).unwrap();
                }
            });

            let (git_tx, git_rx) = std::sync::mpsc::channel::<String>();
            let git_app_handle = handle.clone();
            tauri::async_runtime::spawn(async move {
                while let Ok(msg) = git_rx.recv() {
                    let msg = ANSI_REGEX.replace_all(&msg, "");
                    git_app_handle.emit_all("git-log", &msg).unwrap();
                }
            });

            let (longtail_tx, longtail_rx) = std::sync::mpsc::channel();
            let longtail_handle = handle.clone();
            tauri::async_runtime::spawn(async move {
                while let Ok(msg) = longtail_rx.recv() {
                    if let LongtailMsg::Log(s) = msg {
                        info!("longtail log: {}", &s);
                        longtail_handle.emit_all("longtail-log", &s).unwrap();

                        if let Some(captures) = LONGTAIL_PROGRESS_REGEX.captures(&s) {
                            let progress: String = captures
                                .get(1)
                                .map(|m| m.as_str())
                                .unwrap_or("")
                                .to_string();
                            let elapsed: String = captures
                                .get(2)
                                .map(|m| m.as_str())
                                .unwrap_or("")
                                .to_string();
                            let remaining: String = captures
                                .get(3)
                                .map(|m| m.as_str())
                                .unwrap_or("")
                                .to_string();

                            longtail_handle
                                .emit_all(
                                    "longtail-sync-progress",
                                    LongtailProgressCaptures {
                                        progress,
                                        elapsed,
                                        remaining,
                                    },
                                )
                                .unwrap();
                        } else {
                            warn!("failed to parse longtail log: {}", &s);
                        }
                    }
                }
            });

            let (startup_tx, startup_rx) = std::sync::mpsc::channel::<String>();
            let startup_handle = handle.clone();
            tauri::async_runtime::spawn(async move {
                while let Ok(msg) = startup_rx.recv() {
                    startup_handle.emit_all("startup-message", &msg).unwrap();

                    if msg.eq("Starting server") {
                        break;
                    }
                }
            });

            let (refresh_tx, refresh_rx) = std::sync::mpsc::channel::<()>();
            let refresh_handle = handle.clone();
            tauri::async_runtime::spawn(async move {
                while refresh_rx.recv().is_ok() {
                    refresh_handle.emit_all("git-refresh", "").unwrap();
                }
            });

            let server_log_path = log_path.clone();
            tauri::async_runtime::spawn(async move {
                let server = friendshipper::server::Server::new(
                    PORT,
                    longtail_tx.clone(),
                    notification_tx.clone(),
                    frontend_op_tx,
                    server_log_path,
                    git_tx.clone(),
                    gameserver_log_tx.clone(),
                    otel_reload_handle,
                );

                match server.run(startup_tx, refresh_tx, shutdown_rx).await {
                    Ok(_) => {}
                    Err(e) => {
                        error!("Server error: {:?}", e);
                    }
                }
            });

            let deep_link_handle = handle.clone();
            tauri::async_runtime::spawn(async move {
                let deep_link_request = move |request| {
                    info!("Received deep link: {:?}", request);
                    match deep_link_handle.emit_all("scheme-request-received", request) {
                        Ok(_) => {}
                        Err(e) => {
                            error!("Failed to emit scheme-request-received: {:?}", e);
                        }
                    }

                    if let Some(window) = deep_link_handle.get_window("main") {
                        force_window_to_front(window);
                    }
                };
                match tauri_plugin_deep_link::register("friendshipper", deep_link_request) {
                    Ok(_) => {}
                    Err(e) => {
                        error!("Failed to register deep link handler: {:?}", e);
                    }
                }
            });

            Ok(())
        })
        .system_tray(tray)
        .on_system_tray_event(move |app, event| match event {
            SystemTrayEvent::DoubleClick {
                position: _,
                size: _,
                ..
            } => {
                let window = app.get_window("main").unwrap();

                force_window_to_front(window);
            }
            SystemTrayEvent::MenuItemClick { id, .. } => match id.as_str() {
                "show" => {
                    let window = app.get_window("main").unwrap();

                    force_window_to_front(window);
                }
                "open-sln" => {
                    tauri::async_runtime::block_on(async {
                        match tray_open_sln(url_clone.clone(), client.clone()).await {
                            Ok(_) => {}
                            Err(e) => {
                                error!("Error opening sln: {:?}", e);
                            }
                        }
                    });
                }
                "generate-and-open-sln" => {
                    tauri::async_runtime::block_on(async {
                        match tray_generate_and_open_sln(url_clone.clone(), client.clone()).await {
                            Ok(_) => {}
                            Err(e) => {
                                error!("Error generating and opening sln: {:?}", e);
                            }
                        }
                    });
                }
                "quit" => {
                    std::process::exit(0);
                }
                _ => {}
            },
            _ => {}
        })
        .on_window_event(|event| {
            if let WindowEvent::CloseRequested { api, .. } = event.event() {
                event.window().hide().unwrap();
                api.prevent_close();
            }
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

#[cfg(test)]
mod tests {
    use crate::LONGTAIL_PROGRESS_REGEX;

    #[test]
    fn test_longtail_regex() {
        let caps = LONGTAIL_PROGRESS_REGEX.captures("Updating version             9%: |████                                              |: [30s:6m7s]");
        caps.expect("Failed to match string");
        let caps = LONGTAIL_PROGRESS_REGEX.captures("Indexing version            55%:|███████████████████████████                       |: [0s]");
        caps.expect("Failed to match string");
        let caps = LONGTAIL_PROGRESS_REGEX.captures("Updating version             1%: |                                                  |: [0s]");
        caps.expect("Failed to match string");
    }
}
