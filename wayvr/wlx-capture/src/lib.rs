#![allow(dead_code)]
#![allow(clippy::expect_fun_call)]

pub use drm_fourcc::{DrmFormat, DrmFourcc, DrmModifier};
use frame::WlxFrame;
pub use libc;
#[cfg(feature = "wayland")]
pub use smithay_client_toolkit::reexports::protocols_wlr as wayland_protocols_wlr;
#[cfg(feature = "wayland")]
pub use wayland_client;
#[cfg(feature = "wayland")]
pub use wayland_protocols;

pub mod frame;

#[cfg(feature = "wayland")]
pub mod wayland;

#[cfg(feature = "wlr")]
pub mod wlr_dmabuf;

#[cfg(feature = "wlr")]
pub mod wlr_screencopy;

#[cfg(feature = "pipewire")]
pub mod pipewire;

#[cfg(feature = "xshm")]
pub mod xshm;

pub trait WlxCapture<U, R> {
    fn init(
        &mut self,
        dmabuf_formats: &[DrmFormat],
        user_data: U,
        callback: fn(&U, WlxFrame) -> Option<R>,
    );
    fn is_ready(&self) -> bool;
    fn supports_dmbuf(&self) -> bool;
    fn receive(&mut self) -> Option<R>;
    fn pause(&mut self);
    fn resume(&mut self);
    fn request_new_frame(&mut self);
}
