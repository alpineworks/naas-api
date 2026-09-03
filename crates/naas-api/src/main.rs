use std::process::ExitCode;

use anyhow::{Context, Result};
use clap::Parser;
use tokio_util::sync::CancellationToken;

mod config;
mod error;
mod handlers;
mod ops;
mod pool;
mod reader;
mod telemetry;

use config::Config;

fn main() -> ExitCode {
    let cfg = Config::parse();

    if let Err(e) = telemetry::init_tracing(&cfg) {
        eprintln!("failed to initialize tracing: {e:#}");
        return ExitCode::from(1);
    }

    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            tracing::error!(error = %e, "failed to build tokio runtime");
            return ExitCode::from(1);
        }
    };

    match runtime.block_on(run(cfg)) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            tracing::error!(error = ?e, "fatal: {e:#}");
            ExitCode::from(1)
        }
    }
}

async fn run(cfg: Config) -> Result<()> {
    tracing::info!(
        version = env!("CARGO_PKG_VERSION"),
        api_addr = %cfg.api_addr,
        ops_addr = %cfg.ops_addr,
        multiplier = cfg.multiplier,
        pool_bytes = cfg.pool_bytes,
        max_request_bytes = cfg.max_request_bytes,
        max_sample_micros = cfg.max_sample_micros,
        serial_count = cfg.serial_allowlist.len(),
        "starting naas-api",
    );

    let metrics_handle = telemetry::install_metrics()?;
    let shutdown = CancellationToken::new();
    let pool = pool::Pool::new(cfg.pool_chunks());

    let reader_handles =
        reader::spawn_all(&cfg, &pool.sender(), &shutdown).context("spawning reader threads")?;

    let api_router = handlers::router(pool.clone(), &cfg);
    let ops_router = ops::router(pool.clone(), metrics_handle);

    let api_listener = tokio::net::TcpListener::bind(cfg.api_addr)
        .await
        .with_context(|| format!("binding API listener to {}", cfg.api_addr))?;
    let ops_listener = tokio::net::TcpListener::bind(cfg.ops_addr)
        .await
        .with_context(|| format!("binding ops listener to {}", cfg.ops_addr))?;

    tracing::info!(addr = %cfg.api_addr, "api listening");
    tracing::info!(addr = %cfg.ops_addr, "ops listening");

    let signal_token = shutdown.clone();
    tokio::spawn(async move {
        wait_for_signal().await;
        tracing::info!("shutdown signal received");
        signal_token.cancel();
    });

    let api_shutdown = shutdown.clone();
    let ops_shutdown = shutdown.clone();

    let api_server = axum::serve(api_listener, api_router)
        .with_graceful_shutdown(async move { api_shutdown.cancelled().await });
    let ops_server = axum::serve(ops_listener, ops_router)
        .with_graceful_shutdown(async move { ops_shutdown.cancelled().await });

    let (api_res, ops_res) = tokio::join!(api_server, ops_server);
    api_res.context("api server error")?;
    ops_res.context("ops server error")?;

    pool.close();

    tracing::info!("waiting for reader threads to drain");
    for h in reader_handles {
        if let Err(e) = h.join() {
            tracing::error!(panic = ?e, "reader thread panicked");
        }
    }

    tracing::info!("shutdown complete");
    Ok(())
}

async fn wait_for_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        match signal(SignalKind::terminate()) {
            Ok(mut term) => {
                tokio::select! {
                    _ = tokio::signal::ctrl_c() => {}
                    _ = term.recv() => {}
                }
            }
            Err(e) => {
                tracing::warn!(error = %e, "could not install SIGTERM handler; using ctrl_c only");
                let _ = tokio::signal::ctrl_c().await;
            }
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}
