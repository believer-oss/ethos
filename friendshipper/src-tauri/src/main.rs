#![deny(clippy::all)]
#![warn(rust_2018_idioms)]
#![allow(deprecated)]
// Prevents additional console window on Windows in release, DO NOT REMOVE!!
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::thread;

use ethos_core::longtail::Longtail;
use ethos_core::types::errors::CoreError;
use friendshipper::server::Server;
use lazy_static::lazy_static;
use regex::Regex;
use serde::Serialize;
use tauri::menu::{Menu, MenuItem};
use tauri::tray::TrayIconBuilder;
use tauri::{Emitter, Manager, WebviewWindow};
use tauri_plugin_notification::NotificationExt;
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

fn force_window_to_front(window: WebviewWindow) {
    if window.is_minimized().unwrap() {
        window.unminimize().unwrap();
    } else {
        window.show().unwrap();
        window.set_focus().unwrap();
    }
}

const PORT: u16 = 8484;

fn main() -> Result<(), CoreError> {
    #[cfg(not(target_os = "macos"))]
    {
        let arg = std::env::args().nth(1).unwrap_or_default();
        if arg.starts_with("friendshipper://") {
            tauri_plugin_deep_link::prepare("com.believer.friendshipper");
        } else {
            let _ = tauri_plugin_deep_link::set_identifier("com.believer.friendshipper");
        }
    }

    // MacOS .app files don't inherit any PATH variables
    #[cfg(target_os = "macos")]
    let _ = fix_path_env::fix();

    if let (Some(config_file), Some(config)) = Server::initialize_app_config()? {
        let (log_path, otel_reload_handle) = match tauri::async_runtime::block_on(async {
            logging::init(
                VERSION,
                APP_NAME,
                VERSION,
                Some(config.user_display_name.clone()),
                config.otlp_endpoint.clone(),
                config.otlp_headers.clone(),
            )
        }) {
            Ok(path) => path,
            Err(e) => {
                error!("Failed to initialize logging: {:?}", e);
                std::process::exit(1);
            }
        };

        match utils::process::check_for_process(APP_NAME, PORT) {
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
            .plugin(tauri_plugin_updater::Builder::new().build())
            .plugin(tauri_plugin_shell::init())
            .plugin(tauri_plugin_notification::init())
            .plugin(tauri_plugin_os::init())
            .plugin(tauri_plugin_clipboard::init())
            .plugin(tauri_plugin_fs::init())
            .plugin(tauri_plugin_dialog::init())
            .plugin(tauri_plugin_process::init())
            .manage(State {
                server_url: server_url.clone(),
                log_path: log_path.clone(),
                client: client.clone(),
                shutdown_tx: shutdown_tx.clone(),
            })
            .invoke_handler(tauri::generate_handler![
                assign_user_to_group,
                cancel_download,
                check_login_required,
                checkout_trunk,
                checkout_main_branch,
                clone_repo,
                configure_git_user,
                copy_profile_data_from_gameserver,
                create_playtest,
                delete_playtest,
                delete_snapshot,
                download_server_logs,
                fix_rebase,
                get_build,
                get_builds,
                get_commits,
                get_dynamic_config,
                get_app_config,
                get_log_path,
                update_app_config,
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
                get_workflow_junit_artifact,
                get_workflow_node_logs,
                install_git,
                launch_server,
                list_snapshots,
                logout,
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
                open_url_for_path,
                reinstall_git_hooks,
                save_snapshot,
                save_changeset,
                load_changeset,
                stop_workflow,
                sync_engine_commit_with_uproject,
                sync_uproject_commit_with_engine,
                reset_repo,
                refetch_repo,
                reset_repo_to_commit,
                restart,
                generate_sln,
                open_sln,
                reset_longtail,
                show_commit_files,
                shutdown_server,
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

                let notification_handle = handle.clone();
                thread::spawn(move || {
                    while let Ok(notification) = notification_rx.recv() {
                        notification_handle
                            .notification()
                            .builder()
                            .title(APP_NAME)
                            .body(&notification)
                            .show()
                            .unwrap();
                    }
                });

                let quit_i = MenuItem::with_id(app, "quit", "Quit", true, None::<&str>)?;
                let show_i = MenuItem::with_id(app, "show", "Show UI", true, None::<&str>)?;
                let open_sln_i =
                    MenuItem::with_id(app, "open-sln", "Open .sln", true, None::<&str>)?;
                let generate_and_open_sln_i = MenuItem::with_id(
                    app,
                    "generate-and-open-sln",
                    "Generate and open .sln",
                    true,
                    None::<&str>,
                )?;
                let menu = Menu::with_items(
                    app,
                    &[&quit_i, &show_i, &open_sln_i, &generate_and_open_sln_i],
                )?;

                let menu_url = url_clone.clone();
                let _ = TrayIconBuilder::new()
                    .icon(app.default_window_icon().unwrap().clone())
                    .menu(&menu)
                    .show_menu_on_left_click(true)
                    .on_menu_event(move |app, event| match event.id.as_ref() {
                        "show" => {
                            let window = app.get_webview_window("main").unwrap();
                            force_window_to_front(window);
                        }
                        "open-sln" => {
                            tauri::async_runtime::block_on(async {
                                match tray_open_sln(menu_url.clone(), client.clone()).await {
                                    Ok(_) => {}
                                    Err(e) => {
                                        error!("Error opening sln: {:?}", e);
                                    }
                                }
                            });
                        }
                        "generate-and-open-sln" => {
                            tauri::async_runtime::block_on(async {
                                match tray_generate_and_open_sln(menu_url.clone(), client.clone())
                                    .await
                                {
                                    Ok(_) => {}
                                    Err(e) => {
                                        error!("Error generating and opening sln: {:?}", e);
                                    }
                                }
                            });
                        }
                        "quit" => {
                            tauri::async_runtime::block_on(async {
                                // Try sending shutdown message for up to 5 seconds
                                let start = std::time::Instant::now();
                                while start.elapsed() < std::time::Duration::from_secs(5) {
                                    match shutdown_tx.send(()).await {
                                        Ok(_) => {
                                            // Message sent, but keep trying until error or timeout
                                            std::thread::sleep(std::time::Duration::from_millis(
                                                100,
                                            ));
                                        }
                                        Err(_) => break, // Channel closed, stop retrying
                                    }
                                }
                            });

                            std::process::exit(0);
                        }
                        _ => {}
                    })
                    .build(app)?;

                let (frontend_op_tx, frontend_op_rx) = std::sync::mpsc::channel();
                {
                    let frontend_op_handle = handle.clone();
                    thread::spawn(move || {
                        while let Ok(op) = frontend_op_rx.recv() {
                            match op {
                                FrontendOp::ShowUI => {
                                    let window =
                                        frontend_op_handle.get_webview_window("main").unwrap();
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
                        gameserver_handle.emit("gameserver-log", &msg).unwrap();
                    }
                });

                let (git_tx, git_rx) = std::sync::mpsc::channel::<String>();
                let git_app_handle = handle.clone();
                tauri::async_runtime::spawn(async move {
                    while let Ok(msg) = git_rx.recv() {
                        let msg = ANSI_REGEX.replace_all(&msg, "");
                        git_app_handle.emit("git-log", &msg).unwrap();
                    }
                });

                let (longtail_tx, longtail_rx) = std::sync::mpsc::channel::<LongtailMsg>();
                let longtail_handle = handle.clone();
                tauri::async_runtime::spawn(async move {
                    while let Ok(msg) = longtail_rx.recv() {
                        Longtail::log_message(msg.clone());

                        if let LongtailMsg::Log(s) = msg {
                            longtail_handle.emit("longtail-log", &s).unwrap();

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
                                    .emit(
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
                        startup_handle.emit("startup-message", &msg).unwrap();

                        if msg.eq("Starting server") {
                            break;
                        }
                    }
                });

                let (refresh_tx, refresh_rx) = std::sync::mpsc::channel::<()>();
                let refresh_handle = handle.clone();
                tauri::async_runtime::spawn(async move {
                    while refresh_rx.recv().is_ok() {
                        refresh_handle.emit("git-refresh", "").unwrap();
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

                    match server
                        .run(
                            config,
                            config_file,
                            startup_tx.clone(),
                            refresh_tx,
                            shutdown_rx,
                        )
                        .await
                    {
                        Ok(_) => {}
                        Err(e) => {
                            startup_tx
                                .send("Warning: Server failed to start".to_string())
                                .unwrap();
                            git_tx.send(e.to_string()).unwrap();
                            error!("Server error: {:?}", e);
                        }
                    }
                });

                #[cfg(not(target_os = "macos"))]
                {
                    let deep_link_handle = handle.clone();
                    tauri::async_runtime::spawn(async move {
                        let deep_link_request = move |request| {
                            info!("Received deep link: {:?}", request);
                            match deep_link_handle.emit("scheme-request-received", request) {
                                Ok(_) => {}
                                Err(e) => {
                                    error!("Failed to emit scheme-request-received: {:?}", e);
                                }
                            }

                            if let Some(window) = deep_link_handle.get_webview_window("main") {
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
                }

                Ok(())
            })
            .run(tauri::generate_context!())
            .expect("error while running tauri application");

        Ok(())
    } else {
        error!("Failed to initialize app config");
        Err(CoreError::Internal(anyhow::anyhow!(
            "Failed to initialize app config"
        )))
    }
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
