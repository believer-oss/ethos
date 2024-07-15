pub use clients::aws::AWSClient;

pub mod auth;
pub mod clients;
pub mod fs;
pub mod longtail;
pub mod middleware;
pub mod msg;
pub mod operations;
pub mod storage;
pub mod tauri;
pub mod types;
pub mod utils;
pub mod worker;

pub const CREATE_NO_WINDOW: u32 = 0x08000000;

pub static ETHOS_APP_NAME: &str = "ethos";

const DYNAMIC_CONFIG_KEY: &str = "friendshipper/dynamic-config.json";
static AWS_REGION: &str = "us-west-2";
pub const KUBE_SHA_LABEL_KEY: &str = "believer.dev/commit";
pub static AWS_ACCOUNT_ID: &str = match option_env!("AWS_ACCOUNT_ID") {
    Some(account_id) => account_id,
    None => "",
};
pub static AWS_ROLE_NAME: &str = match option_env!("AWS_ROLE_NAME") {
    Some(role_name) => role_name,
    None => "",
};
pub static AWS_SSO_START_URL: &str = match option_env!("AWS_SSO_START_URL") {
    Some(sso_start_url) => sso_start_url,
    None => "",
};

#[cfg(target_os = "windows")]
pub const BIN_SUFFIX: &str = ".exe";
#[cfg(target_os = "linux")]
pub const BIN_SUFFIX: &str = "friendshipper-linux-amd64";
#[cfg(target_os = "macos")]
pub const BIN_SUFFIX: &str = "friendshipper-darwin-amd64";

// Longtail download info
const LONGTAIL_VERSION: &str = "v0.4.2";
#[cfg(target_os = "windows")]
const LONGTAIL_SHA256: &str = "2cb9396d4a09f7083dc5909296944c155415eb4e3c7cbb79dd7d835a2db46e25";
#[cfg(target_os = "linux")]
const LONGTAIL_SHA256: &str = "ea702f4236b7d7edb0619101a1ead350437d5d43ad138ef90ed925303fd2d4fd";
#[cfg(target_os = "macos")]
const LONGTAIL_SHA256: &str = "cf71bb53f1b2819e5aebfcd1b7cbfbf24e88b4bc8ecdf1147bac6fdc4e56d92a";
const LONGTAIL_DL_PREFIX: &str = "https://github.com/DanEngelbrecht/golongtail/releases/download";

#[cfg(test)]
mod tests {}
