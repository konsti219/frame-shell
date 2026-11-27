//! A virtual DualSense on `/dev/uhid`, so the kernel's `hid-playstation` driver and SDL/Steam (through hidraw)
//! see a real DualSense with motion sensors. Emulated as the Bluetooth variant, the way Sunshine's inputtino does it.
//!
//! Report layouts and feature data follow inputtino (MIT, Copyright (c) 2021-2024 Games on Whales) and the kernel's
//! `drivers/hid/hid-playstation.c`.

mod tables;

use std::{
    fs::{File, OpenOptions},
    io::{self, Read, Write},
    os::fd::AsRawFd,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread::JoinHandle,
    time::{SystemTime, UNIX_EPOCH},
};

// struct uhid_event: a u32 type followed by the largest request (uhid_create2_req: 128 + 64 + 64 + 2 + 2 + 4 * 4 +
// 4096 bytes)
const UHID_EVENT_SIZE: usize = 4 + 128 + 64 + 64 + 2 + 2 + 16 + 4096;
const UHID_DESTROY: u32 = 1;
const UHID_OUTPUT: u32 = 6;
const UHID_GET_REPORT: u32 = 9;
const UHID_GET_REPORT_REPLY: u32 = 10;
const UHID_CREATE2: u32 = 11;
const UHID_INPUT2: u32 = 12;
const UHID_SET_REPORT: u32 = 13;
const UHID_SET_REPORT_REPLY: u32 = 14;

const BUS_BLUETOOTH: u16 = 0x05;
const SONY_VENDOR: u32 = 0x054C;
const DUALSENSE_PRODUCT: u32 = 0x0CE6;

const REPORT_CALIBRATION: u8 = 0x05;
const REPORT_PAIRING_INFO: u8 = 0x09;
const REPORT_FIRMWARE_INFO: u8 = 0x20;

const INPUT_REPORT_BT: u8 = 0x31;
const INPUT_REPORT_BT_SIZE: usize = 78;
const CRC_SEED_INPUT: u8 = 0xA1;
const CRC_SEED_FEATURE: u8 = 0xA3;

/// Raw gyro units per degree per second, from the calibration data the device reports
/// (speed 2 x 500 over a +-10000 range: hid-playstation and SDL both arrive at 1/20 deg/s)
const GYRO_PER_DEG_S: f32 = 20.0;
/// Raw accelerometer units per g (+-10000 range for 2 x 1 g)
const ACCEL_PER_G: f32 = 10000.0;
const STANDARD_GRAVITY: f32 = 9.806_65;

/// DualSense buttons, as the flags of [`Ds5State::buttons`]
pub mod button {
    pub const CROSS: u32 = 1 << 0;
    pub const CIRCLE: u32 = 1 << 1;
    pub const SQUARE: u32 = 1 << 2;
    pub const TRIANGLE: u32 = 1 << 3;
    pub const L1: u32 = 1 << 4;
    pub const R1: u32 = 1 << 5;
    pub const CREATE: u32 = 1 << 6;
    pub const OPTIONS: u32 = 1 << 7;
    pub const L3: u32 = 1 << 8;
    pub const R3: u32 = 1 << 9;
    pub const PS: u32 = 1 << 10;
    pub const TOUCHPAD: u32 = 1 << 11;
    pub const DPAD_UP: u32 = 1 << 12;
    pub const DPAD_DOWN: u32 = 1 << 13;
    pub const DPAD_LEFT: u32 = 1 << 14;
    pub const DPAD_RIGHT: u32 = 1 << 15;
}

/// One frame of controller state
#[derive(Debug, Clone, Copy, Default)]
pub struct Ds5State {
    /// Sticks, -1..1, +y up
    pub left_stick: [f32; 2],
    pub right_stick: [f32; 2],
    /// Triggers, 0..1
    pub l2: f32,
    pub r2: f32,
    /// [`button`] flags
    pub buttons: u32,
    /// Angular velocity in deg/s, SDL's gamepad axes (+x right, +y up, +z towards the player)
    pub gyro: [f32; 3],
    /// Acceleration in m/s^2 including the reaction to gravity (+9.8 on +y when held flat), same axes
    pub accel: [f32; 3],
}

