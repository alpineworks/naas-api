use anyhow::{Context, Result};
use metrics_exporter_prometheus::{PrometheusBuilder, PrometheusHandle};
use tracing_subscriber::EnvFilter;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;

use crate::config::Config;

pub fn init_tracing(cfg: &Config) -> Result<()> {
    let filter = EnvFilter::try_new(&cfg.log_level).or_else(|_| EnvFilter::try_new("info"))?;
    let layer = tracing_subscriber::fmt::layer()
        .json()
        .with_current_span(false)
        .with_span_list(false);
    tracing_subscriber::registry()
        .with(filter)
        .with(layer)
        .try_init()
        .context("install tracing subscriber")?;
    Ok(())
}

pub fn install_metrics() -> Result<PrometheusHandle> {
    PrometheusBuilder::new()
        .install_recorder()
        .context("install prometheus recorder")
}
