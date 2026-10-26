//! The C ABI the WiVRn client calls (frame_abi.h): its state, commands, connections and input

use super::*;

pub(crate) extern "C" fn drag_offset(output: *mut f32) {
    if output.is_null() {
        return;
    }
    let offset = *DRAG_OFFSET
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    unsafe { std::ptr::copy_nonoverlapping(offset.as_ptr(), output, offset.len()) };
}

pub(crate) extern "C" fn input_poll(event: *mut ShellInput) -> bool {
    if event.is_null() {
        return false;
    }
    if let Some(next) = INPUT
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .pop_front()
    {
        unsafe {
            event.write(next);
        }
        true
    } else {
        false
    }
}

pub(crate) extern "C" fn lobby_state(json: *const std::ffi::c_char) {
    let json = c_str(json);
    // A PC asking for its PIN opens the dashboard on the WiVRn tab
    let pairing = json.contains("\"state\":\"pin\"");
    let mut state = LOBBY_STATE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if pairing && !state.1.contains("\"state\":\"pin\"") {
        PAIRING_REQUEST.store(true, Ordering::Relaxed);
        SHOW_DASHBOARD.store(true, Ordering::Relaxed);
    }
    state.0 += 1;
    state.1 = json;
}

pub(crate) extern "C" fn stream_state(source: u32, json: *const std::ffi::c_char) {
    let json = c_str(json);
    // The PC encodes its desktops with the headset's settings
    if let Ok(value) = serde_json::from_str::<serde_json::Value>(&json)
        && let Some(settings) = value.get("settings")
        && let Some(bitrate) = settings
            .get("desktop_bitrate")
            .and_then(serde_json::Value::as_u64)
    {
        crate::frame_host::sync_desktop_settings(wayvr_ipc::frame::DesktopSettings {
            bitrate_per_1080p: bitrate as u32,
            hevc: settings
                .get("desktop_codec")
                .and_then(serde_json::Value::as_str)
                == Some("h265"),
        });
    }
    let mut state = LOBBY_STATE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    state.0 += 1;
    if let Ok(value) = serde_json::from_str(&json) {
        STREAM_STATES.lock().unwrap().insert(source, value);
    }
}

pub(crate) extern "C" fn command_poll(buffer: *mut std::ffi::c_char, size: usize) -> bool {
    if buffer.is_null() || size == 0 {
        return false;
    }
    let Some(command) = COMMANDS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .pop_front()
    else {
        return false;
    };
    let bytes = &command.as_bytes()[..command.len().min(size - 1)];
    unsafe {
        std::ptr::copy_nonoverlapping(bytes.as_ptr().cast(), buffer, bytes.len());
        *buffer.add(bytes.len()) = 0;
    }
    true
}

pub(crate) extern "C" fn connected(source: u32, connected: bool, vr: bool) {
    if connected {
        crate::frame_host::connected(source, vr);
        CONNECTED.store(crate::frame_host::vr_source().is_some(), Ordering::Relaxed);
        crate::frame_host::resync_desktop_settings();
        bump_lobby_state();
        return;
    }
    INPUT
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .retain(|event| event.source != source);
    let mut released = Vec::new();
    surfaces().retain(|&key, surface| {
        if (key >> 32) as u32 != source {
            return true;
        }
        if let Some((frame, _)) = surface.frame.take() {
            released.push(frame.id);
        }
        false
    });
    for id in released {
        release_frame(id);
    }
    crate::frame_host::disconnected(source);
    CONNECTED.store(crate::frame_host::vr_source().is_some(), Ordering::Relaxed);
    let mut state = LOBBY_STATE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    state.0 += 1;
    STREAM_STATES.lock().unwrap().remove(&source);
    if vr {
        COMMANDS
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .retain(|command| {
                !(command.starts_with("stop_app ") || command.starts_with("set_active_app "))
            });
    }
}

pub(crate) extern "C" fn stop() {
    crate::RUNNING.store(false, Ordering::Relaxed);
    disconnect_all();
}

/// The shell stops: every PC is gone for it
pub(crate) fn disconnect_all() {
    for (source, vr) in crate::frame_host::sources() {
        connected(source, false, vr);
    }
}

pub(crate) extern "C" fn run(callbacks: *const frame_shell_host) -> i32 {
    let Some(callbacks) = (unsafe { callbacks.as_ref() }) else {
        return 1;
    };
    *host() = Some(Host(*callbacks));
    match std::panic::catch_unwind(run_embedded) {
        Ok(Ok(())) => 0,
        Ok(Err(error)) => {
            eprintln!("Frame shell: {error:#}");
            1
        }
        Err(_) => {
            eprintln!("Frame shell panicked");
            2
        }
    }
}

pub(crate) static API: frame_shell_api = frame_shell_api {
    version: FRAME_SHELL_ABI_VERSION,
    run: Some(run),
    stop: Some(stop),
    input_poll: Some(input_poll),
    desktop_surface: Some(desktop_surface),
    desktop_frame: Some(desktop_frame),
    connected: Some(connected),
    lobby_state: Some(lobby_state),
    stream_state: Some(stream_state),
    host_message: Some(host_message),
    hosts_state: Some(hosts_state),
    command_poll: Some(command_poll),
    drag_offset: Some(drag_offset),
};

/// The shell's entry points for the WiVRn client (frame_abi.h)
#[unsafe(no_mangle)]
pub extern "C" fn frame_shell_api() -> *const frame_shell_api {
    &API
}
const _: frame_abi::frame_shell_api_fn = Some(frame_shell_api);

pub(crate) extern "C" fn host_message(source: u32, data: *const u8, size: usize) {
    if !data.is_null() {
        crate::frame_host::receive(source, unsafe { std::slice::from_raw_parts(data, size) });
    }
}

pub(crate) extern "C" fn hosts_state(pcs: *const frame_abi::frame_shell_pc, count: usize, selected: u32) {
    let was_pairing = crate::frame_hosts::pcs()
        .iter()
        .any(|pc| pc.connection == crate::frame_hosts::Connection::Pairing);
    let old = crate::frame_hosts::background_source();
    unsafe { crate::frame_hosts::update(pcs, count, selected) };
    crate::frame_host::sync_presentation();
    if old != crate::frame_hosts::background_source() {
        crate::frame_host::background_changed();
    }
    CONNECTED.store(crate::frame_host::vr_source().is_some(), Ordering::Relaxed);
    if !was_pairing
        && crate::frame_hosts::pcs()
            .iter()
            .any(|pc| pc.connection == crate::frame_hosts::Connection::Pairing)
    {
        PIN.lock().unwrap().clear();
        PAIRING_REQUEST.store(true, Ordering::Relaxed);
        SHOW_DASHBOARD.store(true, Ordering::Relaxed);
    }
    bump_lobby_state();
}
