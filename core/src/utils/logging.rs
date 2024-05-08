use std::fs;
use std::path::PathBuf;

use directories_next::ProjectDirs;
use tracing::level_filters::LevelFilter;

pub fn init(prefix: &str, app: &str) -> anyhow::Result<PathBuf> {
    use tracing_subscriber::{prelude::*, EnvFilter, Registry};

    let env_filter = EnvFilter::builder()
        .with_default_directive(LevelFilter::INFO.into())
        .from_env_lossy();
    let stdout_log = tracing_subscriber::fmt::layer().pretty();

    let subscriber = Registry::default().with(stdout_log).with(env_filter);

    let proj_dirs = ProjectDirs::from("", "", app).expect("Unable to get project dirs");
    let mut log_path = proj_dirs.data_dir().to_path_buf();
    log_path.push("logs");
    // Create the directory if needed
    if !log_path.exists() {
        fs::create_dir_all(&log_path)?;
    }

    let file_appender = tracing_appender::rolling::RollingFileAppender::builder()
        .filename_prefix(format!("{}-{}", app, prefix))
        .filename_suffix("log")
        .max_log_files(12)
        .rotation(tracing_appender::rolling::Rotation::HOURLY)
        .build(log_path.clone())
        .expect("file appender should build");

    let file_appender_layer = tracing_subscriber::fmt::layer()
        .json()
        .with_writer(file_appender);

    let subscriber = subscriber.with(file_appender_layer);
    tracing::subscriber::set_global_default(subscriber).expect("Unable to set global subscriber");
    log_panics::init();

    Ok(log_path)
}
