use ethos_core::middleware::nonce::{NONCE, NONCE_HEADER};
use ethos_core::storage::ArtifactList;

mod common;

#[tokio::test(flavor = "multi_thread")]
async fn test_v1_builds() -> Result<(), Box<dyn std::error::Error>> {
    let mut server = common::setup("v1".parse().unwrap()).await?;

    let client = reqwest::Client::new();
    let resp = client
        .get("http://localhost:8585/builds")
        .header(NONCE_HEADER, NONCE.to_string())
        .send()
        .await?;

    let artifact_list = resp.json::<ArtifactList>().await?;

    println!("{:?}", artifact_list);
    assert_eq!(artifact_list.entries.len(), 4);
    assert_eq!(artifact_list.method_prefix, "file://".into());
    assert_eq!(
        artifact_list.entries[0].key.0,
        "v1/believerco-gameprototypemp/client/win64/development/3deadbeef90deadbeef90deadbeef90deadbeef9.json"
    );
    assert_eq!(
        artifact_list.entries[0].display_name,
        "client-win64-3deadbee"
    );
    assert_eq!(
        artifact_list.entries[0].commit,
        Some("3deadbeef90deadbeef90deadbeef90deadbeef9".to_string())
    );

    server.shutdown().await;

    common::teardown().await;

    Ok(())
}
