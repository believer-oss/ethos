use kube::api::ObjectList;

use anyhow::{anyhow, Context};
use futures::StreamExt;
use serde::Serialize;
use tracing::{debug, info, instrument, warn};

use crate::types::argo::workflow::Workflow;
use crate::types::errors::CoreError;
use crate::utils::junit::JunitOutput;

#[derive(Clone, Serialize)]
pub struct LogChunk {
    pub data: String,
    pub finished: bool,
    pub error: Option<String>,
}

#[derive(Debug, PartialEq)]
pub enum LogSource {
    ActivePod,        // Live pod logs
    ArtifactS3,       // S3 artifact logs
    ArchivedWorkflow, // Archived workflow logs
}

// Save some data by filtering fields
const ARGO_WORKFLOW_DEFAULT_FIELDS: &str = "items.metadata.name,items.metadata.annotations,items.metadata.labels,items.metadata.creationTimestamp,items.metadata.uid,items.status.phase,items.status.finishedAt,items.status.estimatedDuration,items.status.progress,items.status.startedAt";
const ARGO_WORKFLOW_NODES_FIELDS: &str = "metadata.name,metadata.namespace,metadata.uid,metadata.creationTimestamp,metadata.labels,metadata.annotations,spec,status";
pub const ARGO_WORKFLOW_REPO_LABEL_KEY: &str = "believer.dev/repo";
pub const ARGO_WORKFLOW_REF_LABEL_KEY: &str = "believer.dev/ref";
pub const ARGO_WORKFLOW_COMMIT_LABEL_KEY: &str = "believer.dev/commit";
pub const ARGO_WORKFLOW_PUSHER_LABEL_KEY: &str = "believer.dev/pusher";
pub const ARGO_WORKFLOW_MESSAGE_ANNOTATION_KEY: &str = "believer.dev/message";
pub const ARGO_WORKFLOW_COMPARE_ANNOTATION_KEY: &str = "believer.dev/compare";
pub const ARGO_WORKFLOW_HIDDEN_LABEL_KEY: &str = "believer.dev/friendshipper-hidden";
pub const ARGO_WORKFLOW_ARCHIVE_STATUS: &str = "workflows.argoproj.io/workflow-archiving-status";

#[derive(Debug, Clone)]
pub struct ArgoClient {
    host: String,
    client: reqwest::Client,
    auth: String,
    namespace: String,
}

impl ArgoClient {
    pub fn new(host: String, auth: String, namespace: String) -> Self {
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(3600)) // 1 hour timeout for streaming
            .build()
            .expect("Failed to build reqwest client");
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

    #[instrument(skip(self))]
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

