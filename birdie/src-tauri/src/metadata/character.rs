use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CharacterMetadata {
    pub code_name: String,
    pub character_name: String,
    pub rigs: HashMap<String, String>,
    pub animations: HashMap<String, String>,
    pub meshes: HashMap<String, String>,
}
