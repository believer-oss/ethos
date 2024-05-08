use core::fmt;

use lazy_static::lazy_static;
use regex::Regex;
use serde::{Deserialize, Serialize};

lazy_static! {
    static ref SHA_REGEX: Regex = Regex::new(r"^[0-9a-f]{8}$").unwrap();
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Commit {
    pub sha: String,
    pub author: Option<String>,
    pub message: Option<String>,
    pub timestamp: Option<String>,
}

impl fmt::Display for Commit {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.sha)
    }
}

impl TryFrom<String> for Commit {
    type Error = &'static str;

    fn try_from(s: String) -> Result<Self, Self::Error> {
        if SHA_REGEX.is_match(&s) {
            Ok(Commit {
                sha: s,
                author: None,
                message: None,
                timestamp: None,
            })
        } else {
            Err("Must pass valid short sha for commit")
        }
    }
}
