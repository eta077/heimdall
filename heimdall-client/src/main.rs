use heimdall::DeviceUpdateMessage;

use sysinfo::{CpuExt, System, SystemExt};

use tracing::error;

use std::io::prelude::*;
use std::net::{SocketAddr, TcpStream};
use std::thread;
use std::time::Duration;

struct HeimdallClientConfig {
    server_address: SocketAddr,
    device_name: String,
}

const DELAY_MS: u64 = 1500;

fn main() {
    tracing_subscriber::fmt().init();

    let mut stream = None;
    let mut sys = System::new_all();

    let server_address;
    match "127.0.0.1:4064".parse() {
        Ok(address) => {
            server_address = address;
        }
        Err(e) => {
            error!("Heimdall client unable to parse server address: {e}");
            return;
        }
    }

    let config = HeimdallClientConfig {
        server_address,
        device_name: String::from("Laptop"),
    };

    loop {
        sys.refresh_all();
        let cpu_usage =
            sys.cpus().iter().map(|cpu| cpu.cpu_usage()).sum::<f32>() / sys.cpus().len() as f32;
        let mem_usage = (sys.used_memory() as f64 / sys.total_memory() as f64) as f32 * 100.0;
        send_update(
            &config,
            &mut stream,
            DeviceUpdateMessage::CpuMem {
                name: config.device_name.clone(),
                cpu: cpu_usage,
                mem: mem_usage,
            },
        );
        thread::sleep(System::MINIMUM_CPU_UPDATE_INTERVAL * 2);
    }
}

fn send_update(
    config: &HeimdallClientConfig,
    stream_opt: &mut Option<TcpStream>,
    msg: DeviceUpdateMessage,
) {
    if stream_opt.is_none() {
        let stream_result =
            TcpStream::connect_timeout(&config.server_address, Duration::from_millis(DELAY_MS));
        match stream_result {
            Ok(stream) => {
                *stream_opt = Some(stream);
            }
            Err(e) => {
                error!("Could not connect to Heimdall server: {e}");
                return;
            }
        }
    }
    // safe to unwrap; value is inserted above
    let stream = stream_opt.as_mut().unwrap();
    let buffer: Vec<u8> = msg.into();

    let write_result = stream.write_all(&buffer);
    if let Err(e) = write_result {
        error!("Could not send device update: {e}");
        *stream_opt = None;
    }
}
