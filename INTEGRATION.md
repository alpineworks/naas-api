# naas-api integration plan

HTTP API wrapping [`alpineworks/infnoise-rs`](https://github.com/alpineworks/infnoise-rs) — the Rust port of the Infinite Noise Multiplier driver — to expose a real-hardware true-RNG over the network.

## Dependency

`infnoise-rs` is published only on GitHub for now (no crates.io). Pull it in via git:

```toml
[dependencies]
infnoise-core = { git = "https://github.com/alpineworks/infnoise-rs", tag = "v0.1.0" }
infnoise-ftdi = { git = "https://github.com/alpineworks/infnoise-rs", tag = "v0.1.0" }
```

Pin to a tag (or rev) for reproducible builds. `infnoise-core` gives `extract_bytes`, `Whitener`, and `clamp_entropy`; `infnoise-ftdi` gives `Device::open` / `Device::read_buffer`.

We do **not** depend on the `infnoise` binary crate — naas-api owns its own read loop.

License compatibility: infnoise-rs is CC0 (public-domain dedication), naas-api is MIT — that's fine, CC0 work can be used in any license context.

## Throughput / latency baseline

### Per-device hardware ceiling (upstream-reported, optimistic)
| Mode | Output rate |
|---|---|
| `--raw` | ~60 KB/s |
| `--multiplier 10` | ~600 KB/s |
| `--multiplier 100` | ~6 MB/s |
| `--multiplier 1000` | ~60 MB/s |

### Measured against naas-api on a real device (release build, multiplier=100)
| Scenario | Throughput |
|---|---|
| Single sustained stream | ~2.5 MB/s |
| 32 × 1 MiB parallel requests | ~2.3 MB/s aggregate, 8–14 s tail latency |
| 8 concurrent streams | ~2.4 MB/s aggregate, share spread 0.9–2.1 MiB over 4 s |

The measured ceiling matches the theoretical `multiplier=100` rate (FT240X 480 KB/s ÷ 512-byte buffer × 3200-byte output ≈ 3 MB/s) minus health-check rejections and per-buffer USB latency. Upstream's 6 MB/s figure appears optimistic for this device; **plan for ~2.4 MB/s per device**, scale by adding hardware.

### Algorithm-only (no USB), benchmarked on M-series Mac
| Mode | Sustained |
|---|---|
| raw | 8 MB/s |
| multiplier=10 | 33 MB/s |
| multiplier=100 | 141 MB/s |
| multiplier=1000 | 189 MB/s (I/O-bound at this point) |

**Key implication:** the algorithm is ~100× faster than what one device can feed it. CPU is never the bottleneck for sane multipliers; the FT240X's bit-bang clock (480 KB/s of FTDI bytes → 60 KB/s of raw INM) is the hard floor. Scaling is by adding devices, not by tuning code.

USB latency: ~16 ms per 512-byte buffer at the configured baud. Producer-side queue depth dominates request latency for small reads.

## Architecture

```
┌──────────────┐   ┌──────────────────┐   ┌─────────────────┐   ┌──────────────────────┐
│ FTDI device  │──▶│ Reader thread(s) │──▶│ Bounded buffer  │──▶│ API listener :8080   │
│ (one per HW) │   │ (sync, blocking) │   │ pool (1 MiB)    │   │ axum, /api/v1/random │
└──────────────┘   └──────────────────┘   └─────────────────┘   └──────────────────────┘
                                                  │
                                                  ▼
                                          ┌────────────────────────┐
                                          │ Ops listener :8081     │
                                          │ axum, /healthz /metrics│
                                          └────────────────────────┘
```

**One reader thread per USB device.** Sync I/O (libftdi has no real async story we want to bother with). Each reader runs the same loop the `infnoise` binary does:

```rust
device.read_buffer(&mut in_buf)?;
let entropy = extract_bytes(&mut bytes, &in_buf, &mut hc)?;
if hc.ok_to_use_data() && hc.entropy_on_target(entropy, BUFLEN) {
    let entropy_clamped = clamp_entropy(entropy, hc.expected_entropy_per_bit());
    let n = whitener.process(&bytes, &mut out, entropy_clamped, mode);
    pool.push_blocking(&out[..n]);
    while whitener.bytes_pending() > 0 {
        let n = whitener.squeeze_next(&mut out);
        pool.push_blocking(&out[..n]);
    }
}
```

**Bounded buffer pool** — fixed-size, default 1 MiB, `async-channel::bounded::<bytes::Bytes>` (mpmc; sync `send_blocking` for readers, async `recv` for handlers). Backpressure: readers block on push when full (natural rate-limiting); handlers `await` on pull when empty.

**Two axum listeners on one tokio runtime.** Public API on 8080, ops (health + metrics) on 8081. `tokio::join!` both `serve()` futures; signal handler triggers graceful shutdown on each.

**Multi-device fan-in** for scaling: N devices → N reader threads → one shared pool. Scaling is linear.

## API surface

**API listener — `0.0.0.0:8080`:**

| Method | Path | Behavior |
|---|---|---|
| `GET` | `/api/v1/random/<n>` | Return exactly `n` bytes as `application/octet-stream`. 400 if `n` > 1 MiB. |
| `GET` | `/api/v1/random/stream` | `Transfer-Encoding: chunked` octet-stream. Streams from the pool until the client disconnects. For sustained consumers. |
| `GET` | `/api/v1/random/hex/<n>` | Same bytes, hex-encoded `text/plain; charset=us-ascii`. 1 MiB cap on the underlying byte count. |
| `GET` | `/api/v1/random/base64/<n>` | Same bytes, standard base64 `text/plain; charset=us-ascii`. 1 MiB cap on the underlying byte count. |

**Ops listener — `0.0.0.0:8081`:**

| Method | Path | Behavior |
|---|---|---|
| `GET` | `/healthz` | 200 if at least one device is producing entropy within the last health-window; 503 if all readers are stuck. |
| `GET` | `/metrics` | Prometheus exposition format. Per-device labels by FTDI serial: pool depth, entropy bytes pushed, healthcheck rejections, USB error counts. |

No authentication in v1 (private-network deployment is the assumed posture). API-key middleware is a v1.1 candidate.

## Configuration

All settings via env vars with matching CLI flags (clap derive + `env` feature). CLI flags override env.

| Env var | CLI flag | Default | Notes |
|---|---|---|---|
| `NAAS_API_ADDR` | `--api-addr` | `0.0.0.0:8080` | Public API listener. |
| `NAAS_OPS_ADDR` | `--ops-addr` | `0.0.0.0:8081` | Health + metrics listener. |
| `NAAS_MULTIPLIER` | `--multiplier` | `100` | INM whitener multiplier. Supported: 1, 10, 100, 1000. |
| `NAAS_POOL_BYTES` | `--pool-bytes` | `1048576` | Bounded pool capacity. |
| `NAAS_MAX_REQUEST_BYTES` | `--max-request-bytes` | `1048576` | Per-request cap on the bounded `random` endpoints. |
| `NAAS_SERIAL_ALLOWLIST` | `--serial-allowlist` | *(unset)* | Comma-separated FTDI serials. Unset = scan and use all detected devices. |
| `NAAS_LOG_LEVEL` | `--log-level` | `info` | `tracing` env-filter syntax (e.g. `info,naas_api=debug`). |

Logging: always JSON via `tracing-subscriber` `fmt().json()`. One stream to stdout.

## Resolved decisions

| Topic | Decision |
|---|---|
| HTTP framework | `axum` on `tokio` multi-thread runtime |
| Streaming response | Chunked `application/octet-stream` (no SSE) |
| Pool implementation | `async-channel::bounded::<bytes::Bytes>` (mpmc, sync↔async bridge) |
| Multi-device discovery | Scan FTDI devices at boot; optional `NAAS_SERIAL_ALLOWLIST` |
| Linux `RNDADDENTROPY` feed | Out of scope (separate daemon if ever revisited) |
| Metrics granularity | Per-device, labeled by serial |
| Graceful shutdown | Close listeners on SIGTERM/Ctrl-C, drop readers, exit |
| Container base | `debian:bookworm-slim` + `libftdi1` from apt |
| Architectures | `linux/amd64`, `linux/arm64` |
| Auth | None in v1 |
| Multiplier scope | Server config only |
| Per-request cap | 1 MiB on bounded endpoints; use `/stream` for sustained consumers |
| Output formats | binary, hex, base64 |
| Listener layout | API on 8080, ops on 8081 |

## Test surface

We don't re-validate bit-extraction, healthcheck, or whitening — `infnoise-rs` does that against the C reference. naas-api's own test surface:

- HTTP layer: handlers return correct content-type, length, status codes; cap enforcement; hex/base64 encoding round-trips.
- Pool / backpressure semantics: readers block on push when full, handlers wait or 503 when empty (configurable behavior, default: short bounded wait then 503).
- Concurrent-request behavior: no torn reads, no double-served bytes, fairness across in-flight handlers (a single big request shouldn't starve a stream of small ones).
- Graceful device disconnect/reconnect: reader thread exits cleanly, supervisor restarts after backoff, health flips to degraded when no producer is live.
- Streaming: client disconnect doesn't leak buffers; backpressure on slow consumers.

Hardware is not required for any of these — fake the producer with a deterministic byte source for the HTTP/pool tests.

## Release & packaging

**Versioning:** SemVer. Releases tagged `vX.Y.Z`.

**Release workflow** (`.github/workflows/release.yml`, on tag push `v*`):
1. `docker/setup-qemu-action` + `docker/setup-buildx-action`.
2. `docker/login-action` against `ghcr.io` with `GITHUB_TOKEN`.
3. `docker/build-push-action` with `platforms: linux/amd64,linux/arm64`, tags `ghcr.io/alpineworks/naas-api:vX.Y.Z`, plus `:latest` only on non-prerelease tags.
4. `gh release create` with auto-generated notes; body links the GHCR image and `docker pull` snippet.

**CI workflow** (`.github/workflows/ci.yml`, on push / PR):
- `cargo fmt --check`
- `cargo clippy --all-targets -- -D warnings`
- `cargo test --all-features`
- `cargo deny check` (advisories + licenses + bans)
- `docker buildx build --platform linux/amd64` (no push) to catch Dockerfile drift

Runner: `ubuntu-latest`. macOS jobs optional once cross-platform dev becomes a real need.

## Rust standards

- Edition 2024, latest stable rustc; pin via `rust-toolchain.toml`.
- `clippy` baseline `-D warnings`; opt into `clippy::pedantic` per module where it pulls its weight.
- Errors: `thiserror` for typed boundaries (`ConfigError`, `DeviceError`, `PoolError`); `anyhow::Result` at `main` only.
- Async: `tokio` multi-thread runtime for HTTP; readers stay sync on `std::thread`.
- Buffers: `bytes::Bytes` end-to-end so axum response bodies hand them out without copying.
- No `unsafe` outside what `infnoise-ftdi` already encapsulates.
- Layout: workspace `Cargo.toml` with a single `naas-api` binary crate (room to split into a lib later without restructuring imports).

## What's already proven in infnoise-rs

We don't need to re-validate any of this in naas-api — it's covered by the upstream cross-validation:

- Bit-extraction, healthcheck, and Keccak whitening produce **byte-identical output to the C reference** across raw + AllEntropy + multiplier {1, 2, 3, 5, 10}.
- Verified against both synthetic INM data and 256 buffers (~131 KB) of real INM hardware noise checked into `infnoise-rs/tests/fixtures/hw-capture.bin`.
- CI runs the full cross-validation on every push (Linux + macOS).

## Suggested first commit

1. Workspace `Cargo.toml` declaring `naas-api` as a single binary crate, edition 2024. `rust-toolchain.toml` pinning latest stable.
2. Deps: `axum`, `tokio` (`rt-multi-thread`, `macros`, `signal`), `tokio-util` (`CancellationToken`), `tower-http` (`trace`), `async-channel`, `bytes`, `futures`, `clap` (`derive`, `env`), `tracing`, `tracing-subscriber` (`env-filter`, `json`), `metrics` + `metrics-exporter-prometheus`, `hex`, `base64`, `thiserror`, `anyhow`, `infnoise-core` (git tag), `infnoise-ftdi` (git tag).
3. `src/main.rs` skeleton:
   - `Config` parsed via `clap`.
   - Per-detected (or allow-listed) FTDI device: spawn one `std::thread` reader pushing `Bytes` into a shared `crossbeam_channel`.
   - Two `axum::Router`s — API on 8080 (`/api/v1/random/*`), ops on 8081 (`/healthz`, `/metrics`).
   - `tokio::join!` both servers under graceful-shutdown wired to `tokio::signal::ctrl_c` + SIGTERM.
4. `Dockerfile`: `cargo-chef` two-stage build → runtime on `debian:bookworm-slim` with `libftdi1` apt-installed; `USER` non-root; `EXPOSE 8080 8081`.
5. `.github/workflows/ci.yml`: fmt + clippy + test + cargo-deny + Docker build (no push).
6. `.github/workflows/release.yml`: on tag `v*`, multi-arch buildx + push to `ghcr.io/alpineworks/naas-api` + `gh release create`.

That gets us to a working end-to-end demo. Explicit v1 non-goals: auth, request-id propagation, OpenTelemetry traces, Linux kernel-pool feed, JSON/UUID/integer output formats, per-request multiplier override.

## Known v1 limitations (deferred fixes)

Surfaced during initial load testing; safe for the documented private-network deployment, **must address before public exposure**:

1. **No pull timeout.** `pool.pull()` and `/random/stream` await indefinitely on `async-channel::recv`. If the device disappears and the pool drains, in-flight handlers hang until the client times out. The reader supervisor retries `Device::open()` every 2 s, but persistent failure leaves handlers blocked. Fix: wrap `pool.pull()` in `tokio::time::timeout` keyed off `NAAS_REQUEST_TIMEOUT`; return 503 on expiry.
2. **No request-concurrency cap.** All inbound requests are accepted regardless of count. Each holds a `BytesMut::with_capacity(n)` allocation while waiting; pathological clients can pre-allocate gigabytes. Fix: `tower::limit::ConcurrencyLimitLayer` on the API router (e.g. 256 in flight).
3. **No per-IP rate limiting.** A single client can drain the pool and starve others. Fix: `tower_governor` or similar per-`X-Forwarded-For` (or peer addr) limiter, configurable by env.

## Bugs found and fixed during initial integration

- **`metrics::counter!()` does not accumulate** when called inline with the same name+labels each time — the macro returns a fresh handle whose increments don't sum into the registered atomic. Fix: cache `Counter` handles once at startup and call `.increment()` on the cached value. Applied in `reader.rs` (`DeviceMetrics`) and `handlers.rs` (`ResponseMetrics`). All future counter use must follow this pattern.
- **`/healthz` was 503 when the pool was full** because the staleness check ignored "reader parked because pool is at capacity". Fix: `pending_chunks > 0` → 200 first, then fall through to the staleness check.
