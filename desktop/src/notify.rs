//! Desktop notifications (org.freedesktop.Notifications), e.g. which monitor to pick in the screen cast dialog.

use dbus::arg::{PropMap, Variant};
use dbus::blocking::Connection;
use std::time::Duration;

const DEST: &str = "org.freedesktop.Notifications";
const PATH: &str = "/org/freedesktop/Notifications";

/// A shown notification, closed when dropped.
pub struct Notification {
    connection: Connection,
    id: u32,
}

impl Notification {
    /// Shows a notification that stays until it is dropped (or the user dismisses it).
    pub fn show(summary: &str, body: &str) -> anyhow::Result<Self> {
        let connection = Connection::new_session()?;
        let mut hints = PropMap::new();
        hints.insert("urgency".into(), Variant(Box::new(2u8)));
        let (id,): (u32,) = connection
            .with_proxy(DEST, PATH, Duration::from_millis(500))
            .method_call(
                DEST,
                "Notify",
                ("Frame shell", 0u32, "video-display", summary, body, Vec::<&str>::new(), hints, 0i32),
            )?;
        Ok(Self { connection, id })
    }
}

impl Drop for Notification {
    fn drop(&mut self) {
        let _: Result<(), _> = self
            .connection
            .with_proxy(DEST, PATH, Duration::from_millis(500))
            .method_call(DEST, "CloseNotification", (self.id,));
    }
}
