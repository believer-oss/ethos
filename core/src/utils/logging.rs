use directories_next::ProjectDirs;
use opentelemetry_otlp::WithExportConfig;
use opentelemetry_sdk::trace::{Sampler, Tracer};
use opentelemetry_sdk::Resource;
use std::fs;
use std::path::PathBuf;
use std::time::Duration;
use tracing::level_filters::LevelFilter;
use tracing_opentelemetry::OpenTelemetryLayer;
use tracing_subscriber::layer::{Layered, SubscriberExt};
use tracing_subscriber::reload::Handle;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::{reload, EnvFilter, Registry};

pub const OTEL_TRACER_PROTOCOL: opentelemetry_otlp::Protocol = opentelemetry_otlp::Protocol::Grpc;
pub const OTEL_TRACER_TIMEOUT: Duration = Duration::from_secs(10);
pub const ETHOS_TRACE_EVENT_TARGET: &str = "ethos::trace";

pub type OtelReloadHandle = Handle<
    Option<OpenTelemetryLayer<Layered<EnvFilter, Registry, Registry>, Tracer>>,
    Layered<EnvFilter, Registry, Registry>,
>;

pub fn init(
    prefix: &str,
    app: &str,
    version: &str,
    username: Option<String>,
    otlp_endpoint: Option<String>,
    otlp_headers: Option<String>,
) -> anyhow::Result<(PathBuf, OtelReloadHandle)> {
    // OpenTelemetry
    let otel_layer = match otlp_endpoint {
        Some(endpoint) if !endpoint.is_empty() => {
            // if there are OTEL headers, set OTEL_EXPORTER_OTLP_HEADERS appropriately
            if let Some(headers) = otlp_headers {
                std::env::set_var("OTEL_EXPORTER_OTLP_HEADERS", headers);
            }

            let mut resource_attributes = vec![
                opentelemetry::KeyValue::new("service.name", app.to_string().to_lowercase()),
                opentelemetry::KeyValue::new("service.version", version.to_string()),
            ];

            if let Some(username) = username {
                resource_attributes.push(opentelemetry::KeyValue::new("user", username));
            }

            let tracer = opentelemetry_otlp::new_pipeline()
                .tracing()
                .with_trace_config(
                    opentelemetry_sdk::trace::config()
                        .with_resource(Resource::new(resource_attributes))
                        .with_sampler(Sampler::AlwaysOn),
                )
                .with_exporter(
                    opentelemetry_otlp::new_exporter()
                        .http()
                        .with_protocol(OTEL_TRACER_PROTOCOL)
                        .with_endpoint(endpoint.clone())
                        .with_timeout(OTEL_TRACER_TIMEOUT),
                )
                .install_batch(opentelemetry_sdk::runtime::Tokio)
                .expect("otel tracing pipeline should install");

            Some(tracing_opentelemetry::layer().with_tracer(tracer))
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
    let file_appender_layer = tracing_subscriber::fmt::layer()
        .json()
        .with_writer(file_appender);

    // Registry
    tracing_subscriber::registry()
        .with(env_filter)
        .with(otel_layer)
        .with(stdout_log)
        .with(file_appender_layer)
        .init();

    Ok((log_path, reload_handle))
}
