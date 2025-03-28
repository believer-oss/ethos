pub mod auto_merge_submit;
pub mod commits;
pub mod merge_queue;
pub mod pulls;
pub mod submit;
mod user;

pub use auto_merge_submit::auto_merge_submit_handler;
pub use commits::get_commit_statuses;
pub use merge_queue::get_merge_queue;
pub use pulls::{get_pull_request, get_pull_requests};
pub use submit::submit_handler;
pub use user::get_user;
