#![deny(clippy::all)]

//! Obtain resource statistics from devices on the local network

use blt_utils::*;

use crossbeam_channel::{unbounded, Receiver, Sender};

use serde::Serialize;

use tracing::{error, info, trace, warn};

use std::collections::HashMap;
use std::io::Read;
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4, TcpListener, TcpStream};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

pub const HEIMDALL_PORT: u16 = 4064;

pub type HeimdallState = Vec<Device>;

#[derive(Clone, Debug, Serialize)]
pub struct Device {
    pub name: String,
    pub cpu_usage: f32,
    pub mem_usage: f32,
}

#[derive(Debug)]
pub enum DeviceUpdateMessage {
    CpuMem { name: String, cpu: f32, mem: f32 },
}

impl From<DeviceUpdateMessage> for Vec<u8> {
    fn from(value: DeviceUpdateMessage) -> Self {
        match value {
            DeviceUpdateMessage::CpuMem { name, cpu, mem } => {
                let mut buffer = Vec::new();
                serialize_string(name, &mut buffer);
                serialize_f32(cpu, &mut buffer);
                serialize_f32(mem, &mut buffer);
                finalize_serialization(&mut buffer);
                buffer
            }
        }
    }
}

impl TryFrom<Vec<u8>> for DeviceUpdateMessage {
    type Error = blt_utils::DeserializationError;

    fn try_from(mut value: Vec<u8>) -> Result<Self, Self::Error> {
        let name = deserialize_string(&mut value)?;
        let cpu = deserialize_f32(&mut value)?;
        let mem = deserialize_f32(&mut value)?;

        Ok(DeviceUpdateMessage::CpuMem { name, cpu, mem })
    }
}

#[derive(Default)]
pub struct HeimdallServer {
    state: Arc<Mutex<HashMap<String, Device>>>,
    state_senders: Arc<Mutex<Vec<Sender<HeimdallState>>>>,
}

impl HeimdallServer {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn start(&self) {
        let (sender, receiver) = unbounded();
        let state = Arc::clone(&self.state);
        let senders = Arc::clone(&self.state_senders);

        thread::Builder::new()
            .name(String::from("HeimdallServerThread"))
            .spawn(|| Self::receiver_task(receiver, state, senders))
            .expect("Could not spawn HeimdallServerThread");

        thread::Builder::new()
            .name(String::from("HeimdallSocketThread"))
            .spawn(|| Self::socket_accept_task(sender))
            .expect("Could not spawn HeimdallSocketThread");
    }

    pub fn add_listener(&self) -> Receiver<HeimdallState> {
        let (sender, receiver) = unbounded();
        self.state_senders
            .lock()
            .expect("add_listener could not lock state_listeners")
            .push(sender);
        receiver
    }

    fn receiver_task(
        receiver: Receiver<DeviceUpdateMessage>,
        state: Arc<Mutex<HashMap<String, Device>>>,
        senders: Arc<Mutex<Vec<Sender<HeimdallState>>>>,
    ) {
        loop {
            if !receiver.is_empty() {
                let mut new_state = state.lock().expect("receiver_task could not lock state");
                for msg in receiver.try_iter() {
                    match msg {
                        DeviceUpdateMessage::CpuMem { name, cpu, mem } => {
                            new_state
                                .entry(name.clone())
                                .and_modify(|device| {
                                    device.cpu_usage = cpu;
                                    device.mem_usage = mem;
                                })
                                .or_insert_with(|| Device {
                                    name,
                                    cpu_usage: cpu,
                                    mem_usage: mem,
                                });
                        }
                    }
                }
                let state_event = new_state.values().cloned().collect::<Vec<Device>>();
                for sender in senders
                    .lock()
                    .expect("receiver_task could not lock senders")
                    .iter()
                {
                    if sender.send(state_event.clone()).is_err() {
                        // TODO: remove from senders
                        warn!("receiver_task failed to send update to sender");
                    }
                }
            }
            thread::sleep(Duration::from_millis(1000));
        }
    }

    fn socket_accept_task(sender: Sender<DeviceUpdateMessage>) {
        let listener = TcpListener::bind(SocketAddr::V4(SocketAddrV4::new(
            Ipv4Addr::new(0, 0, 0, 0),
            HEIMDALL_PORT,
        )))
        .expect("Heimdall server unable to bind to socket");
        loop {
            match listener.accept() {
                Ok((socket, addr)) => {
                    let task_sender = sender.clone();
                    let thread_name = ["HeimdallStreamThread-", &addr.to_string()].concat();
                    thread::Builder::new()
                        .name(thread_name.clone())
                        .spawn(move || Self::socket_stream_task(socket, task_sender))
                        .expect("Could not spawn HeimdallServerThread");
                }
                Err(e) => {
                    error!("Heimdall server could not accept client: {e}");
                }
            }
        }
    }

    fn socket_stream_task(mut client: TcpStream, sender: Sender<DeviceUpdateMessage>) {
        let peer_address = client.peer_addr().expect("Unable to obtain peer address");
        info!("accepted socket connection from {peer_address}");
        loop {
            let mut buf = [0; std::mem::size_of::<usize>()];
            if let Err(ex) = client.read_exact(&mut buf) {
                error!(%peer_address, %ex, "Unable to read message size");
                break;
            }
            let message_size = usize::from_le_bytes(buf);
            if message_size == 0 {
                error!(%peer_address, "Received a message of size 0");
                continue;
            }
            let mut data = vec![0; message_size];
            if let Err(ex) = client.read_exact(&mut data.as_mut_slice()) {
                error!(%peer_address, %ex, "Unable to read message body");
                break;
            }
            match DeviceUpdateMessage::try_from(data) {
                Ok(message) => {
                    trace!("Received {message:?} from {peer_address}");
                    // receiver won't be disconnected
                    let _ = sender.send(message);
                }
                Err(e) => {
                    error!("{e}");
                }
            }
        }
    }
}
