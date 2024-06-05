use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CharacterMetadata {
    pub code_name: String,
    pub character_name: String,
    pub rigs: HashMap<String, String>,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RigsMetadata {
    pub body: String,
    pub face: String,
    pub weapon_primary: String,
    pub weapon_secondary: String,
}
