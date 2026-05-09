use axum::Router;
use axum::body::Body;
use axum::extract::{Path, State};
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use base64::Engine;
use bytes::Bytes;
use futures::stream;
use metrics::Counter;
use tower_http::trace::TraceLayer;

use crate::error::ServiceError;
use crate::pool::Pool;

#[derive(Clone)]
struct ApiState {
    pool: Pool,
    max_bytes: usize,
    metrics: ResponseMetrics,
}

#[derive(Clone)]
struct ResponseMetrics {
    binary: Counter,
    hex: Counter,
    base64: Counter,
    stream: Counter,
}

impl ResponseMetrics {
    fn new() -> Self {
        Self {
            binary: metrics::counter!("naas_response_bytes_total", "format" => "binary"),
            hex: metrics::counter!("naas_response_bytes_total", "format" => "hex"),
            base64: metrics::counter!("naas_response_bytes_total", "format" => "base64"),
            stream: metrics::counter!("naas_response_bytes_total", "format" => "stream"),
        }
    }
}

pub fn router(pool: Pool, max_bytes: usize) -> Router {
    let state = ApiState {
        pool,
        max_bytes,
        metrics: ResponseMetrics::new(),
    };
    Router::new()
        .route("/api/v1/random/stream", get(random_stream))
        .route("/api/v1/random/{n}", get(random_bytes))
        .route("/api/v1/random/hex/{n}", get(random_hex))
        .route("/api/v1/random/base64/{n}", get(random_base64))
        .layer(TraceLayer::new_for_http())
        .with_state(state)
}

fn check_n(n: usize, max: usize) -> Result<(), ServiceError> {
    if n == 0 {
        return Err(ServiceError::EmptyRequest);
    }
    if n > max {
        return Err(ServiceError::RequestTooLarge { requested: n, max });
    }
    Ok(())
}

async fn random_bytes(
    State(state): State<ApiState>,
    Path(n): Path<usize>,
) -> Result<Response, ServiceError> {
    check_n(n, state.max_bytes)?;
    let bytes = state.pool.pull(n).await?;
    state.metrics.binary.increment(n as u64);
    Ok((
        [(
            header::CONTENT_TYPE,
            HeaderValue::from_static("application/octet-stream"),
        )],
        bytes,
    )
        .into_response())
}

async fn random_hex(
    State(state): State<ApiState>,
    Path(n): Path<usize>,
) -> Result<Response, ServiceError> {
    check_n(n, state.max_bytes)?;
    let bytes = state.pool.pull(n).await?;
    state.metrics.hex.increment(n as u64);
    Ok((
        [(
            header::CONTENT_TYPE,
            HeaderValue::from_static("text/plain; charset=us-ascii"),
        )],
        hex::encode(&bytes),
    )
        .into_response())
}

async fn random_base64(
    State(state): State<ApiState>,
    Path(n): Path<usize>,
) -> Result<Response, ServiceError> {
    check_n(n, state.max_bytes)?;
    let bytes = state.pool.pull(n).await?;
    state.metrics.base64.increment(n as u64);
    let body = base64::engine::general_purpose::STANDARD.encode(&bytes);
    Ok((
        [(
            header::CONTENT_TYPE,
            HeaderValue::from_static("text/plain; charset=us-ascii"),
        )],
        body,
    )
        .into_response())
}

async fn random_stream(State(state): State<ApiState>) -> Response {
    let pool = state.pool.clone();
    let counter = state.metrics.stream.clone();
    let body_stream = stream::unfold((pool, counter), |(pool, counter)| async move {
        match pool.pull_chunk().await {
            Ok(chunk) => {
                counter.increment(chunk.len() as u64);
                Some((
                    Ok::<Bytes, std::convert::Infallible>(chunk),
                    (pool, counter),
                ))
            }
            Err(_) => None,
        }
    });
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "application/octet-stream")
        .body(Body::from_stream(body_stream))
        .expect("static response builder")
}