    #[instrument(skip(self, workflow, callback))]
    pub async fn get_logs_for_workflow_node<F>(
        &self,
        workflow: &Workflow,
        node_id: &str,
        callback: Option<F>,
    ) -> Result<String, CoreError>
    where
        F: Fn(LogChunk) + Clone + Send,
    {
        let workflow_name = workflow
            .metadata
            .name
            .as_ref()
            .ok_or_else(|| CoreError::Internal(anyhow!("Workflow has no name")))?;
        let workflow_uid = workflow
            .metadata
            .uid
            .as_ref()
            .ok_or_else(|| CoreError::Internal(anyhow!("Workflow has no UID")))?;

        let log_source = self.determine_log_source(workflow, node_id);

        // Compute pod name for active pod logs
        let pod_name = self.get_pod_name(workflow, node_id);

        match log_source {
            LogSource::ActivePod => {
                let pod_name = pod_name.ok_or_else(|| {
                    CoreError::Internal(anyhow!("Failed to compute pod name for node {}", node_id))
                })?;

                if let Some(cb) = callback {
                    // Use streaming for active pods
                    self.get_active_workflow_logs_streaming(workflow_name, &pod_name, cb)
                        .await?;
                    Ok(String::new())
                } else {
                    // Try active pod logs first, fall back to artifacts, then archived
                    match self
                        .get_active_workflow_logs(workflow_name, &pod_name)
                        .await
                    {
                        Ok(logs) => Ok(logs),
                        Err(_) => {
                            match self
                                .get_artifact_logs(workflow_name, node_id, "main-logs")
                                .await
                            {
                                Ok(logs) => Ok(logs),
                                Err(_) => self.get_archived_logs(workflow_uid, node_id).await,
                            }
                        }
                    }
                }
            }
            LogSource::ArtifactS3 => {
                // Try artifact logs first, fall back to archived
                let logs = match self
                    .get_artifact_logs(workflow_name, node_id, "main-logs")
                    .await
                {
                    Ok(logs) => logs,
                    Err(_) => self.get_archived_logs(workflow_uid, node_id).await?,
                };

                if let Some(cb) = callback {
                    cb(LogChunk {
                        data: logs.clone(),
                        finished: true,
                        error: None,
                    });
                }
                Ok(logs)
            }
            LogSource::ArchivedWorkflow => {
                info!(
                    "Fetching archived workflow logs for uid: {}, node: {}",
                    workflow_uid, node_id
                );
                let logs = self.get_archived_logs(workflow_uid, node_id).await?;

                if let Some(cb) = callback {
                    cb(LogChunk {
                        data: logs.clone(),
                        finished: true,
                        error: None,
                    });
                }
                Ok(logs)
            }
        }
    }

    #[instrument(skip(self))]
    pub fn determine_log_source(&self, workflow: &Workflow, node_id: &str) -> LogSource {
        // Check if workflow is archived
        let archiving_status_label = workflow
            .metadata
            .labels
            .as_ref()
            .and_then(|labels| labels.get(ARGO_WORKFLOW_ARCHIVE_STATUS));

        if archiving_status_label.is_some() {
            debug!("Workflow is archived, using ArchivedWorkflow");
            return LogSource::ArchivedWorkflow;
        }

        // Find the specific node
        if let Some(nodes) = &workflow.status {
            if let Some(nodes_map) = &nodes.nodes {
                if let Some(node) = nodes_map.get(node_id) {
                    debug!(
                        "Found target node - phase: {}, has_outputs: {}",
                        node.phase,
                        node.outputs.is_some()
                    );

                    // Check if node has main-logs artifact
                    if let Some(outputs) = &node.outputs {
                        if let Some(artifacts) = &outputs.artifacts {
                            for artifact in artifacts {
                                if artifact.name == "main-logs" {
                                    debug!("Node has main-logs artifact, using ArtifactS3");
                                    return LogSource::ArtifactS3;
                                }
                            }
                        }
                        debug!("Node has outputs but no main-logs artifact");
                    }

                    // Check if it's a running pod (running nodes without artifacts are pods)
                    if node.phase == "Running" && node.outputs.is_none() {
                        debug!("Node is running with no outputs, using ActivePod");
                        return LogSource::ActivePod;
                    }
                } else {
                    debug!("Node {} not found in workflow nodes", node_id);
                }
            }
        }

        debug!("No specific criteria met, defaulting to ArchivedWorkflow");
        LogSource::ArchivedWorkflow
    }

