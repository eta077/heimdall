//! Obtain resource statistics from devices on the local network

use crossbeam_channel::{unbounded, Receiver, Sender};

use serde::Serialize;

use sysinfo::CpuExt;
use sysinfo::System;
use sysinfo::SystemExt;

use tracing::{trace, warn};

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

pub type HeimdallState = Vec<Device>;

#[derive(Clone, Debug, Serialize)]
pub struct Device {
    pub name: String,
    pub cpu_usage: f32,
    pub mem_usage: f32,
}

enum DeviceUpdateMessage {
    CpuMem { name: String, cpu: f32, mem: f32 },
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
            .spawn(|| Self::socket_task(sender))
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

    fn socket_task(sender: Sender<DeviceUpdateMessage>) {
        let mut sys = System::new_all();

        loop {
            sys.refresh_all();
            let cpu_usage =
                sys.cpus().iter().map(|cpu| cpu.cpu_usage()).sum::<f32>() / sys.cpus().len() as f32;
            let mem_usage = (sys.used_memory() as f64 / sys.total_memory() as f64) as f32 * 100.0;
            trace!("cpu {cpu_usage} mem {mem_usage}");
            if sender
                .send(DeviceUpdateMessage::CpuMem {
                    name: String::from("Laptop"),
                    cpu: cpu_usage,
                    mem: mem_usage,
                })
                .is_err()
            {
                break;
            }
            thread::sleep(System::MINIMUM_CPU_UPDATE_INTERVAL * 2);
        }
    }
}
