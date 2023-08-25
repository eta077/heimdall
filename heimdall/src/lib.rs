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
use std::thread::JoinHandle;
use std::time::Duration;

pub const HEIMDALL_PORT: u16 = 4064;

pub type HeimdallState = Vec<Device>;

#[derive(Copy, Clone, Debug, Serialize)]
pub enum ConnectionType {
    Origin,
    Wired,
    Wireless,
}

#[derive(Copy, Clone, Debug, Serialize)]
pub enum DeviceStatus {
    Enabled,
    Disabled,
}

#[derive(Clone, Debug, Serialize)]
pub struct Device {
    pub name: String,
    pub connection: ConnectionType,
    pub capabilities: Vec<(String, bool)>,
    pub status: DeviceStatus,
    pub cpu_usage: f32,
    pub mem_usage: f32,
}

impl Device {
    pub fn new(name: String, connection: ConnectionType) -> Self {
        Device {
            name,
            connection,
            capabilities: Vec::new(),
            status: DeviceStatus::Enabled,
            cpu_usage: 0.0,
            mem_usage: 0.0,
        }
    }
}

#[derive(Debug)]
pub enum DeviceUpdateMessage {
    Create {
        name: String,
        connection: ConnectionType,
    },
    CpuMem {
        name: String,
        cpu: f32,
        mem: f32,
    },
}

impl From<DeviceUpdateMessage> for Vec<u8> {
    fn from(value: DeviceUpdateMessage) -> Self {
        match value {
            DeviceUpdateMessage::Create { name, connection } => {
                let mut buffer = Vec::new();
                // message type
                serialize_u8(0, &mut buffer);

                serialize_string(name, &mut buffer);
                serialize_u8(
                    match connection {
                        ConnectionType::Origin => 0,
                        ConnectionType::Wired => 1,
                        ConnectionType::Wireless => 2,
                    },
                    &mut buffer,
                );
                finalize_serialization(&mut buffer);
                buffer
            }
            DeviceUpdateMessage::CpuMem { name, cpu, mem } => {
                let mut buffer = Vec::new();
                // message type
                serialize_u8(1, &mut buffer);

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
        let message_type = deserialize_u8(&mut value)?;

        match message_type {
            0 => {
                let name = deserialize_string(&mut value)?;
                let connection_type = deserialize_u8(&mut value)?;

                let connection = match connection_type {
                    0 => ConnectionType::Origin,
                    1 => ConnectionType::Wired,
                    2 => ConnectionType::Wireless,
                    err => {
                        return Err(blt_utils::DeserializationError::InvalidValue(
                            ["Unexpected connection type: ", &err.to_string()].concat(),
                        ));
                    }
                };

                Ok(DeviceUpdateMessage::Create { name, connection })
            }
            1 => {
                let name = deserialize_string(&mut value)?;
                let cpu = deserialize_f32(&mut value)?;
                let mem = deserialize_f32(&mut value)?;

                Ok(DeviceUpdateMessage::CpuMem { name, cpu, mem })
            }
            err => Err(blt_utils::DeserializationError::InvalidValue(
                ["Unexpected message type: ", &err.to_string()].concat(),
            )),
        }
    }
}

#[derive(Default)]
pub struct HeimdallServer {
    state: Arc<Mutex<HashMap<String, Device>>>,
    state_senders: Arc<Mutex<Vec<Sender<HeimdallState>>>>,
    server_handle: Option<JoinHandle<()>>,
    socket_handle: Option<JoinHandle<()>>,
}

impl HeimdallServer {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn start(&mut self) {
        {
            let mut state = self.state.lock().expect("start could not lock state");
            state.clear();
            let router_name = String::from("Router");
            state.insert(
                router_name.clone(),
                Device::new(router_name, ConnectionType::Origin),
            );
        }

        let (sender, receiver) = unbounded();
        let state = Arc::clone(&self.state);
        let senders = Arc::clone(&self.state_senders);

        if let Some(handle) = self.server_handle.take() {
            let _ = handle.join();
        }

        self.server_handle = Some(
            thread::Builder::new()
                .name(String::from("HeimdallServerThread"))
                .spawn(|| Self::receiver_task(receiver, state, senders))
                .expect("Could not spawn HeimdallServerThread"),
        );

        if let Some(handle) = self.socket_handle.take() {
            let _ = handle.join();
        }

        self.socket_handle = Some(
            thread::Builder::new()
                .name(String::from("HeimdallSocketThread"))
                .spawn(|| Self::socket_accept_task(sender))
                .expect("Could not spawn HeimdallSocketThread"),
        );
    }

    pub fn add_listener(&self) -> Receiver<HeimdallState> {
        let (sender, receiver) = unbounded();
        // receiver will not be disconnected
        let _ = sender.send(
            self.state
                .lock()
                .expect("add_listener could not log state")
                .values()
                .cloned()
                .collect(),
        );
        self.state_senders
            .lock()
            .expect("add_listener could not lock state_senders")
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
                        DeviceUpdateMessage::Create { name, connection } => {
                            new_state.insert(name.clone(), Device::new(name, connection));
                        }
                        DeviceUpdateMessage::CpuMem { name, cpu, mem } => {
                            new_state.entry(name.clone()).and_modify(|device| {
                                device.cpu_usage = cpu;
                                device.mem_usage = mem;
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