    // Generate pod name using v2 format
    fn get_pod_name(&self, workflow: &Workflow, node_id: &str) -> Option<String> {
        use crate::types::argo::workflow::WorkflowNodeStatus;

        let workflow_name = workflow.metadata.name.as_ref()?;
        let nodes = workflow.status.as_ref()?.nodes.as_ref()?;
        let node: &WorkflowNodeStatus = nodes.get(node_id)?;

        // Convert containerSet node name to pod node name by removing ".<containerName>" postfix
        let pod_node_name = if node.node_type.as_deref() == Some("Container") {
            node.name
                .rsplit_once('.')
                .map(|(prefix, _)| prefix)
                .unwrap_or(&node.name)
        } else {
            &node.name
        };

        // If workflow name equals node name, use workflow name
        if workflow_name == pod_node_name {
            return Some(workflow_name.clone());
        }

        // Get template name from node (check TemplateRef first, like upstream)
        let template_name = node
            .template_ref
            .as_ref()
            .and_then(|tr| tr.template.as_ref())
            .or(node.template_name.as_ref())
            .map(|s| s.as_str())
            .unwrap_or("");

        debug!(
            "Pod name computation: workflow={}, node_name={}, pod_node_name={}, template_name={}",
            workflow_name, node.name, pod_node_name, template_name
        );

        // Build prefix
        let mut prefix = workflow_name.clone();
        if !template_name.is_empty() {
            prefix = format!("{}-{}", prefix, template_name);
        }

        // Ensure prefix length (max 243 chars for k8s naming)
        const MAX_PREFIX_LENGTH: usize = 243;
        if prefix.len() > MAX_PREFIX_LENGTH - 1 {
            prefix.truncate(MAX_PREFIX_LENGTH - 1);
        }

        // Create FNV-1a 32-bit hash of pod node name (matching Go's fnv.New32a())
        let hash = self.fnv_hash_32a(pod_node_name);

        let pod_name = format!("{}-{}", prefix, hash);
        debug!("Computed pod name: {}", pod_name);

        Some(pod_name)
    }

    // FNV-1a 32-bit hash implementation (matching Go's hash/fnv.New32a())
    // There is a 64-bit implementation in the 'fnv' crate, but it does not include a non-usize
    // option.
    fn fnv_hash_32a(&self, input: &str) -> u32 {
        // https://en.wikipedia.org/wiki/Fowler%E2%80%93Noll%E2%80%93Vo_hash_function#FNV_hash_parameters
        const FNV_OFFSET_BASIS: u32 = 2166136261;
        const FNV_PRIME: u32 = 16777619;

        let mut hash = FNV_OFFSET_BASIS;
        for byte in input.as_bytes() {
            hash ^= *byte as u32;
            hash = hash.wrapping_mul(FNV_PRIME);
        }
        hash
    }

    #[instrument(skip(self))]
    pub async fn get_artifact_logs(
        &self,
        workflow_name: &str,
        node_id: &str,
        artifact_name: &str,
    ) -> Result<String, CoreError> {
        let url = format!(
            "{}/artifact-files/{}/workflows/{}/{}/outputs/{}",
            self.host, self.namespace, workflow_name, node_id, artifact_name
        );

        let response = self
            .client
            .get(&url)
            .header("Authorization", format!("Bearer {}", self.auth))
            .send()
            .await?;

        if response.status().is_client_error() || response.status().is_server_error() {
            let body = response.text().await?;
            return Err(CoreError::Internal(anyhow!(
                "Artifact logs not available: {}",
                body
            )));
        }

        Ok(response.text().await?)
    }

    #[instrument(skip(self))]
    pub async fn get_archived_logs(&self, uid: &str, node_id: &str) -> Result<String, CoreError> {
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

        if response.status().is_client_error() || response.status().is_server_error() {
            let body = response.text().await?;
            return Err(CoreError::Internal(anyhow!(
                "Archived workflow logs not available: {}",
                body
            )));
        }

        Ok(response.text().await?)
    }

    #[instrument(skip(self))]
    pub async fn get_active_workflow_logs(
        &self,
        workflow_name: &str,
        pod_name: &str,
    ) -> Result<String, CoreError> {
        let url = format!(
            "{}/api/v1/workflows/{}/{}/log",
            self.host, self.namespace, workflow_name
        );

        let response = self
            .client
            .get(&url)
            .timeout(std::time::Duration::from_secs(10))
            .query(&[("podName", pod_name), ("logOptions.container", "main")])
            .header("Authorization", format!("Bearer {}", self.auth))
            .send()
            .await?;

        if response.status().is_client_error() || response.status().is_server_error() {
            let body = response.text().await?;
            return Err(CoreError::Internal(anyhow!(
                "Active workflow logs not available: {}",
                body
            )));
        }

        Ok(response.text().await?)
    }

