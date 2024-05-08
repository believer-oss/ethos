use std::sync::Arc;

use parking_lot::RwLock;

use ethos_core::types::config::{DynamicConfig, RepoConfig};
pub use router::router;

pub mod router;

pub type RepoConfigRef = Arc<RwLock<RepoConfig>>;
pub type DynamicConfigRef = Arc<RwLock<DynamicConfig>>;