/// Rumble requested by the game, 0..1 per motor (left = strong, right = weak)
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Ds5Rumble {
    pub left: f32,
    pub right: f32,
}

pub struct VirtualDs5 {
    file: Arc<File>,
    stop: Arc<AtomicBool>,
    reader: Option<JoinHandle<()>>,
    rumble: Arc<Mutex<Ds5Rumble>>,
    seq: u8,
}

impl VirtualDs5 {
    /// Creates the device; `hid-playstation` probes it in the background (it asks for the feature reports, which the
    /// reader thread answers)
    pub fn create(name: &str) -> io::Result<Self> {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .open("/dev/uhid")?;
        let mac = mac_address();
        let uniq = mac
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<Vec<_>>()
            .join(":");

        let mut event = [0u8; UHID_EVENT_SIZE];
        event[..4].copy_from_slice(&UHID_CREATE2.to_ne_bytes());
        let request = &mut event[4..];
        put_str(&mut request[0..128], name);
        put_str(&mut request[128..192], "frame-shell-gamepad");
        put_str(&mut request[192..256], &uniq);
        let descriptor = &tables::REPORT_DESCRIPTOR_BT;
        request[256..258].copy_from_slice(&(descriptor.len() as u16).to_ne_bytes());
        request[258..260].copy_from_slice(&BUS_BLUETOOTH.to_ne_bytes());
        request[260..264].copy_from_slice(&SONY_VENDOR.to_ne_bytes());
        request[264..268].copy_from_slice(&DUALSENSE_PRODUCT.to_ne_bytes());
        request[268..272].copy_from_slice(&0x0100u32.to_ne_bytes()); // version
        request[272..276].copy_from_slice(&0u32.to_ne_bytes()); // country
        request[276..276 + descriptor.len()].copy_from_slice(descriptor);
        (&file).write_all(&event)?;

        let file = Arc::new(file);
        let stop = Arc::new(AtomicBool::new(false));
        let rumble = Arc::new(Mutex::new(Ds5Rumble::default()));
        let reader = {
            let (file, stop, rumble) = (file.clone(), stop.clone(), rumble.clone());
            std::thread::Builder::new()
                .name("ds5-uhid".into())
                .spawn(move || read_events(&file, &stop, &rumble, mac))?
        };

        Ok(Self {
            file,
            stop,
            reader: Some(reader),
            rumble,
            seq: 0,
        })
    }

    pub fn send(&mut self, state: &Ds5State) -> io::Result<()> {
        let report = input_report(state, self.seq);
        self.seq = self.seq.wrapping_add(1);

        let mut event = [0u8; 4 + 2 + INPUT_REPORT_BT_SIZE];
        event[..4].copy_from_slice(&UHID_INPUT2.to_ne_bytes());
        event[4..6].copy_from_slice(&(INPUT_REPORT_BT_SIZE as u16).to_ne_bytes());
        event[6..].copy_from_slice(&report);
        (&*self.file).write_all(&event)
    }

    pub fn rumble(&self) -> Ds5Rumble {
        self.rumble
            .lock()
            .map_or_else(|_| Ds5Rumble::default(), |rumble| *rumble)
    }
}

impl Drop for VirtualDs5 {
    fn drop(&mut self) {
        let mut event = [0u8; 4];
        event.copy_from_slice(&UHID_DESTROY.to_ne_bytes());
        let _ = (&*self.file).write_all(&event);
        self.stop.store(true, Ordering::Relaxed);
        if let Some(reader) = self.reader.take() {
            let _ = reader.join();
        }
    }
}

