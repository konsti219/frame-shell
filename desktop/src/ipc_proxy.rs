//! WayVR's IPC socket on the PC: its clients (the browser media bridge, wayvrctl) talk to the shell on the headset
//! as if it ran here. Packets are decoded and forwarded per connection; process control stays local to the headset.
use crate::{Output, lock};
use std::{
    collections::HashMap,
    io::{ErrorKind, Read, Write},
    net::Shutdown,
    os::{
        linux::net::SocketAddrExt,
        unix::net::{SocketAddr, UnixListener, UnixStream},
    },
    sync::{
        Mutex,
        atomic::{AtomicU32, Ordering},
        mpsc::{self, Receiver, Sender},
    },
    thread,
    time::Duration,
};
use wayvr_ipc::{
    frame::HostMessage, ipc, packet_client::PacketClient, packet_server::PacketServer,
};

const MAX_PACKET: usize = 128 * 1024;

struct Connection {
    socket: UnixStream,
    replies: Sender<Vec<u8>>,
}

#[derive(Default)]
pub struct Proxy {
    next: AtomicU32,
    connections: Mutex<HashMap<u32, Connection>>,
}

impl Proxy {
    /// A packet from the shell for one PC client
    pub fn send(&self, connection: u32, packet: &PacketServer) {
        let failed = lock(&self.connections)
            .get(&connection)
            .is_some_and(|client| client.replies.send(ipc::data_encode(packet)).is_err());
        if failed {
            self.close(connection);
        }
    }

    pub fn close(&self, connection: u32) {
        if let Some(stream) = lock(&self.connections).remove(&connection) {
            let _ = stream.socket.shutdown(Shutdown::Both);
        }
    }

    pub fn close_all(&self) {
        for (_, stream) in lock(&self.connections).drain() {
            let _ = stream.socket.shutdown(Shutdown::Both);
        }
    }
}

pub fn run(output: &Output, proxy: &Proxy) {
    let listener = SocketAddr::from_abstract_name(wayvr_ipc::ipc::SOCKET_NAME.as_bytes())
        .and_then(|address| UnixListener::bind_addr(&address))
        .and_then(|listener| listener.set_nonblocking(true).map(|()| listener));
    let listener = match listener {
        Ok(listener) => listener,
        Err(error) => {
            eprintln!("WayVR IPC socket: {error}");
            return;
        }
    };
    thread::scope(|scope| {
        while !output.stopped() {
            match listener.accept() {
                Ok((stream, _)) => {
                    let id = proxy.next.fetch_add(1, Ordering::Relaxed);
                    let (Ok(writer), Ok(socket)) = (stream.try_clone(), stream.try_clone()) else {
                        continue;
                    };
                    let (replies, receiver) = mpsc::channel();
                    lock(&proxy.connections).insert(id, Connection { socket, replies });
                    output.send(&HostMessage::IpcOpen(id));
                    scope.spawn(move || {
                        thread::scope(|writers| {
                            writers
                                .spawn(move || write_replies(output, proxy, id, writer, receiver));
                            if let Err(error) = serve(output, proxy, id, stream) {
                                eprintln!("WayVR IPC client: {error:#}");
                            }
                            proxy.close(id);
                        });
                        output.send(&HostMessage::IpcClose(id));
                    });
                }
                Err(error) if error.kind() == ErrorKind::WouldBlock => {
                    output.sleep(Duration::from_millis(100));
                }
                Err(error) => {
                    eprintln!("WayVR IPC socket: {error}");
                    output.sleep(Duration::from_secs(1));
                }
            }
        }
        proxy.close_all();
    });
}

fn serve(output: &Output, proxy: &Proxy, id: u32, mut stream: UnixStream) -> anyhow::Result<()> {
    stream.set_read_timeout(Some(Duration::from_millis(200)))?;
    let mut buffer = Vec::new();
    let mut chunk = [0u8; 16384];
    while !output.stopped() {
        match stream.read(&mut chunk) {
            Ok(0) => return Ok(()),
            Ok(count) => buffer.extend_from_slice(&chunk[..count]),
            Err(error)
                if matches!(
                    error.kind(),
                    ErrorKind::WouldBlock | ErrorKind::TimedOut | ErrorKind::Interrupted
                ) => {}
            Err(error) => return Err(error.into()),
        }
        while buffer.len() >= 4 {
            let length = u32::from_be_bytes(buffer[..4].try_into().unwrap()) as usize;
            anyhow::ensure!(length <= MAX_PACKET, "Packet too large");
            if buffer.len() < 4 + length {
                break;
            }
            let packet: PacketClient = ipc::data_decode(&buffer[4..4 + length])?;
            buffer.drain(..4 + length);
            match packet {
                // Starting and stopping processes on the headset isn't offered to PC programs
                PacketClient::WvrProcessLaunch(serial, _) => proxy.send(
                    id,
                    &PacketServer::WvrProcessLaunchResponse(
                        serial,
                        Err("Not available from the PC".into()),
                    ),
                ),
                PacketClient::WvrProcessTerminate(_) => {}
                packet => output.send(&HostMessage::Ipc(id, packet)),
            }
        }
    }
    Ok(())
}

fn write_replies(
    output: &Output,
    proxy: &Proxy,
    id: u32,
    mut stream: UnixStream,
    replies: Receiver<Vec<u8>>,
) {
    while !output.stopped() {
        match replies.recv_timeout(Duration::from_millis(200)) {
            Ok(data) if write_packet(&mut stream, &data).is_err() => break,
            Ok(_) | Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }
    proxy.close(id);
}

fn write_packet(stream: &mut UnixStream, data: &[u8]) -> std::io::Result<()> {
    stream.set_write_timeout(Some(Duration::from_millis(200)))?;
    stream.write_all(&(data.len() as u32).to_be_bytes())?;
    stream.write_all(data)
}
