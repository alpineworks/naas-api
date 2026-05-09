use std::time::{SystemTime, UNIX_EPOCH};

use axum::Router;
use axum::extract::State;
use axum::http::{StatusCode, header};
use axum::response::IntoResponse;
use axum::routing::get;
use metrics_exporter_prometheus::PrometheusHandle;

use crate::pool::Pool;

const HEALTH_STALE_MS: u64 = 10_000;

#[derive(Clone)]
struct OpsState {
    pool: Pool,
    metrics: PrometheusHandle,
}

impl std::fmt::Debug for OpsState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OpsState")
            .field("pool", &self.pool)
            .finish_non_exhaustive()
    }
}

pub fn router(pool: Pool, metrics: PrometheusHandle) -> Router {
    let state = OpsState { pool, metrics };
    Router::new()
        .route("/healthz", get(health))
        .route("/metrics", get(metrics_endpoint))
        .with_state(state)
}

async fn health(State(state): State<OpsState>) -> impl IntoResponse {
    if state.pool.pending_chunks() > 0 {
        return (StatusCode::OK, "ok\n");
    }
    let last = state.pool.last_push_ms();
    if last == 0 {
        return (StatusCode::SERVICE_UNAVAILABLE, "starting up\n");
    }
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    if now.saturating_sub(last) > HEALTH_STALE_MS {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            "no entropy producer healthy\n",
        );
    }
    (StatusCode::OK, "ok\n")
}

async fn metrics_endpoint(State(state): State<OpsState>) -> impl IntoResponse {
    metrics::gauge!("naas_pool_chunks_pending").set(state.pool.pending_chunks() as f64);
    metrics::gauge!("naas_pool_chunks_capacity").set(state.pool.capacity_chunks() as f64);
    let body = state.metrics.render();
    (
        [(
            header::CONTENT_TYPE,
            "text/plain; version=0.0.4; charset=utf-8",
        )],
        body,
    )
}
