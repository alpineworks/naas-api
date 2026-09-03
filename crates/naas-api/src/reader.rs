use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use bytes::Bytes;
use metrics::Counter;
use thiserror::Error;
use tokio_util::sync::CancellationToken;

use infnoise_core::{
    BUFLEN, ExtractError, HealthCheck, HealthCheckError, ProcessMode, Whitener, clamp_entropy,
    extract_bytes,
};
use infnoise_ftdi::{Device, FtdiError};

use crate::config::Config;
use crate::pool::PoolSender;

const PREDICTION_BITS: u8 = 14;
const DESIGN_K: f64 = 1.84;
const WARMUP_LIMIT: u32 = 5_000;
const RECONNECT_BACKOFF: Duration = Duration::from_secs(2);

#[derive(Debug, Error)]
pub enum ReaderError {
    #[error("ftdi: {0}")]
    Ftdi(#[from] FtdiError),
    #[error("health check init failed: {0}")]
    HealthInit(HealthCheckError),
    #[error("extract: {0}")]
    Extract(#[from] ExtractError),
    #[error("warmup exceeded {0} rounds without health checker accepting data")]
    WarmupTimeout(u32),
}

pub fn spawn_all(
    cfg: &Config,
    sender: &PoolSender,
    shutdown: &CancellationToken,
) -> std::io::Result<Vec<JoinHandle<()>>> {
    let serials: Vec<Option<String>> = if cfg.serial_allowlist.is_empty() {
        vec![None]
    } else {
        cfg.serial_allowlist
            .iter()
            .map(|s| Some(s.clone()))
            .collect()
    };

    let mode = cfg.process_mode();
    let max_sample = cfg.max_sample_duration();
    let mut handles = Vec::with_capacity(serials.len());

    for serial in serials {
        let label = serial.clone().unwrap_or_else(|| "default".to_string());
        let sender = sender.clone();
        let shutdown = shutdown.clone();
        let h = thread::Builder::new()
            .name(format!("infnoise:{label}"))
            .spawn(move || {
                supervise(
                    serial.as_deref(),
                    &label,
                    mode,
                    max_sample,
                    &sender,
                    &shutdown,
                );
            })?;
        handles.push(h);
    }
    Ok(handles)
}

struct DeviceMetrics {
    entropy_bytes: Counter,
    health_rejections: Counter,
    timing_rejections: Counter,
    reader_errors: Counter,
}

impl DeviceMetrics {
    fn new(label: &str) -> Self {
        let device = label.to_string();
        Self {
            entropy_bytes: metrics::counter!("naas_entropy_bytes_total", "device" => device.clone()),
            health_rejections: metrics::counter!("naas_health_rejections_total", "device" => device.clone()),
            timing_rejections: metrics::counter!("naas_timing_rejections_total", "device" => device.clone()),
            reader_errors: metrics::counter!("naas_reader_errors_total", "device" => device),
        }
    }
}

fn supervise(
    serial: Option<&str>,
    label: &str,
    mode: ProcessMode,
    max_sample: Duration,
    sender: &PoolSender,
    shutdown: &CancellationToken,
) {
    let m = DeviceMetrics::new(label);
    while !shutdown.is_cancelled() && !sender.is_closed() {
        match read_loop(serial, label, mode, max_sample, sender, shutdown, &m) {
            Ok(()) => break,
            Err(e) => {
                m.reader_errors.increment(1);
                tracing::error!(device = %label, error = %e, "reader loop exited; backing off");
                if shutdown.is_cancelled() {
                    break;
                }
                thread::sleep(RECONNECT_BACKOFF);
            }
        }
    }
    tracing::info!(device = %label, "reader thread exiting");
}

fn read_loop(
    serial: Option<&str>,
    label: &str,
    mode: ProcessMode,
    max_sample: Duration,
    sender: &PoolSender,
    shutdown: &CancellationToken,
    m: &DeviceMetrics,
) -> Result<(), ReaderError> {
    tracing::info!(device = %label, ?serial, "opening device");
    let mut device = Device::open(serial)?;
    let mut hc = HealthCheck::new(PREDICTION_BITS, DESIGN_K).map_err(ReaderError::HealthInit)?;
    let mut whitener = Whitener::new();

    let mut in_buf = [0u8; BUFLEN];
    let mut bytes = [0u8; BUFLEN / 8];
    let mut out = [0u8; 128];

    let mut warmup_rounds = 0u32;
    while !hc.ok_to_use_data() {
        if shutdown.is_cancelled() {
            return Ok(());
        }
        warmup_rounds += 1;
        if warmup_rounds > WARMUP_LIMIT {
            return Err(ReaderError::WarmupTimeout(WARMUP_LIMIT));
        }
        if !timed_read(&mut device, &mut in_buf, max_sample, m)? {
            continue;
        }
        let _ = extract_bytes(&mut bytes, &in_buf, &mut hc)?;
    }
    tracing::info!(device = %label, warmup_rounds, "device warmed up");

    while !shutdown.is_cancelled() && !sender.is_closed() {
        if whitener.bytes_pending() > 0 {
            let n = whitener.squeeze_next(&mut out);
            if !push(sender, &out[..n], &m.entropy_bytes) {
                return Ok(());
            }
            continue;
        }

        if !timed_read(&mut device, &mut in_buf, max_sample, m)? {
            continue;
        }
        let entropy = extract_bytes(&mut bytes, &in_buf, &mut hc)?;
        if !hc.ok_to_use_data() || !hc.entropy_on_target(entropy, BUFLEN as u32) {
            m.health_rejections.increment(1);
            continue;
        }
        let entropy_clamped = clamp_entropy(entropy, hc.expected_entropy_per_bit());
        let n = whitener.process(&bytes, &mut out, entropy_clamped, mode);
        if n > 0 && !push(sender, &out[..n], &m.entropy_bytes) {
            return Ok(());
        }
    }
    Ok(())
}

/// Drive one INM clock cycle and time the USB round trip. Returns `Ok(false)`
/// when the sample took longer than `max_sample` and must be discarded.
///
/// This is the `MAX_MICROSEC_FOR_SAMPLES` guard from the C reference's
/// `readData`: a stalled bit-bang clock lets the INM's analog loop settle, so
/// bits sampled after a long stall carry less entropy than the health checker
/// assumes. The health checker is a statistical estimate; this guard is the
/// independent, per-sample defense.
fn timed_read(
    device: &mut Device,
    in_buf: &mut [u8; BUFLEN],
    max_sample: Duration,
    m: &DeviceMetrics,
) -> Result<bool, ReaderError> {
    let start = Instant::now();
    device.read_buffer(in_buf)?;
    if start.elapsed() > max_sample {
        m.timing_rejections.increment(1);
        return Ok(false);
    }
    Ok(true)
}

fn push(sender: &PoolSender, data: &[u8], entropy_counter: &Counter) -> bool {
    let bytes = Bytes::copy_from_slice(data);
    let len = bytes.len() as u64;
    if sender.push_blocking(bytes).is_err() {
        return false;
    }
    entropy_counter.increment(len);
    true
}
