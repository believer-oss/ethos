//! Shared type definitions for the friendshipper client/server boundary.
//!
//! This crate exists so that `friendshipper-server` can consume the wire contract
//! without depending on `ethos-core`, which carries tauri and its GUI toolchain.
//! Keep the dependency list here minimal.

pub mod aws;
pub mod config;
pub mod errors;
pub mod github;
