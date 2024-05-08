use test_log::test;
use tokio::{fs, io::AsyncWriteExt};
use tracing::info;

use ethos_core::middleware::nonce::{NONCE, NONCE_HEADER};
use ethos_core::types::repo::{PushRequest, RepoStatus};

mod common;

#[test(tokio::test(flavor = "multi_thread"))]
async fn test_status_endpoint() -> Result<(), Box<dyn std::error::Error>> {
    info!("Starting test_status_endpoint");
    let mut server = common::setup("v1".parse().unwrap()).await?;

    info!("Server started. Initiating request.");
    let client = reqwest::Client::new();
    let resp = client
        .get("http://localhost:8585/repo/status")
        .header(NONCE_HEADER, NONCE.to_string())
        .send()
        .await?;

    info!("Request complete. Checking response.");
    let status = resp.json::<RepoStatus>().await?;

    assert_eq!(status.branch, "main");
    assert_eq!(status.remote_branch, "origin/main");

    server.shutdown().await;

    common::teardown().await;

    Ok(())
}

#[test(tokio::test(flavor = "multi_thread"))]
async fn test_new_file_workflow() -> Result<(), Box<dyn std::error::Error>> {
    info!("Starting test_new_file_workflow");
    let mut server = common::setup("v1".parse().unwrap()).await?;

    let path = common::TEST_DIR.join("test-local").join("test.txt");

    // open file and write "foo" to it
    info!("Creating file");
    let mut file = fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(&path)
        .await?;

    info!("Writing to file");
    file.write_all(b"foo").await?;
    file.sync_all().await?;

    info!("Sending request");
    let client = reqwest::Client::new();
    let resp = client
        .get("http://localhost:8585/repo/status")
        .header(NONCE_HEADER, NONCE.to_string())
        .send()
        .await?;

    info!("Checking response");
    let status = resp.json::<RepoStatus>().await?;

    // file currently untracked
    assert!(status.untracked_files.contains("test.txt"));
    assert!(status.modified_files.is_empty());

    // ??
    let file_info = status.untracked_files.into_iter().next().unwrap();
    assert_eq!(file_info.working_state, String::from("?"));
    assert_eq!(file_info.index_state, String::from("?"));

    // add file
    common::add_file("test.txt").await;

    let resp = client
        .get("http://localhost:8585/repo/status")
        .header(NONCE_HEADER, NONCE.to_string())
        .send()
        .await?;

    let status = resp.json::<RepoStatus>().await?;

    // file staged
    assert!(status.untracked_files.is_empty());
    assert!(status.modified_files.contains("test.txt"));

    // A
    let file_info = status.modified_files.into_iter().next().unwrap();
    assert_eq!(file_info.working_state, String::from(""));
    assert_eq!(file_info.index_state, String::from("A"));

    // commit
    common::commit("test commit").await;

    let resp = client
        .get("http://localhost:8585/repo/status")
        .header(NONCE_HEADER, NONCE.to_string())
        .send()
        .await?;

    let status = resp.json::<RepoStatus>().await?;

    // no files
    assert!(status.untracked_files.is_empty());
    assert!(status.modified_files.is_empty());
    assert_eq!(status.commits_ahead, 1);

    // push
    common::push("main").await;

    let resp = client
        .get("http://localhost:8585/repo/status")
        .header(NONCE_HEADER, NONCE.to_string())
        .send()
        .await?;

    let status = resp.json::<RepoStatus>().await?;

    // no files
    assert!(status.untracked_files.is_empty());
    assert!(status.modified_files.is_empty());
    assert_eq!(status.commits_ahead, 0);

    // modified files
    file.write_all(b"bar").await?;
    file.sync_all().await?;

    let client = reqwest::Client::new();
    let resp = client
        .get("http://localhost:8585/repo/status")
        .header(NONCE_HEADER, NONCE.to_string())
        .send()
        .await?;

    let status = resp.json::<RepoStatus>().await?;
    assert!(status.untracked_files.is_empty());
    assert!(status.modified_files.contains("test.txt"));
    assert_eq!(status.commits_ahead, 0);

    server.shutdown().await;

    common::teardown().await;

    Ok(())
}

#[test(tokio::test(flavor = "multi_thread"))]
async fn test_unreal_workflow() -> Result<(), Box<dyn std::error::Error>> {
    info!("Starting test_unreal_workflow");
    let mut server = common::setup("v1".parse().unwrap()).await?;

    let path = common::TEST_DIR.join("test-local").join("test.txt");

    // open file and write "foo" to it
    info!("Creating file");
    let mut file = fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(&path)
        .await?;

    info!("Writing to file");
    file.write_all(b"foo").await?;
    file.sync_all().await?;

    let body = PushRequest {
        files: vec![String::from("test.txt")],
        commit_message: String::from("test commit"),
    };

    info!("Sending request");
    let client = reqwest::Client::new();
    client
        .post("http://localhost:8585/repo/push")
        .header(NONCE_HEADER, NONCE.to_string())
        .header("Content-Type", "application/json")
        .body(serde_json::to_string(&body)?)
        .send()
        .await?;

    // check the status
    info!("Checking status");
    let resp = client
        .get("http://localhost:8585/repo/status")
        .header(NONCE_HEADER, NONCE.to_string())
        .send()
        .await?;

    info!("Checking response");
    let status = resp.json::<RepoStatus>().await?;
    info!("{:?}", status);

    assert!(status.modified_files.is_empty());
    assert_eq!(status.commits_ahead, 0);

    assert_eq!(
        common::get_latest_commit_message("origin/main").await,
        String::from("test commit")
    );

    server.shutdown().await;

    common::teardown().await;

    Ok(())
}