fn input_report(state: &Ds5State, seq: u8) -> [u8; INPUT_REPORT_BT_SIZE] {
    let mut data = [0u8; INPUT_REPORT_BT_SIZE];
    data[0] = INPUT_REPORT_BT;
    let report = &mut data[2..];

    let stick = |value: f32, invert: bool| {
        let value = if invert { -value } else { value };
        (128.0 + value.clamp(-1.0, 1.0) * 127.5)
            .round()
            .clamp(0.0, 255.0) as u8
    };
    report[0] = stick(state.left_stick[0], false);
    report[1] = stick(state.left_stick[1], true); // the DualSense's y grows downwards
    report[2] = stick(state.right_stick[0], false);
    report[3] = stick(state.right_stick[1], true);
    let trigger = |value: f32| (value.clamp(0.0, 1.0) * 255.0).round() as u8;
    report[4] = trigger(state.l2);
    report[5] = trigger(state.r2);
    report[6] = seq;

    let pressed = |flag: u32| state.buttons & flag != 0;
    let mut buttons0 = hat(state.buttons);
    for (flag, bit) in [
        (button::SQUARE, 0x10),
        (button::CROSS, 0x20),
        (button::CIRCLE, 0x40),
        (button::TRIANGLE, 0x80),
    ] {
        if pressed(flag) {
            buttons0 |= bit;
        }
    }
    let mut buttons1 = 0u8;
    for (flag, bit) in [
        (button::L1, 0x01),
        (button::R1, 0x02),
        (button::CREATE, 0x10),
        (button::OPTIONS, 0x20),
        (button::L3, 0x40),
        (button::R3, 0x80),
    ] {
        if pressed(flag) {
            buttons1 |= bit;
        }
    }
    // L2/R2 digital bits follow the analog values
    if report[4] > 0 {
        buttons1 |= 0x04;
    }
    if report[5] > 0 {
        buttons1 |= 0x08;
    }
    let mut buttons2 = 0u8;
    if pressed(button::PS) {
        buttons2 |= 0x01;
    }
    if pressed(button::TOUCHPAD) {
        buttons2 |= 0x02;
    }
    report[7] = buttons0;
    report[8] = buttons1;
    report[9] = buttons2;

    for axis in 0..3 {
        let gyro = to_i16(state.gyro[axis] * GYRO_PER_DEG_S);
        let accel = to_i16(state.accel[axis] / STANDARD_GRAVITY * ACCEL_PER_G);
        report[15 + axis * 2..17 + axis * 2].copy_from_slice(&gyro.to_le_bytes());
        report[21 + axis * 2..23 + axis * 2].copy_from_slice(&accel.to_le_bytes());
    }
    // Sensor timestamp in units of 1/3 µs
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    report[27..31].copy_from_slice(&((now / 333) as u32).to_le_bytes());

    // Both touch points inactive (contact bit set)
    report[32] = 0x80;
    report[36] = 0x80;
    // Battery: full (status 2), charge 10 = 100 %, so games don't warn about it
    report[52] = 0x2A;

    let crc = crc32(CRC_SEED_INPUT, &data[..INPUT_REPORT_BT_SIZE - 4]);
    data[INPUT_REPORT_BT_SIZE - 4..].copy_from_slice(&crc.to_le_bytes());
    data
}

const fn hat(buttons: u32) -> u8 {
    let up = buttons & button::DPAD_UP != 0;
    let down = buttons & button::DPAD_DOWN != 0;
    let left = buttons & button::DPAD_LEFT != 0;
    let right = buttons & button::DPAD_RIGHT != 0;
    match (up, down, left, right) {
        (true, false, false, false) => 0,
        (true, false, false, true) => 1,
        (false, false, false, true) => 2,
        (false, true, false, true) => 3,
        (false, true, false, false) => 4,
        (false, true, true, false) => 5,
        (false, false, true, false) => 6,
        (true, false, true, false) => 7,
        _ => 8, // neutral
    }
}

fn to_i16(value: f32) -> i16 {
    value
        .round()
        .clamp(f32::from(i16::MIN), f32::from(i16::MAX)) as i16
}

