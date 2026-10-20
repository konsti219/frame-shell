use wlx_common::config::InputEmulationMethod;

use super::hid::{self, VirtualKey};

use crate::subsystem::hid::provider::HidProvider;
use crate::subsystem::hid::provider::dummy::DummyProvider;
use crate::{backend::wayvr::WvrServerState, overlays::toast::Toast, subsystem::hid::XkbKeymap};

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum InputFocus {
    PhysicalScreen,
    WayVR, // (wayland window id data is handled internally),
}

pub struct HidWrapper {
    input_focus: InputFocus,
    pub inner: Box<dyn HidProvider>,
    pub keymap: Option<XkbKeymap>,
    held_keys: Vec<VirtualKey>,
    modifiers: u8,
}

#[allow(clippy::map_unwrap_or)]
impl HidWrapper {
    pub fn new(method: InputEmulationMethod) -> (Self, Option<Toast>) {
        let maybe_provider = match method {
            InputEmulationMethod::Uinput => hid::provider::uinput::initialize_uinput(),
            InputEmulationMethod::WlVirtual => hid::provider::wl_virtual::initialize_wl_virtual(),
            InputEmulationMethod::None => Ok(hid::provider::dummy::initialize_dummy()),
        };

        let (provider, toast) = maybe_provider
            .map(|provider| (provider, None))
            .unwrap_or_else(|toast| (Box::new(DummyProvider {}), Some(toast)));

        (
            Self {
                input_focus: InputFocus::PhysicalScreen,
                inner: provider,
                keymap: None,
                held_keys: Vec::new(),
                modifiers: 0,
            },
            toast,
        )
    }

    pub fn set_input_focus(
        &mut self,
        mut wvr_server: Option<&mut WvrServerState>,
        value: InputFocus,
    ) -> bool {
        if self.input_focus == value {
            false
        } else {
            match self.input_focus {
                InputFocus::PhysicalScreen => {
                    for key in self.held_keys.drain(..) {
                        self.inner.send_key(key, false);
                    }
                    self.inner.set_modifiers(0);
                    self.inner.release();
                }
                InputFocus::WayVR => {
                    if let Some(server) = wvr_server.as_deref_mut() {
                        for key in self.held_keys.drain(..) {
                            server.send_key(key as u32, false);
                        }
                        server.set_modifiers(0);
                    } else {
                        self.held_keys.clear();
                    }
                }
            }
            self.input_focus = value;

            if let Some(server) = wvr_server.as_deref_mut() {
                server.set_input_focus(matches!(value, InputFocus::WayVR));
            }
            self.set_modifiers_routed(wvr_server, self.modifiers);

            true
        }
    }

    pub const fn get_input_focus(&self) -> InputFocus {
        self.input_focus
    }

    pub fn send_key_routed(
        &mut self,
        wvr_server: Option<&mut WvrServerState>,
        key: VirtualKey,
        down: bool,
    ) {
        if down {
            if !self.held_keys.contains(&key) {
                self.held_keys.push(key);
            }
        } else if let Some(index) = self.held_keys.iter().position(|held| *held == key) {
            self.held_keys.swap_remove(index);
        } else {
            return;
        }
        match self.input_focus {
            InputFocus::PhysicalScreen => self.inner.send_key(key, down),
            InputFocus::WayVR => {
                if let Some(wvr_server) = wvr_server {
                    wvr_server.send_key(key as u32, down);
                }
            }
        }
    }

    pub fn keymap_changed(&mut self, wvr_server: Option<&mut WvrServerState>, keymap: &XkbKeymap) {
        if let Some(wvr_server) = wvr_server {
            let _ = wvr_server
                .set_keymap(&keymap.inner)
                .inspect_err(|e| log::error!("Could not set WayVR keymap: {e:?}"));
        } else {
            self.keymap = Some(keymap.clone());
            self.inner.set_keymap(keymap);
        }

        log::info!(
            "Keymap changed: {}",
            keymap.inner.layouts().next().unwrap_or("Unknown")
        );
    }

    pub fn set_modifiers_routed(&mut self, wvr_server: Option<&mut WvrServerState>, mods: u8) {
        self.modifiers = mods;
        match self.input_focus {
            InputFocus::PhysicalScreen => self.inner.set_modifiers(mods),
            InputFocus::WayVR => {
                if let Some(wvr_server) = wvr_server {
                    wvr_server.set_modifiers(mods);
                }
            }
        }
    }
}
