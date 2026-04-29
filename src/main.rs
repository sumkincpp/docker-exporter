use log::{LevelFilter, info};
use prometheus::{Encoder, TextEncoder};
use simplelog::{Config as LogConfig, SimpleLogger};
use std::env::{self, VarError};
use std::net::{Ipv4Addr, SocketAddrV4};
use tiny_http::{Header, Response, Server};

#[path = "collector/mod.rs"]
mod collector;
mod docker;

pub struct Config {
    port: u16,
    min_log_level: LevelFilter,
    pub collect_image_metrics: bool,
    pub collect_volume_metrics: bool,
}

impl Config {
    fn is_truthy(var: Result<String, VarError>, default: bool) -> bool {
        match var {
            Ok(s) => s == "1" || s.eq_ignore_ascii_case("true"),
            _ => default,
        }
    }

    fn new() -> Config {
        Config {
            port: 9417,
            min_log_level: if Self::is_truthy(env::var("VERBOSE"), cfg!(debug_assertions)) {
                LevelFilter::Debug
            } else {
                LevelFilter::Info
            },
            collect_image_metrics: Self::is_truthy(
                env::var("COLLECT_IMAGE_METRICS"),
                cfg!(debug_assertions),
            ),
            collect_volume_metrics: Self::is_truthy(
                env::var("COLLECT_VOLUME_METRICS"),
                cfg!(debug_assertions),
            ),
        }
    }
}

#[tokio::main]
async fn main() {
    ctrlc::set_handler(|| {
        info!("Exiting.");
        std::process::exit(0);
    })
    .unwrap();

    let config = Config::new();
    SimpleLogger::init(config.min_log_level, LogConfig::default()).unwrap();

    let mut collector = collector::Collector::new(docker::UnixSocketClient::default());

    let addr = SocketAddrV4::new(Ipv4Addr::new(0, 0, 0, 0), config.port);
    let server = Server::http(addr).unwrap();

    for req in server.incoming_requests() {
        if req.url() != "/metrics" {
            req.respond(Response::empty(404)).unwrap_or(());
            continue;
        }

        if collector.update(&config).await {
            let mut buffer = Vec::new();
            let encoder = TextEncoder::new();
            encoder.encode(&collector.gather(), &mut buffer).unwrap();

            let res = Response::from_data(buffer)
                .with_header(Header::from_bytes("Content-Type", prometheus::TEXT_FORMAT).unwrap());
            req.respond(res).unwrap_or(());
        } else {
            req.respond(Response::empty(408)).unwrap_or(());
        }
    }
}
