use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GitHubStatusResponse {
    /// Statuspage indicator: "none", "minor", "major", "critical", or "maintenance".
    pub indicator: String,
    /// Human-readable status, e.g. "All Systems Operational".
    pub description: String,
    /// Link to the public GitHub status page.
    pub url: String,
}
