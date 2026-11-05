use std::future::IntoFuture;
use std::sync::Arc;

use clap::Parser;
use tracing::info;

use bmc_aggregator::bmc::feature::device_registry::DeviceRegistry;
use bmc_aggregator::bmc::feature::registered_backends;
use bmc_aggregator::bmc::feature::video::webtransport;
use bmc_aggregator::bmc::msd_storage::FsMsdStorage;
use bmc_aggregator::config::Config;
use bmc_aggregator::web::routes::create_router;
use bmc_aggregator::web::state::AppState;

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
    info!(
        "config loaded: {} device(s) configured",
        config.devices.len()
    );

    let config_lock = Arc::new(tokio::sync::RwLock::new(config));
    let config_for_state = config_lock.read().await.clone();

    // Initialize DeviceRegistry with feature backends
    let registry = DeviceRegistry::new();
    let backends = registered_backends();
    registry.init(&config_for_state.devices, backends).await;

    let msd_storage_root = config_for_state
        .server
        .data_dir
        .join("msd")
        .to_string_lossy()
        .to_string();
    let state = Arc::new(AppState {
        registry,
        config: config_lock.clone(),
        config_path: cli.config.clone(),
        configuration: bmc_aggregator::bmc::configuration::ConfigurationCoordinator::new(),
        msd_storage: FsMsdStorage::new(msd_storage_root, config_lock),
        active_downloads: tokio::sync::RwLock::new(std::collections::HashMap::new()),
        webtransport: tokio::sync::RwLock::new(None),
    });
    let app = create_router(state.clone());
    let option_monitor =
        bmc_aggregator::bmc::configuration::start_configuration_option_monitor(state.clone());

    let addr = state.config.read().await.server.listen.clone();
    info!("listening on {}", addr);
    let listener = tokio::net::TcpListener::bind(&addr).await?;
    let local_addr = listener.local_addr()?;
    if !local_addr.ip().is_loopback() && !state.config.read().await.server.allow_insecure_http {
        anyhow::bail!(
            "refusing plain HTTP listener {local_addr}; bind to loopback behind a TLS reverse proxy or explicitly set server.allow_insecure_http=true"
        );
    }
    let webtransport_state = state.clone();
    let webtransport_task = tokio::spawn(async move {
        if let Err(e) = webtransport::serve(webtransport_state).await {
            tracing::warn!("webtransport server stopped: {e}");
        }
    });
    let (shutdown_tx, mut shutdown_rx) = tokio::sync::watch::channel(false);
    let server = axum::serve(listener, app)
        .with_graceful_shutdown(async move {
            let _ = shutdown_rx.changed().await;
        })
        .into_future();
    tokio::pin!(server);

    tokio::select! {
        result = &mut server => {
            webtransport_task.abort();
            result?;
        }
        _ = shutdown_signal() => {
            // Stop accepting HTTP connections and immediately release feature
            // runtimes. MJPEG workers watch their FeatureConnection stop
            // channel, so persistent multipart responses cannot postpone the
            // AVCT close record until systemd's hard-stop deadline.
            let _ = shutdown_tx.send(true);
            webtransport_task.abort();
            info!("shutdown requested; stopping device lifecycles");
            let cancellations: Vec<_> = state
                .active_downloads
                .write()
                .await
                .drain()
                .filter_map(|(_, mut task)| task.cancellation.take())
                .collect();
            for cancellation in cancellations {
                let _ = cancellation.send(());
            }
            state.registry.shutdown().await;
            option_monitor.abort();
            server.await?;
        }
    }

    Ok(())
}

async fn shutdown_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{signal, SignalKind};

        match signal(SignalKind::terminate()) {
            Ok(mut terminate) => tokio::select! {
                _ = tokio::signal::ctrl_c() => {}
                _ = terminate.recv() => {}
            },
            Err(error) => {
                tracing::warn!("SIGTERM handler unavailable: {error}");
                let _ = tokio::signal::ctrl_c().await;
            }
        }
    }

    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}
