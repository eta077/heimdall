use heimdall::DeviceUpdateMessage;

use sysinfo::{CpuExt, System, SystemExt};

use tracing::error;

use std::io::prelude::*;
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4, TcpStream};
use std::thread;
use std::time::Duration;

const ADDRESS: SocketAddr = SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::new(127, 0, 0, 1), 4064));
const DELAY_MS: u64 = 1500;

fn main() {
    tracing_subscriber::fmt().init();

    let mut stream = None;
    let mut sys = System::new_all();

    loop {
        sys.refresh_all();
        let cpu_usage =
            sys.cpus().iter().map(|cpu| cpu.cpu_usage()).sum::<f32>() / sys.cpus().len() as f32;
        let mem_usage = (sys.used_memory() as f64 / sys.total_memory() as f64) as f32 * 100.0;
        send_update(
            &mut stream,
            DeviceUpdateMessage::CpuMem {
                name: String::from("Laptop"),
                cpu: cpu_usage,
                mem: mem_usage,
            },
        );
        thread::sleep(System::MINIMUM_CPU_UPDATE_INTERVAL * 2);
    }
}

fn send_update(stream_opt: &mut Option<TcpStream>, msg: DeviceUpdateMessage) {
    if stream_opt.is_none() {
        let stream_result = TcpStream::connect_timeout(&ADDRESS, Duration::from_millis(DELAY_MS));
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
    let mut buffer: Vec<u8> = msg.into();

    let write_result = stream.write_all(&buffer);
    if let Err(e) = write_result {
        error!("Could not send device update: {e}");
    }
}
