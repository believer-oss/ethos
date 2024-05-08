use std::str::FromStr;
use std::{
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use axum::{body::Body, http::Request, response::Response};
use lazy_static::lazy_static;
use parking_lot::RwLock;
use tokio::{
    process::Command,
    sync::{
        mpsc,
        oneshot::{Receiver, Sender},
    },
    task::JoinHandle,
};
use tower_http::trace::TraceLayer;
use tracing::{info, Span};

use ethos_core::fs::LocalDownloadPath;
use ethos_core::storage::mock::MockArtifactProvider;
use ethos_core::storage::{ArtifactStorage, StorageSchemaVersion};
use ethos_core::types::config::{AppConfig, DynamicConfig, RepoConfig};
use ethos_core::worker::RepoWorker;
use ethos_core::AWSClient;
#[cfg(windows)]
use friendshipper::repo::CREATE_NO_WINDOW;
use friendshipper::state::AppState;

static ACCESS_KEY: &str = match option_env!("ACCESS_KEY") {
    Some(v) => v,
    None => "",
};
static SECRET_KEY: &str = match option_env!("SECRET_KEY") {
    Some(v) => v,
    None => "",
};

lazy_static! {
    pub static ref TEST_DIR: PathBuf = {
        let mut path = std::env::current_dir().unwrap();
        path.push("tests/tmp");
        path
    };
}

fn build_uproject_path(repo_path: &Path) -> PathBuf {
    let cwd = std::env::current_dir().unwrap();
    repo_path.join(cwd).join("friendshipper-tests.uproject")
}

pub struct TestServer {
    pub state: Arc<AppState>,
    exit_tx: Option<Sender<()>>,
    server_thread: Option<JoinHandle<()>>,
}

impl TestServer {
    pub fn new(state: Arc<AppState>, exit_tx: Sender<()>) -> Self {
        Self {
            state,
            exit_tx: Some(exit_tx),
            server_thread: None,
        }
    }

    pub async fn start(&mut self, exit_rx: Receiver<()>) {
        let app = friendshipper::router(self.state.clone()).unwrap().layer(
            TraceLayer::new_for_http()
                .on_request(|request: &Request<Body>, _span: &Span| {
                    info!("Request: {} {}", request.method(), request.uri().path(),);
                })
                .on_response(|response: &Response, _latency: Duration, _span: &Span| {
                    info!("Response: {} {:?}", response.status(), response.body());
                }),
        );

        let address = String::from("127.0.0.1:8585");

        info!("starting server at {}", address);

        let shutdown = async {
            info!("shutting down");

            match exit_rx.await {
                Ok(_) => {
                    info!("received shutdown signal");

                    // Wait a second to flush an update request if there is one
                    tokio::time::sleep(Duration::from_secs(3)).await;
                }
                Err(e) => {
                    info!("error receiving shutdown signal: {:?}", e);
                }
            }
        };

        self.server_thread = Some(tokio::spawn(async move {
            let listener = tokio::net::TcpListener::bind(address).await.unwrap();
            match axum::serve(listener, app.into_make_service())
                .with_graceful_shutdown(shutdown)
                .await
            {
                Ok(_) => {
                    info!("server exited");
                }
                Err(e) => {
                    info!("server error: {:?}", e);
                }
            }
        }));
    }

    pub async fn shutdown(&mut self) {
        info!("Shutting down server");
        if let Some(tx) = self.exit_tx.take() {
            _ = tx.send(());
        }
    }
}

async fn initialize_test_repo() {
    let repo_path = TEST_DIR.join("test-local");
    let _ = std::fs::remove_dir_all(&repo_path);
    let _ = std::fs::create_dir_all(&repo_path);

    let remote_path = TEST_DIR.join("test-remote");
    let _ = std::fs::remove_dir_all(&remote_path);
    let _ = std::fs::create_dir_all(&remote_path);

    // initialize local
    let mut init = Command::new("git");
    init.arg("init").current_dir(&repo_path);

    #[cfg(windows)]
    init.creation_flags(CREATE_NO_WINDOW);

    match init.output().await {
        Ok(_) => {}
        Err(e) => {
            panic!("failed to initialize test repo: {:?}", e);
        }
    }

    // initialize remote
    let mut init = Command::new("git");
    init.arg("init").current_dir(&remote_path);

    #[cfg(windows)]
    init.creation_flags(CREATE_NO_WINDOW);

    match init.output().await {
        Ok(_) => {}
        Err(e) => {
            panic!("failed to initialize test remote: {:?}", e);
        }
    }

    let mut config = Command::new("git");
    config
        .arg("config")
        .arg("--local")
        .arg("user.name")
        .arg("test")
        .current_dir(&repo_path);

    #[cfg(windows)]
    config.creation_flags(CREATE_NO_WINDOW);

    match config.output().await {
        Ok(_) => {}
        Err(e) => {
            panic!("failed to set git config: {:?}", e);
        }
    }

    let mut config = Command::new("git");
    config
        .arg("config")
        .arg("--local")
        .arg("user.email")
        .arg("me@test.com")
        .current_dir(&repo_path);

    #[cfg(windows)]
    config.creation_flags(CREATE_NO_WINDOW);

    match config.output().await {
        Ok(_) => {}
        Err(e) => {
            panic!("failed to set git config: {:?}", e);
        }
    }

    let mut config = Command::new("git");
    config
        .arg("config")
        .arg("core.autocrlf")
        .arg("true")
        .current_dir(&repo_path);

    #[cfg(windows)]
    config.creation_flags(CREATE_NO_WINDOW);

    match config.output().await {
        Ok(_) => {}
        Err(e) => {
            panic!("failed to set git config: {:?}", e);
        }
    }

    let mut config = Command::new("git");
    config
        .arg("config")
        .arg("core.autocrlf")
        .arg("true")
        .current_dir(&remote_path);

    #[cfg(windows)]
    config.creation_flags(CREATE_NO_WINDOW);

    match config.output().await {
        Ok(_) => {}
        Err(e) => {
            panic!("failed to set git config: {:?}", e);
        }
    }

    let mut config = Command::new("git");
    config
        .arg("config")
        .arg("core.filemode")
        .arg("false")
        .current_dir(&repo_path);

    #[cfg(windows)]
    config.creation_flags(CREATE_NO_WINDOW);

    match config.output().await {
        Ok(_) => {}
        Err(e) => {
            panic!("failed to set git config: {:?}", e);
        }
    }

    let mut config = Command::new("git");
    config
        .arg("config")
        .arg("core.filemode")
        .arg("false")
        .current_dir(&remote_path);

    #[cfg(windows)]
    config.creation_flags(CREATE_NO_WINDOW);

    match config.output().await {
        Ok(_) => {}
        Err(e) => {
            panic!("failed to set git config: {:?}", e);
        }
    }

    let mut remote = Command::new("git");
    remote
        .arg("remote")
        .arg("add")
        .arg("origin")
        .arg(&remote_path)
        .current_dir(&repo_path);

    #[cfg(windows)]
    remote.creation_flags(CREATE_NO_WINDOW);

    match remote.output().await {
        Ok(_) => {}
        Err(e) => {
            panic!("failed to set git remote: {:?}", e);
        }
    }

    let mut checkout_temp = Command::new("git");
    checkout_temp
        .arg("checkout")
        .arg("-b")
        .arg("temp")
        .current_dir(&remote_path);

    #[cfg(windows)]
    checkout_temp.creation_flags(CREATE_NO_WINDOW);

    match checkout_temp.output().await {
        Ok(_) => {}
        Err(e) => {
            panic!("failed to checkout temp branch on remote: {:?}", e);
        }
    }

    let mut checkout = Command::new("git");
    checkout
        .arg("checkout")
        .arg("-b")
        .arg("main")
        .current_dir(&repo_path);

    #[cfg(windows)]
    checkout.creation_flags(CREATE_NO_WINDOW);

    match checkout.output().await {
        Ok(_) => {}
        Err(e) => {
            panic!("failed to checkout main branch: {:?}", e);
        }
    }

    let mut commit = Command::new("git");
    commit
        .arg("commit")
        .arg("--allow-empty")
        .arg("-m")
        .arg("initial commit")
        .current_dir(&repo_path);

    #[cfg(windows)]
    commit.creation_flags(CREATE_NO_WINDOW);

    match commit.output().await {
        Ok(_) => {}
        Err(e) => {
            panic!("failed to commit initial commit: {:?}", e);
        }
    }

    let mut push = Command::new("git");
    push.arg("push")
        .arg("-u")
        .arg("origin")
        .arg("main")
        .current_dir(&repo_path);

    #[cfg(windows)]
    push.creation_flags(CREATE_NO_WINDOW);

    match push.output().await {
        Ok(_) => {}
        Err(e) => {
            panic!("failed to push initial commit: {:?}", e);
        }
    }
}

pub async fn setup(schema_version: StorageSchemaVersion) -> anyhow::Result<TestServer> {
    info!("Setting up test server");
    initialize_test_repo().await;

    info!("Initialized test repo. Setting up app config.");
    let repo_path = TEST_DIR.join("test-local");
    let app_config = Arc::new(RwLock::new(AppConfig {
        user_display_name: "test_user".to_string(),
        repo_path: repo_path.to_str().unwrap().to_string(),
        pull_dlls: false,
        open_uproject_after_sync: false,
        ..Default::default()
    }));

    let config_file = TEST_DIR.join("config.yaml");

    let (exit_tx, exit_rx) = tokio::sync::oneshot::channel::<()>();

    let (frontend_op_tx, _) = std::sync::mpsc::channel();

    info!("Creating AWS client");
    let aws_client = AWSClient::from_static_creds(ACCESS_KEY, SECRET_KEY, None).await;

    info!("Created AWS client. Creating notification channel.");
    let (notification_tx, notification_rx) = std::sync::mpsc::channel();

    // start the operation worker
    info!("Starting operation worker");
    let (op_tx, op_rx) = mpsc::channel(32);
    let mut worker = RepoWorker::new(app_config.clone(), op_rx);
    tokio::spawn(async move {
        worker.run().await;
    });

    info!("Started operation worker. Creating channels for longtail, git, and gameserver.");
    let (longtail_tx, longtail_rx) = std::sync::mpsc::channel();
    let (git_tx, git_rx) = std::sync::mpsc::channel();
    let (gs_tx, _gs_rx) = std::sync::mpsc::channel();

    // start a notification logger
    info!("Starting notification logger");
    tokio::spawn(async move {
        while let Ok(msg) = notification_rx.recv() {
            info!("notification: {}", msg);
        }
    });

    info!("Started notification logger. Creating git logger.");
    tokio::spawn(async move {
        while let Ok(msg) = git_rx.recv() {
            info!("git: {}", msg);
        }
    });

    info!("Started git logger. Creating longtail logger.");
    tokio::spawn(async move {
        while let Ok(msg) = longtail_rx.recv() {
            info!("longtail: {:?}", msg);
        }
    });

    info!("Created longtail logger. Creating repo config.");
    let remote_path: PathBuf = TEST_DIR.join("test-remote");
    let repo_config = Arc::new(RwLock::new(RepoConfig {
        uproject_path: build_uproject_path(&remote_path)
            .into_os_string()
            .into_string()
            .unwrap(),
        trunk_branch: "main".to_string(),
        git_hooks_path: None,
    }));

    info!("Created app state. Creating artifact storage.");
    let dynamic_config = Arc::new(RwLock::new(DynamicConfig {
        kubernetes_cluster_name: "prototype-build".to_string(),
        ..Default::default()
    }));

    let mp = MockArtifactProvider::new();
    let storage = ArtifactStorage::new(Arc::new(mp), schema_version);

    info!("Created artifact storage. Creating app state.");
    let mut state = AppState::new(
        app_config,
        repo_config,
        dynamic_config,
        config_file,
        Some(storage),
        longtail_tx,
        op_tx,
        notification_tx,
        frontend_op_tx,
        String::from("0.0.0"),
        Some(aws_client),
        PathBuf::from_str("test-path").unwrap(),
        git_tx,
        gs_tx,
    )
    .await?;

    info!("[testing module] created app state");

    state.longtail.download_path = LocalDownloadPath(TEST_DIR.join("longtail-downloads"));

    if state.longtail.exec_path.is_none() && state.longtail.update_exec().is_err() {
        let tx_lock = state.longtail_tx.clone();
        if let Err(e) = state.longtail.get_longtail(tx_lock.clone()) {
            info!("failed to get longtail executable: {:?}", e);
            info!(
                "[testing module] failed to get longtail executable: {:?}",
                e
            );
        }
        _ = state.longtail.update_exec();
    };

    info!(
        "[testing module] longtail update done. exe path: {:?}",
        &state.longtail.exec_path
    );

    let state = Arc::new(state);

    let mut server = TestServer::new(state, exit_tx);

    server.start(exit_rx).await;

    Ok(server)
}

pub async fn teardown() {
    info!("Removing test repos");
    let repo_path = TEST_DIR.join("test-local");
    match std::fs::remove_dir_all(repo_path) {
        Ok(_) => {
            info!("removed test repo");
        }
        Err(e) => {
            info!("failed to remove test repo: {:?}", e);
        }
    }

    let remote_path = TEST_DIR.join("test-remote");
    match std::fs::remove_dir_all(remote_path) {
        Ok(_) => {
            info!("removed test remote");
        }
        Err(e) => {
            info!("failed to remove test remote: {:?}", e);
        }
    }

    let longtail_path = TEST_DIR.join("longtail-downloads");
    match std::fs::remove_dir_all(longtail_path) {
        Ok(_) => {
            info!("removed longtail downloads");
        }
        Err(e) => {
            info!("failed to remove longtail downloads: {:?}", e);
        }
    }
}

// local repo operations
#[allow(dead_code)]
pub async fn add_file(path: &str) {
    // add the file
    let output = Command::new("git")
        .arg("add")
        .arg(path)
        .current_dir(&TEST_DIR.join("test-local"))
        .output()
        .await;

    match output {
        Ok(output) => {
            let message = String::from_utf8_lossy(&output.stdout);
            info!("{}", message);
        }
        Err(e) => {
            info!("failed to add file: {:?}", e);
        }
    }
}

#[allow(dead_code)]
pub async fn commit(message: &str) {
    // commit the file
    let output = Command::new("git")
        .arg("commit")
        .arg("-m")
        .arg(message)
        .current_dir(&TEST_DIR.join("test-local"))
        .output()
        .await;

    match output {
        Ok(output) => {
            let message = String::from_utf8_lossy(&output.stdout);
            info!("{}", message);
        }
        Err(e) => {
            info!("failed to commit file: {:?}", e);
        }
    }
}

#[allow(dead_code)]
pub async fn push(branch: &str) {
    // push the file
    let output = Command::new("git")
        .arg("push")
        .arg("origin")
        .arg(branch)
        .current_dir(&TEST_DIR.join("test-local"))
        .output()
        .await;

    match output {
        Ok(output) => {
            let message = String::from_utf8_lossy(&output.stdout);
            info!("{}", message);
        }
        Err(e) => {
            info!("failed to push file: {:?}", e);
        }
    }
}

#[allow(dead_code)]
pub async fn get_latest_commit_message(branch: &str) -> String {
    // get the latest commit message
    let output = Command::new("git")
        .arg("log")
        .arg(branch)
        .arg("-1")
        .arg("--pretty=%B")
        .current_dir(&TEST_DIR.join("test-local"))
        .output()
        .await;

    match output {
        Ok(output) => {
            let message = String::from_utf8_lossy(&output.stdout);
            message.trim().to_string()
        }
        Err(e) => {
            panic!("failed to get latest commit message: {:?}", e);
        }
    }
}
