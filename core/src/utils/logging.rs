use std::{env, fs};
use std::path::PathBuf;
use std::time::Duration;
use directories_next::ProjectDirs;
use opentelemetry_otlp::WithExportConfig;
use opentelemetry_sdk::trace::{Tracer};
use tracing::level_filters::LevelFilter;
use tracing_opentelemetry::OpenTelemetryLayer;
use tracing_subscriber::{EnvFilter, Registry, reload};
use tracing_subscriber::layer::{SubscriberExt};
use tracing_subscriber::reload::Handle;
use tracing_subscriber::util::SubscriberInitExt;

pub const OTEL_TRACER_PROTOCOL: opentelemetry_otlp::Protocol = opentelemetry_otlp::Protocol::Grpc;
pub const OTEL_TRACER_TIMEOUT: Duration = Duration::from_secs(10);
pub const ETHOS_TRACE_EVENT_TARGET: &str = "ethos::trace";

// Type mismatch [E0308]expected `OtelReloadHandle`, but found `Handle<Option<Filtered<OpenTelemetryLayer<Layered<EnvFilter, Registry, Registry>, Tracer>, FilterFn<fn(&Metadata) -> bool>, Layered<EnvFilter, Registry, Registry>>>, Layered<EnvFilter, Registry, Registry>>`

pub type OtelReloadHandle = Handle<Option<OpenTelemetryLayer<Registry, Tracer>>, Registry>;
pub fn init(prefix: &str, app: &str) -> anyhow::Result<(PathBuf, OtelReloadHandle)> {
    // OpenTelemetry
    let otel_layer = match env::var("OTEL_EXPORTER_OTLP_ENDPOINT") {
        Ok(endpoint) if !endpoint.is_empty() => {
            let tracer = opentelemetry_otlp::new_pipeline()
                .tracing()
                .with_exporter(
                    opentelemetry_otlp::new_exporter()
                        .http()
                        .with_protocol(OTEL_TRACER_PROTOCOL)
                        .with_endpoint(endpoint)
                        .with_timeout(OTEL_TRACER_TIMEOUT),
                )
                .install_batch(opentelemetry_sdk::runtime::Tokio)
                .expect("otel tracing pipeline should install");
            let otel_layer = tracing_opentelemetry::layer()
                .with_tracer(tracer);

            Some(otel_layer)
        }
        _ => None,
    };

    let (otel_layer, reload_handle) = reload::Layer::new(otel_layer);

    let env_filter = EnvFilter::builder()
        .with_default_directive(LevelFilter::INFO.into())
        .from_env_lossy();
    let stdout_log = tracing_subscriber::fmt::layer().pretty();

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
    let (file_appender, _file_appender_guard) = tracing_appender::non_blocking(file_appender);
    let file_appender_layer = tracing_subscriber::fmt::layer()
        .json()
        .with_writer(file_appender);

    // Registry
    tracing_subscriber::registry()
        .with(otel_layer)
        .with(stdout_log)
        .with(env_filter)
        .with(file_appender_layer)
        .init();

    Ok((log_path, reload_handle))
}