fn read_events(file: &File, stop: &AtomicBool, rumble: &Mutex<Ds5Rumble>, mac: [u8; 6]) {
    let mut event = vec![0u8; UHID_EVENT_SIZE];
    while !stop.load(Ordering::Relaxed) {
        let mut poll = libc::pollfd {
            fd: file.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: one valid pollfd
        let ready = unsafe { libc::poll(&raw mut poll, 1, 100) };
        if ready <= 0 || poll.revents & libc::POLLIN == 0 {
            continue;
        }
        let Ok(size) = (&*file).read(&mut event) else {
            continue;
        };
        if size < 4 {
            continue;
        }
        let kind = u32::from_ne_bytes([event[0], event[1], event[2], event[3]]);
        let payload = &event[4..size];
        let result = match kind {
            UHID_GET_REPORT if payload.len() >= 5 => {
                let id = [payload[0], payload[1], payload[2], payload[3]];
                reply_get_report(file, id, payload[4], mac)
            }
            UHID_SET_REPORT if payload.len() >= 4 => {
                // Accept everything (SDL sets the Bluetooth "enhanced" mode this way)
                let mut reply = [0u8; 4 + 4 + 2];
                reply[..4].copy_from_slice(&UHID_SET_REPORT_REPLY.to_ne_bytes());
                reply[4..8].copy_from_slice(&payload[..4]);
                (&*file).write_all(&reply)
            }
            UHID_OUTPUT if payload.len() >= 4096 + 2 => {
                // struct uhid_output_req: data[4096], u16 size, u8 rtype
                let length = usize::from(u16::from_ne_bytes([payload[4096], payload[4097]]));
                if let Some(update) = parse_rumble(&payload[..length.min(4096)])
                    && let Ok(mut rumble) = rumble.lock()
                {
                    *rumble = update;
                }
                Ok(())
            }
            _ => Ok(()),
        };
        if let Err(error) = result {
            log::warn!("Virtual DualSense: {error}");
        }
    }
}

fn reply_get_report(file: &File, id: [u8; 4], report: u8, mac: [u8; 6]) -> io::Result<()> {
    let data: Option<Vec<u8>> = match report {
        REPORT_CALIBRATION => Some(tables::CALIBRATION.to_vec()),
        REPORT_FIRMWARE_INFO => Some(tables::FIRMWARE_INFO.to_vec()),
        REPORT_PAIRING_INFO => {
            let mut data = tables::PAIRING_INFO.to_vec();
            // The MAC address, least significant byte first
            for (index, byte) in mac.iter().rev().enumerate() {
                data[1 + index] = *byte;
            }
            Some(data)
        }
        _ => None,
    };

    // struct uhid_get_report_reply_req: u32 id, u16 err, u16 size, data[4096]
    let mut reply = vec![0u8; 4 + 4 + 2 + 2 + 4096];
    reply[..4].copy_from_slice(&UHID_GET_REPORT_REPLY.to_ne_bytes());
    reply[4..8].copy_from_slice(&id);
    match data {
        Some(mut data) => {
            // Bluetooth feature reports end in a CRC32 over the seed byte and the rest of the report
            let end = data.len() - 4;
            let crc = crc32(CRC_SEED_FEATURE, &data[..end]);
            data[end..].copy_from_slice(&crc.to_le_bytes());
            reply[10..12].copy_from_slice(&(data.len() as u16).to_ne_bytes());
            reply[12..12 + data.len()].copy_from_slice(&data);
        }
        None => reply[8..10].copy_from_slice(&(libc::EINVAL as u16).to_ne_bytes()),
    }
    (&*file).write_all(&reply)
}

/// The motors from an output report: USB (0x02) or Bluetooth (0x31, from SDL with its "enable HID" byte or from
/// the kernel with a sequence byte and a tag before the common part)
fn parse_rumble(data: &[u8]) -> Option<Ds5Rumble> {
    let common = match *data.first()? {
        0x02 => 1,
        0x31 if data.get(1)? & 0x02 != 0 => 2,
        0x31 => 3,
        _ => return None,
    };
    let (flag0, flag2) = (*data.get(common)?, *data.get(common + 38).unwrap_or(&0));
    let (right, left) = (*data.get(common + 2)?, *data.get(common + 3)?);
    if flag0 & 0x03 != 0 || flag2 & 0x04 != 0 {
        Some(Ds5Rumble {
            left: f32::from(left) / 255.0,
            right: f32::from(right) / 255.0,
        })
    } else if flag0 == 0 && data.get(common + 1).copied() == Some(0) && flag2 == 0 {
        Some(Ds5Rumble::default())
    } else {
        None
    }
}

fn put_str(field: &mut [u8], value: &str) {
    let bytes = value.as_bytes();
    let length = bytes.len().min(field.len() - 1);
    field[..length].copy_from_slice(&bytes[..length]);
}

/// A locally administered MAC address that is stable for this process
fn mac_address() -> [u8; 6] {
    let seed = u64::from(std::process::id())
        ^ SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
    let bytes = seed.to_le_bytes();
    [0x02, 0x46, bytes[0], bytes[1], bytes[2], bytes[3]]
}

/// CRC-32 (IEEE) over `seed` followed by `data`, as the DualSense uses on Bluetooth
fn crc32(seed: u8, data: &[u8]) -> u32 {
    let mut crc = !0u32;
    for &byte in std::iter::once(&seed).chain(data) {
        crc ^= u32::from(byte);
        for _ in 0..8 {
            crc = if crc & 1 != 0 {
                (crc >> 1) ^ 0xEDB8_8320
            } else {
                crc >> 1
            };
        }
    }
    !crc
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crc32_matches_the_standard_check_value() {
        // CRC-32/ISO-HDLC of "123456789" is 0xCBF43926; with a seed byte prepended it must equal the plain CRC of
        // the concatenation
        let plain = {
            let mut crc = !0u32;
            for &byte in b"123456789" {
                crc ^= u32::from(byte);
                for _ in 0..8 {
                    crc = if crc & 1 != 0 {
                        (crc >> 1) ^ 0xEDB8_8320
                    } else {
                        crc >> 1
                    };
                }
            }
            !crc
        };
        assert_eq!(plain, 0xCBF4_3926);
        assert_eq!(crc32(b'1', b"23456789"), 0xCBF4_3926);
    }

    #[test]
    fn neutral_report() {
        let report = input_report(&Ds5State::default(), 7);
        assert_eq!(report[0], 0x31);
        assert_eq!(&report[2..6], &[128, 128, 128, 128]);
        assert_eq!(report[2 + 7] & 0x0F, 8, "hat neutral");
        let crc = crc32(CRC_SEED_INPUT, &report[..74]);
        assert_eq!(&report[74..], &crc.to_le_bytes());
    }

    #[test]
    fn motion_units() {
        let state = Ds5State {
            gyro: [10.0, 0.0, 0.0],
            accel: [0.0, STANDARD_GRAVITY, 0.0],
            ..Default::default()
        };
        let report = input_report(&state, 0);
        let gyro_x = i16::from_le_bytes([report[2 + 15], report[2 + 16]]);
        let accel_y = i16::from_le_bytes([report[2 + 23], report[2 + 24]]);
        assert_eq!(gyro_x, 200);
        assert_eq!(accel_y, 10000);
    }

    #[test]
    fn rumble_from_sdl_and_kernel_reports() {
        let mut sdl = vec![0u8; 78];
        sdl[0] = 0x31;
        sdl[1] = 0x02;
        sdl[2] = 0x03; // valid_flag0
        sdl[4] = 51; // right
        sdl[5] = 255; // left
        assert_eq!(
            parse_rumble(&sdl),
            Some(Ds5Rumble {
                left: 1.0,
                right: 0.2
            })
        );
        let mut usb = vec![0u8; 48];
        usb[0] = 0x02;
        usb[1] = 0x01;
        usb[3] = 255;
        assert_eq!(
            parse_rumble(&usb),
            Some(Ds5Rumble {
                left: 0.0,
                right: 1.0
            })
        );
    }
}
