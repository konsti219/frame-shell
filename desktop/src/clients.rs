//! The PC runtime's clients, for the shell's top row; `FocusClient` makes one the shown one
use crate::Output;
use std::{sync::mpsc::Receiver, time::Duration};
use wayvr_ipc::{frame::HostMessage, runtime::Runtime};

pub fn run(output: &Output, focus: &Receiver<u32>) {
    let mut runtime: Option<Runtime> = None;
    let mut sent = None;
    loop {
        if runtime.is_none() {
            runtime = Runtime::connect().ok();
        }
        let clients = match &runtime {
            Some(connected) => {
                for client in focus.try_iter() {
                    if let Err(error) = connected.set_primary(client) {
                        eprintln!("Runtime client {client}: {error:#}");
                    }
                }
                connected.clients().unwrap_or_else(|_| {
                    runtime = None;
                    Vec::new()
                })
            }
            None => {
                focus.try_iter().for_each(drop);
                Vec::new()
            }
        };
        if sent.as_ref() != Some(&clients) {
            output.send(&HostMessage::Clients(clients.clone()));
            sent = Some(clients);
        }
        if !output.sleep(Duration::from_millis(500)) {
            return;
        }
    }
}
