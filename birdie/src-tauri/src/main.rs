#![deny(clippy::all)]
#![warn(rust_2018_idioms)]
#![allow(deprecated)]
// Prevents additional console window on Windows in release, DO NOT REMOVE!!
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::fs;
use std::path::PathBuf;

use birdie::types::config::BirdieRepoConfig;
use ethos_core::clients::git::Git;
use ethos_core::types::errors::CoreError;
use lazy_static::lazy_static;
use regex::Regex;
use tracing::{error, info, warn};

use birdie::server::Server;
use ethos_core::tauri::State;
use ethos_core::utils::logging;
use ethos_core::{clients, utils};

use crate::command::*;
use ethos_core::tauri::command::*;

use tauri::menu::{Menu, MenuItem};
use tauri::tray::TrayIconBuilder;
use tauri::{Emitter, Manager, WebviewWindow};

pub static VERSION: &str = env!("CARGO_PKG_VERSION");
pub static APP_NAME: &str = env!("CARGO_PKG_NAME");

static PORT: u16 = 8485;

// Leaving this in, but likely can be removed
// #[derive(Clone, Debug, Serialize)]
// struct LongtailProgressCaptures {
//     progress: String,
//     elapsed: String,
//     remaining: String,
// }

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

fn main() -> Result<(), CoreError> {
    // MacOS .app files don't inherit any PATH variables
    #[cfg(target_os = "macos")]
    let _ = fix_path_env::fix();

    if let (Some(config_file), Some(config)) = Server::initialize_app_config()? {
        // if there's a repo path, check its root for a birdie.yaml, and serialize that into a BirdieRepoConfig
        let repo_config: Option<BirdieRepoConfig> = match !config.repo_path.is_empty() {
            true => {
                let repo_config_file = PathBuf::from(&config.repo_path).join("birdie.yaml");
                if repo_config_file.exists() {
                    let file = match fs::OpenOptions::new().read(true).open(&repo_config_file) {
                        Ok(file) => file,
                        Err(e) => {
                            error!("Failed to open repo config file: {:?}", e);
                            std::process::exit(1);
                        }
                    };

                    match serde_yaml::from_reader(file) {
                        Ok(repo_config) => Some(repo_config),
                        Err(e) => {
                            error!("Failed to deserialize repo config: {:?}", e);
                            std::process::exit(1);
                        }
                    }
                } else {
                    None
                }
            }
            false => None,
        };

        let (log_path, _otel_reload_handle) = match repo_config {
            Some(repo_config) => {
                // if either oltp_endpoint or otlp_headers are Some and empty, return an error
                if let Some(endpoint) = &repo_config.otlp_endpoint {
                    if endpoint.is_empty() {
                        return Err(CoreError::Input(anyhow::anyhow!(
                            "otlp_endpoint cannot be empty"
                        )));
                    }
                }

                if let Some(headers) = &repo_config.otlp_headers {
                    if headers.is_empty() {
                        return Err(CoreError::Input(anyhow::anyhow!(
                            "otlp_headers cannot be empty"
                        )));
                    }
                }

                let init_result = tauri::async_runtime::block_on(async {
                    // get username from git config
                    let (tx, rx) = std::sync::mpsc::channel::<String>();
                    let git_client = Git::new(PathBuf::from(&config.repo_path), tx);

                    // spin up a thread to log git messages
                    tauri::async_runtime::spawn(async move {
                        while let Ok(msg) = rx.recv() {
                            info!("{}", msg);
                        }
                    });

                    let username = git_client.get_username().await?;
                    logging::init(
                        VERSION,
                        APP_NAME,
                        VERSION,
                        Some(username),
                        repo_config.otlp_endpoint.clone(),
                        repo_config.otlp_headers.clone(),
                    )
                });

                match init_result {
                    Ok(path) => path,
                    Err(e) => {
                        error!("Failed to initialize logging: {:?}", e);
                        std::process::exit(1);
                    }
                }
            }
            None => match logging::init(VERSION, APP_NAME, VERSION, None, None, None) {
                Ok(path) => path,
                Err(e) => {
                    error!("Failed to initialize logging: {:?}", e);
                    std::process::exit(1);
                }
            },
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

        tauri::Builder::default()
            .plugin(tauri_plugin_fs::init())
            .plugin(tauri_plugin_updater::Builder::new().build())
            .plugin(tauri_plugin_notification::init())
            .plugin(tauri_plugin_shell::init())
            .plugin(tauri_plugin_dialog::init())
            .plugin(tauri_plugin_process::init())
            .manage(State {
                server_url: server_url.clone(),
                log_path: log_path.clone(),
                client,
                shutdown_tx,
            })
            .invoke_handler(tauri::generate_handler![
                checkout_trunk,
                clone_repo,
                configure_git_user,
                download_lfs_files,
                get_fetch_include,
                del_fetch_include,
                fix_rebase,
                get_commits,
                get_all_files,
                get_files,
                get_file,
                get_directory_metadata,
                get_file_history,
                get_log_path,
                get_logs,
                get_rebase_status,
                get_repo_config,
                get_repo_status,
                get_system_status,
                install_git,
                lock_files,
                open_system_logs_folder,
                open_terminal_to_path,
                open_url,
                rebase,
                refetch_repo,
                release_locks,
                revert_files,
                restart,
                run_set_env,
                show_commit_files,
                submit,
                sync_latest,
                unlock_files,
                update_metadata,
                update_metadata_class,
                sync_tools,
                verify_locks,
                get_config,
                update_config
            ])
            .setup(move |app| {
                let handle = app.handle();

                // Setup tray menu
                let quit_i = MenuItem::with_id(app, "quit", "Quit", true, None::<&str>)?;
                let show_i = MenuItem::with_id(app, "show", "Show UI", true, None::<&str>)?;
                let menu = Menu::with_items(app, &[&quit_i, &show_i])?;

                let _ = TrayIconBuilder::new()
                    .icon(app.default_window_icon().unwrap().clone())
                    .menu(&menu)
                    .show_menu_on_left_click(true)
                    .on_menu_event(move |app, event| match event.id.as_ref() {
                        "show" => {
                            let window = app.get_webview_window("main").unwrap();
                            force_window_to_front(window);
                        }
                        "quit" => {
                            std::process::exit(0);
                        }
                        _ => {}
                    })
                    .build(app)?;

                let (git_tx, git_rx) = std::sync::mpsc::channel::<String>();
                let git_app_handle = handle.clone();
                tauri::async_runtime::spawn(async move {
                    while let Ok(msg) = git_rx.recv() {
                        let msg = ANSI_REGEX.replace_all(&msg, "");
                        git_app_handle.emit("git-log", &msg).unwrap();
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

                let server_log_path = log_path.clone();
                tauri::async_runtime::spawn(async move {
                    let server = Server::new(PORT, server_log_path, git_tx.clone());

                    match server
                        .run(config, config_file, startup_tx, shutdown_rx)
                        .await
                    {
                        Ok(_) => {}
                        Err(e) => {
                            error!("Failed to start server: {:?}", e);
                        }
                    }
                });

                Ok(())
            })
            .run(tauri::generate_context!())
            .expect("error while running tauri application");

        Ok(())
    } else {
        error!("Failed to initialize app config");
        std::process::exit(1);
    }
}