    pub async fn get_active_workflow_logs_streaming(
        &self,
        workflow_name: &str,
        pod_name: &str,
        channel: impl Fn(LogChunk) + Clone + Send,
    ) -> Result<(), CoreError> {
        let url = format!(
            "{}/api/v1/workflows/{}/{}/log",
            self.host, self.namespace, workflow_name
        );
        info!(
            "Starting workflow log streaming for {}/{}",
            workflow_name, pod_name
        );

        debug!("Making HTTP request to: {}", url);
        let response = self
            .client
            .get(&url)
            .query(&[
                ("podName", pod_name),
                ("logOptions.container", "main"),
                ("grep", ""),
                ("logOptions.follow", "true"),
            ])
            .header("Authorization", format!("Bearer {}", self.auth))
            .send()
            .await?;

        debug!("Got HTTP response, status: {}", response.status());

        if response.status().is_client_error() || response.status().is_server_error() {
            debug!("HTTP error response, reading error body...");
            let body = response.text().await?;
            debug!("Error body: {}", body);
            channel(LogChunk {
                data: String::new(),
                finished: true,
                error: Some(format!("Active workflow logs not available: {}", body)),
            });
            return Ok(());
        }

        debug!("HTTP response successful, starting to read stream...");

        // Process streaming chunks as they arrive
        let mut stream = response.bytes_stream();
        debug!("Starting to read stream chunks...");

        while let Some(chunk_result) = stream.next().await {
            match chunk_result {
                Ok(chunk) => {
                    let chunk_str = String::from_utf8_lossy(&chunk);
                    debug!("Received chunk of {} bytes: {:?}", chunk.len(), chunk_str);
                    if !chunk_str.is_empty() {
                        // Split on newlines since each line is a separate JSON object
                        for line in chunk_str.lines() {
                            if !line.trim().is_empty() {
                                // Try to parse each line as JSON
                                if let Ok(json_value) =
                                    serde_json::from_str::<serde_json::Value>(line)
                                {
                                    if let Some(result) = json_value.get("result") {
                                        if let Some(content) =
                                            result.get("content").and_then(|c| c.as_str())
                                        {
                                            debug!("Extracted log content: {:?}", content);
                                            channel(LogChunk {
                                                data: content.to_string(),
                                                finished: false,
                                                error: None,
                                            });
                                        }
                                    }
                                } else {
                                    debug!("Failed to parse line as JSON: {:?}", line);
                                }
                            }
                        }
                    }
                }
                Err(e) => {
                    debug!("Stream error: {}", e);

                    // Check if this is a connection closed error (normal when pod completes)
                    let is_connection_closed = e.is_body() || e.is_connect();

                    if is_connection_closed {
                        debug!("Stream closed (pod likely completed)");
                        channel(LogChunk {
                            data: String::new(),
                            finished: true,
                            error: None,
                        });
                    } else {
                        warn!("Unexpected stream error: {}", e);
                        channel(LogChunk {
                            data: String::new(),
                            finished: true,
                            error: Some(e.to_string()),
                        });
                    }
                    return Ok(());
                }
            }
        }

        debug!("Stream ended");
        channel(LogChunk {
            data: String::new(),
            finished: true,
            error: None,
        });

        Ok(())
    }

    #[instrument(skip(self))]
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

    #[instrument(skip(self))]
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

    #[instrument(skip(self))]
    pub async fn get_workflow_with_nodes(&self, name: &str) -> Result<Workflow, CoreError> {
        let url = format!("{}/api/v1/workflows/{}/{}", self.host, self.namespace, name);
        let response = self
            .client
            .get(&url)
            .query(&[("fields", ARGO_WORKFLOW_NODES_FIELDS)]) // skip managedFields
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
