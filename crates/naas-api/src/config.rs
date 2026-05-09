use std::net::SocketAddr;

use clap::Parser;

#[derive(Debug, Clone, Parser)]
#[command(
    name = "naas-api",
    version,
    about = "Noise as a Service: HTTP API for the Infinite Noise tRNG",
    long_about = None,
)]
pub struct Config {
    #[arg(long, env = "NAAS_API_ADDR", default_value = "0.0.0.0:8080")]
    pub api_addr: SocketAddr,

    #[arg(long, env = "NAAS_OPS_ADDR", default_value = "0.0.0.0:8081")]
    pub ops_addr: SocketAddr,

    #[arg(
        long,
        env = "NAAS_MULTIPLIER",
        default_value_t = 100,
        value_parser = parse_multiplier,
    )]
    pub multiplier: u32,

    #[arg(long, env = "NAAS_POOL_BYTES", default_value_t = 1_048_576)]
    pub pool_bytes: usize,

    #[arg(long, env = "NAAS_MAX_REQUEST_BYTES", default_value_t = 1_048_576)]
    pub max_request_bytes: usize,

    #[arg(long, env = "NAAS_SERIAL_ALLOWLIST", value_delimiter = ',')]
    pub serial_allowlist: Vec<String>,

    #[arg(long, env = "NAAS_LOG_LEVEL", default_value = "info")]
    pub log_level: String,
}

impl Config {
    pub fn pool_chunks(&self) -> usize {
        const MAX_CHUNK_BYTES: usize = 128;
        (self.pool_bytes / MAX_CHUNK_BYTES).max(64)
    }

    pub fn process_mode(&self) -> infnoise_core::ProcessMode {
        match self.multiplier {
            0 => infnoise_core::ProcessMode::AllEntropy,
            m => infnoise_core::ProcessMode::Multiplier(m),
        }
    }
}

fn parse_multiplier(s: &str) -> Result<u32, String> {
    let n: u32 = s
        .parse()
        .map_err(|e: std::num::ParseIntError| e.to_string())?;
    match n {
        0 | 1 | 10 | 100 | 1000 => Ok(n),
        _ => Err("multiplier must be one of: 0, 1, 10, 100, 1000".into()),
    }
}
