//! Notifications for the headset: XSOverlay-style UDP messages (VRCX's "WayVR notifications") and the PC's desktop
//! notifications, read like WayVR does on its own machine.
use crate::Output;
use dbus::{blocking::Connection, channel::MatchingReceiver, message::MatchRule};
use serde::Deserialize;
use std::{net::UdpSocket, time::Duration};
use wayvr_ipc::frame::{HostMessage, Notification, NotificationSource};

#[allow(non_snake_case)]
#[derive(Deserialize)]
struct XsoMessage {
    messageType: i32,
    volume: Option<f32>,
    timeout: Option<f32>,
    title: String,
    content: Option<String>,
}

pub fn run_udp(output: &Output) {
    let socket = match UdpSocket::bind("127.0.0.1:42069") {
        Ok(socket) => socket,
        Err(error) => {
            eprintln!("Overlay notification socket: {error}");
            return;
        }
    };
    let _ = socket.set_read_timeout(Some(Duration::from_millis(200)));
    // VRCX embeds icons as base64
    let mut buffer = vec![0u8; 64 * 1024];
    while !output.stopped() {
        let Ok((size, _)) = socket.recv_from(&mut buffer) else {
            continue;
        };
        let Ok(message) = serde_json::from_slice::<XsoMessage>(&buffer[..size]) else {
            eprintln!("Invalid overlay notification");
            continue;
        };
        if message.messageType != 1 {
            continue;
        }
        output.send(&HostMessage::Notification(Notification {
            source: NotificationSource::Overlay,
            title: message.title,
            body: message.content.unwrap_or_default(),
            timeout_seconds: message.timeout.unwrap_or(5.0),
            // XSOverlay plays a sound at volume 0 too
            sound: message.volume.unwrap_or(-1.0) >= 0.0,
        }));
    }
}

pub fn run_dbus(output: &Output) {
    if let Err(error) = monitor(output) {
        eprintln!("Desktop notifications: {error}");
    }
}

fn monitor(output: &Output) -> Result<(), dbus::Error> {
    let connection = Connection::new_session()?;
    let rule = MatchRule::new_method_call()
        .with_member("Notify")
        .with_interface("org.freedesktop.Notifications")
        .with_path("/org/freedesktop/Notifications");
    connection
        .with_proxy(
            "org.freedesktop.DBus",
            "/org/freedesktop/DBus",
            Duration::from_secs(5),
        )
        .method_call::<(), _, _, _>(
            "org.freedesktop.DBus.Monitoring",
            "BecomeMonitor",
            (vec![rule.match_str()], 0u32),
        )?;
    let sender = output.clone();
    connection.start_receive(
        rule,
        Box::new(move |message, _| {
            if let Some(notification) = parse(&message) {
                sender.send(&HostMessage::Notification(notification));
            }
            true
        }),
    );
    while !output.stopped() {
        connection.process(Duration::from_millis(200))?;
    }
    Ok(())
}

fn parse(message: &dbus::Message) -> Option<Notification> {
    let mut args = message.iter_init();
    let app_name: String = args.read().ok()?;
    let _replaces_id: u32 = args.read().ok()?;
    let _app_icon: String = args.read().ok()?;
    let summary: String = args.read().ok()?;
    let body: String = args.read().ok()?;
    Some(Notification {
        source: NotificationSource::Desktop,
        title: if summary.is_empty() {
            app_name
        } else {
            summary
        },
        body,
        timeout_seconds: 5.0,
        // The PC plays its own sounds
        sound: false,
    })
}
