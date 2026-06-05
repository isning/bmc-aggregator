use std::sync::Arc;

use clap::Parser;
use tracing::info;

use bmc_aggregator::bmc::msd_storage::FsMsdStorage;
use bmc_aggregator::bmc::session::manager::StreamOrchestrator;
use bmc_aggregator::config::Config;
use bmc_aggregator::web::handlers::AppState;
use bmc_aggregator::web::routes::create_router;

#[derive(Parser)]
#[command(name = "bmc-aggregator")]
struct Cli {
    #[arg(short, long, default_value = "config.yaml")]
    config: String,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "bmc_aggregator=info,tower_http=info".into()),
        )
        .init();

    let cli = Cli::parse();
    let config = Config::load(&cli.config)?;
    info!("config loaded: {} device(s) configured", config.devices.len());

    let config_lock = Arc::new(tokio::sync::RwLock::new(config));

    // Clone config for AppState's own RwLock before moving config_lock into FsMsdStorage
    let config_for_state = config_lock.read().await.clone();

    let orchestrator = StreamOrchestrator::new();
    orchestrator.init_devices(&config_for_state.devices).await;

    let msd_storage_root = std::env::temp_dir().join("bmc-aggregator-msd").to_string_lossy().to_string();

    let http_client = reqwest::Client::builder()
        .build()?;

    let (event_tx, _) = tokio::sync::broadcast::channel::<String>(256);

    let state = Arc::new(AppState {
        config: tokio::sync::RwLock::new(config_for_state),
        config_path: cli.config.clone(),
        orchestrator,
        msd_runtime: tokio::sync::RwLock::new(std::collections::HashMap::new()),
        msd_storage: FsMsdStorage::new(msd_storage_root, config_lock),
        http_client,
        event_tx,
        active_downloads: tokio::sync::RwLock::new(std::collections::HashMap::new()),
    });
    let app = create_router(state.clone());

    let addr = state.config.read().await.server.listen.clone();
    info!("listening on {}", addr);
    let listener = tokio::net::TcpListener::bind(&addr).await?;
    axum::serve(listener, app).await?;

    Ok(())
}
