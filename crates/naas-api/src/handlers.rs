use axum::Router;
use axum::body::Body;
use axum::extract::{Path, State};
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::get;
use base64::Engine;
use bytes::Bytes;
use futures::stream;
use metrics::Counter;
use tower_http::trace::TraceLayer;

use crate::config::Config;
use crate::error::ServiceError;
use crate::pool::Pool;

#[derive(Clone)]
struct ApiState {
    pool: Pool,
    max_bytes: usize,
    index_html: Bytes,
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

pub fn router(pool: Pool, cfg: &Config) -> Router {
    let state = ApiState {
        pool,
        max_bytes: cfg.max_request_bytes,
        index_html: Bytes::from(index_html(cfg)),
        metrics: ResponseMetrics::new(),
    };
    Router::new()
        .route("/", get(index))
        .route("/api/v1/random/stream", get(random_stream))
        .route("/api/v1/random/{n}", get(random_bytes))
        .route("/api/v1/random/hex/{n}", get(random_hex))
        .route("/api/v1/random/base64/{n}", get(random_base64))
        .layer(TraceLayer::new_for_http())
        .with_state(state)
}

async fn index(State(state): State<ApiState>) -> Html<Bytes> {
    Html(state.index_html.clone())
}

/// Render the plain-HTML usage page once at startup with the live config
/// baked in. No stylesheet on purpose: white background, black text,
/// whatever font the browser defaults to.
fn index_html(cfg: &Config) -> String {
    let whitening = match cfg.multiplier {
        0 => "Whitening: Keccak sponge, emitting only as many bytes as the \
              health checker measured in entropy for that sample. Every output \
              byte is backed by a full byte of measured hardware entropy."
            .to_string(),
        m => format!(
            "Whitening: Keccak sponge with output multiplier {m}. Each 512-bit \
             hardware sample (about 437 bits of measured entropy) reseeds the \
             sponge, which is then squeezed for {} bytes before the next sample \
             is absorbed. Output beyond the first few dozen bytes per sample is \
             cryptographically expanded rather than raw hardware entropy.",
            u64::from(m) * 32
        ),
    };
    INDEX_TEMPLATE
        .replace("__VERSION__", env!("CARGO_PKG_VERSION"))
        .replace("__MAX__", &cfg.max_request_bytes.to_string())
        .replace("__WHITENING__", &whitening)
}

const INDEX_TEMPLATE: &str = r#"<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>naas-api</title>
</head>
<body>

<h1>naas-api</h1>

<p>Noise as a Service, version __VERSION__.</p>

<p>This server hands out random bytes read from an
<a href="https://github.com/leetronics/infnoise">Infinite Noise Multiplier</a>,
a hardware true random number generator on a USB stick. Bytes are produced
by the device, checked, whitened, and queued in a small pool. Each byte is
served to exactly one client and then discarded.</p>

<hr>

<h2>Endpoints</h2>

<pre>
GET /api/v1/random/{n}          n raw bytes           application/octet-stream
GET /api/v1/random/hex/{n}      n bytes, hex          text/plain
GET /api/v1/random/base64/{n}   n bytes, base64       text/plain
GET /api/v1/random/stream       endless chunked       application/octet-stream
</pre>

<p><code>n</code> must be between 1 and __MAX__ inclusive. Anything else gets a 400.
The stream endpoint has no cap: it sends bytes until you disconnect.</p>

<p>Try it in the browser:
<a href="/api/v1/random/hex/16">/api/v1/random/hex/16</a>,
<a href="/api/v1/random/base64/32">/api/v1/random/base64/32</a>.</p>

<h2>Examples</h2>

<pre>
NAAS=http://localhost:8080

# 32 raw bytes into a file
curl -sS "$NAAS/api/v1/random/32" -o key.bin

# 16 bytes as hex
curl -sS "$NAAS/api/v1/random/hex/16"

# a 256-bit key as base64
curl -sS "$NAAS/api/v1/random/base64/32"

# stream into a file until Ctrl-C
curl -sS "$NAAS/api/v1/random/stream" -o noise.bin

# the first 1 MiB of the stream
curl -sS "$NAAS/api/v1/random/stream" | head -c 1048576 > noise.bin

# feed it to a test suite
curl -sS "$NAAS/api/v1/random/stream" | head -c 10485760 | ent
</pre>

<h2>Responses</h2>

<pre>
200   bytes follow
400   n is 0, not a number, or larger than __MAX__
503   entropy pool closed (server is shutting down)
</pre>

<p>A request waits until enough bytes are in the pool. If the device is
unplugged the request blocks until it comes back or the client gives up, so
set a client timeout.</p>

<h2>How the bytes are made</h2>

<ol>
<li>The FT240X on the stick clocks the multiplier and returns 512 comparator
samples per USB read, one noise bit each.</li>
<li>A streaming health check predicts each bit from the previous 14 and
measures how often it is right. The device is designed to give about 0.88
bits of entropy per sample bit. Samples are discarded when the measured
entropy drifts more than 3% from that, when a run of more than 20 identical
bits appears, or when the USB round trip stalled long enough to let the
analog loop settle.</li>
<li>__WHITENING__</li>
</ol>

<h2>Operations</h2>

<p>Health and metrics live on a separate listener (port 8081 by default):
<code>/healthz</code> returns 200 while at least one device is producing,
and <code>/metrics</code> is Prometheus exposition format.</p>

<hr>

<p>Source: <a href="https://github.com/alpineworks/naas-api">alpineworks/naas-api</a>.
Driver: <a href="https://github.com/alpineworks/infnoise-rs">alpineworks/infnoise-rs</a>,
a Rust port of the reference C driver.</p>

</body>
</html>
"#;

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
