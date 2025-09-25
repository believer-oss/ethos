use kube::api::ObjectList;

use anyhow::{anyhow, Context};
use tracing::warn;

use crate::types::argo::workflow::Workflow;
use crate::types::errors::CoreError;
use crate::utils::junit::JunitOutput;

// Save some data by filtering fields
const ARGO_WORKFLOW_DEFAULT_FIELDS: &str = "items.metadata.name,items.metadata.annotations,items.metadata.labels,items.metadata.creationTimestamp,items.metadata.uid,items.status.phase,items.status.finishedAt,items.status.estimatedDuration,items.status.progress,items.status.startedAt";
pub const ARGO_WORKFLOW_REPO_LABEL_KEY: &str = "believer.dev/repo";
pub const ARGO_WORKFLOW_REF_LABEL_KEY: &str = "believer.dev/ref";
pub const ARGO_WORKFLOW_COMMIT_LABEL_KEY: &str = "believer.dev/commit";
pub const ARGO_WORKFLOW_PUSHER_LABEL_KEY: &str = "believer.dev/pusher";
pub const ARGO_WORKFLOW_MESSAGE_ANNOTATION_KEY: &str = "believer.dev/message";
pub const ARGO_WORKFLOW_COMPARE_ANNOTATION_KEY: &str = "believer.dev/compare";
pub const ARGO_WORKFLOW_HIDDEN_LABEL_KEY: &str = "believer.dev/friendshipper-hidden";

#[derive(Debug, Clone)]
pub struct ArgoClient {
    host: String,
    client: reqwest::Client,
    auth: String,
    namespace: String,
}

impl ArgoClient {
    pub fn new(host: String, auth: String, namespace: String) -> Self {
        let client = reqwest::Client::new();
        Self {
            host,
            client,
            auth,
            namespace,
        }
    }

    pub fn set_auth(&mut self, auth: String) {
        self.auth = auth;
    }

    pub async fn get_workflows(
        &self,
        selected_artifact_project: &str,
    ) -> Result<Vec<Workflow>, CoreError> {
        let url = format!("{}/api/v1/workflows/{}", self.host, self.namespace);
        // TODO: We currently only write the project in REPO_LABEL_KEY, but it would be ideal if
        // selected_artifact_project and this label were the same. This will fail if the owner
        // includes a hyphen.
        let (_owner, project) = selected_artifact_project
            .split_once('-')
            .context("Invalid selected_artifact_project name")?;

        let response = self
            .client
            .get(&url)
            .query(&[
                ("fields", ARGO_WORKFLOW_DEFAULT_FIELDS),
                (
                    "listOptions.labelSelector",
                    &format!(
                        "{}!={},{}={}",
                        ARGO_WORKFLOW_HIDDEN_LABEL_KEY,
                        "true", // filter out hidden workflows
                        ARGO_WORKFLOW_REPO_LABEL_KEY,
                        project,
                    ),
                ),
                ("listOptions.limit", &format!("{}", 250)),
            ])
            .header("Authorization", format!("Bearer {}", self.auth))
            .send()
            .await?;

        if response.status().is_client_error() {
            let body = response.text().await?;
            return Err(CoreError::Internal(anyhow!(body)));
        }

        let mut workflows: ObjectList<Workflow> = match response.json().await {
            Err(e) => {
                return Err(e.into());
            }
            Ok(workflows) => workflows,
        };

        workflows.items.sort_by(|a, b| {
            if a.metadata.creation_timestamp == b.metadata.creation_timestamp {
                a.metadata.name.cmp(&b.metadata.name)
            } else {
                a.metadata
                    .creation_timestamp
                    .cmp(&b.metadata.creation_timestamp)
            }
        });

        // sort descending
        workflows.items.reverse();

        Ok(workflows.items)
    }

    pub async fn get_logs_for_workflow_node(
        &self,
        uid: &str,
        node_id: &str,
    ) -> Result<String, CoreError> {
        let url = format!(
            "{}/artifact-files/{}/archived-workflows/{}/{}/outputs/main-logs",
            self.host, self.namespace, uid, node_id
        );
        let response = self
            .client
            .get(&url)
            .header("Authorization", format!("Bearer {}", self.auth))
            .send()
            .await?;

        Ok(response.text().await?)
    }

    pub async fn get_junit_artifact_for_workflow_node(
        &self,
        uid: &str,
        node_id: &str,
    ) -> Result<Option<JunitOutput>, CoreError> {
        let url = format!(
            "{}/artifact-files/{}/archived-workflows/{}/{}/outputs/junit-xml",
            self.host, self.namespace, uid, node_id
        );
        let response = self
            .client
            .get(&url)
            .header("Authorization", format!("Bearer {}", self.auth))
            .send()
            .await;

        match response {
            Ok(resp) => {
                if resp.status() == 404 {
                    warn!("No junit artifact found for workflow node {}", node_id);
                    return Ok(None);
                }

                let text = resp.text().await?;
                let junit_output = JunitOutput::new_from_xml_str(&text)?;
                Ok(Some(junit_output))
            }
            Err(e) => Err(CoreError::Internal(anyhow!(e))),
        }
    }

    pub async fn stop_workflow(&self, workflow: &str) -> Result<String, CoreError> {
        let url = format!(
            "{}/api/v1/workflows/{}/{}/stop",
            self.host, self.namespace, workflow
        );
        let response = self
            .client
            .put(&url)
            .header("Authorization", format!("Bearer {}", self.auth))
            .send()
            .await?;

        Ok(response.text().await?)
    }

    pub async fn get_workflow_with_nodes(&self, name: &str) -> Result<Workflow, CoreError> {
        let url = format!("{}/api/v1/workflows/{}/{}", self.host, self.namespace, name);
        let response = self
            .client
            .get(&url)
            .header("Authorization", format!("Bearer {}", self.auth))
            .send()
            .await?;

        if response.status().is_client_error() {
            let body = response.text().await?;
            return Err(CoreError::Internal(anyhow!(body)));
        }

        let workflow: Workflow = response.json().await?;
        Ok(workflow)
    }
}
