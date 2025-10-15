use axum::Router;
use clap::Parser;
use ethos_core::types::config::{FriendshipperConfig, OktaConfig};
use friendshipper_server::{ServerConfig, APP_NAME, VERSION};
#[allow(unused_imports)]
use jwt_authorizer::{Authorizer, IntoLayer, JwtAuthorizer, Refresh, RefreshStrategy};
use opentelemetry::global;
use opentelemetry_otlp::WithExportConfig;
use opentelemetry_sdk::{trace::Sampler, Resource};
use std::{env, time::Duration};
use tokio::net::TcpListener;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

pub const OTEL_TRACER_PROTOCOL: opentelemetry_otlp::Protocol = opentelemetry_otlp::Protocol::Grpc;
pub const OTEL_TRACER_TIMEOUT: Duration = Duration::from_secs(10);
pub const ETHOS_TRACE_EVENT_TARGET: &str = "ethos::trace";

#[derive(Debug, Parser)]
#[command(author, version, about, long_about = None)]
struct Args {
    #[arg(short, long, default_value_t = 3000)]
    port: u16,

    #[arg(short = 'f', long, required = true)]
    config_file_path: Option<String>,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = Args::parse();

    println!("we are starting");

    // Initialize tracing
    let registry = tracing_subscriber::registry()
        .with(tracing_subscriber::EnvFilter::new(
            std::env::var("RUST_LOG").unwrap_or_else(|_| "info".into()),
        ))
        .with(tracing_subscriber::fmt::layer());

    if let Ok(endpoint) = env::var("OTEL_EXPORTER_OTLP_ENDPOINT") {
        let tracer = opentelemetry_otlp::new_pipeline()
            .tracing()
            .with_trace_config(
                opentelemetry_sdk::trace::config()
                    .with_resource(Resource::new(vec![
                        opentelemetry::KeyValue::new(
                            "service.name",
                            APP_NAME.to_string().to_lowercase(),
                        ),
                        opentelemetry::KeyValue::new("service.version", VERSION),
                    ]))
                    .with_sampler(Sampler::AlwaysOn),
            )
            .with_exporter(
                opentelemetry_otlp::new_exporter()
                    .http()
                    .with_protocol(OTEL_TRACER_PROTOCOL)
                    .with_endpoint(endpoint)
                    .with_timeout(OTEL_TRACER_TIMEOUT),
            )
            .install_batch(opentelemetry_sdk::runtime::Tokio)
            .expect("otel tracing pipeline should install");

        registry
            .with(tracing_opentelemetry::layer().with_tracer(tracer))
            .init();
    } else {
        registry.init();
    }

    // Get OIDC endpoint from environment variable
    let oidc_endpoint = env::var("OIDC_ENDPOINT").expect("OIDC_ENDPOINT must be set");
    let oidc_client_id = env::var("OIDC_CLIENT_ID").expect("OIDC_CLIENT_ID must be set");

    let okta_config = OktaConfig {
        issuer: oidc_endpoint.clone(),
        client_id: oidc_client_id,
    };

    // Load friendshipper config from YAML file
    let config_path = args
        .config_file_path
        .expect("Config file path must be provided");
    let config_file = std::fs::File::open(config_path).expect("Failed to open config file");
    let friendshipper_config: FriendshipperConfig =
        serde_yaml::from_reader(config_file).expect("Failed to parse config file");

    // Update ServerConfig to include FriendshipperConfig
    let server_config = ServerConfig {
        role_to_assume: env::var("ROLE_TO_ASSUME").expect("ROLE_TO_ASSUME must be set"),
        friendshipper_config,
        okta_config,
    };

    // Build our application with a route
    let app: Router = friendshipper_server::router(oidc_endpoint)
        .await?
        .with_state(server_config);

    // Run it
    let listener = TcpListener::bind(("0.0.0.0", args.port)).await.unwrap();
    tracing::info!("listening on {:?}", listener.local_addr());

    axum::serve(listener, app.into_make_service())
        .await
        .unwrap();

    // Ensure all spans have been reported
    global::shutdown_tracer_provider();

    Ok(())
}
