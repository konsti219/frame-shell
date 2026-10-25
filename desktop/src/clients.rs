//! The PC runtime's clients, for the shell's top row; `FocusClient` makes one the shown one
use crate::Output;
use std::{sync::mpsc::Receiver, time::Duration};
use wayvr_ipc::{frame::HostMessage, runtime::Runtime};

pub enum Command {
    Client(u32, wayvr_ipc::frame::ClientAction),
    Background(bool),
    Refresh,
}

pub fn run(output: &Output, focus: &Receiver<Command>) {
    let mut runtime: Option<Runtime> = None;
    let mut sent = None;
    let mut background = true;
    let mut applied_background = None;
    // Reported once per value, not on every retry
    let mut failed_background = None;
    let mut announced = false;
    loop {
        if runtime.is_none() {
            runtime = Runtime::connect().ok();
        }
        let clients = match &runtime {
            Some(connected) => {
                for command in focus.try_iter() {
                    match command {
                        Command::Client(client, action) => {
                            if let Err(error) = connected.control(client, action) {
                                output.send(&HostMessage::Error(format!(
                                    "Changing application {client}: {error:#}"
                                )));
                            }
                        }
                        Command::Background(visible) => background = visible,
                        Command::Refresh => {
                            sent = None;
                            if announced {
                                output.send(&HostMessage::CompositionSupported);
                            }
                        }
                    }
                }
                if applied_background != Some(background) {
                    match connected.background_visible(background) {
                        Ok(()) => {
                            applied_background = Some(background);
                            failed_background = None;
                            if !announced {
                                output.send(&HostMessage::CompositionSupported);
                                announced = true;
                            }
                        }
                        Err(error) => {
                            if failed_background != Some(background) {
                                output.send(&HostMessage::Error(format!(
                                    "Setting remote background: {error:#}"
                                )));
                                failed_background = Some(background);
                            }
                        }
                    }
                }
                connected.clients().ok().or_else(|| {
                    runtime = None;
                    applied_background = None;
                    None
                })
            }
            None => {
                for command in focus.try_iter() {
                    match command {
                        Command::Background(visible) => background = visible,
                        Command::Refresh => sent = None,
                        Command::Client(_, _) => {}
                    }
                }
                None
            }
        };
        if sent.as_ref() != Some(&clients) {
            match &clients {
                Some(clients) => output.send(&HostMessage::Clients(clients.clone())),
                None => output.send(&HostMessage::RuntimeUnavailable),
            }
            sent = Some(clients);
        }
        if !output.sleep(Duration::from_millis(500)) {
            return;
        }
    }
}
