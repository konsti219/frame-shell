//! Calibrates the eye tracking from pointer clicks: the user looks at what they click. Each click pairs its target
//! with the Steam Frame's (uncalibrated) gaze at that moment; a gain and offset per axis fitted over recent clicks is
//! written to ~/.config/frame-shell/eye-calibration, which Monado's gaze reader applies.

use glam::Vec3A;
use std::{collections::VecDeque, path::PathBuf, sync::Mutex};

const SHM_PATH: &[u8] = b"/dev/shm/eye-server.mmap\0";
// Shared memory version 5: header, then the gaze record
const VERSION: usize = 0x00;
const INITIALIZED: usize = 0x04;
const MUTEX: usize = 0x08;
const RECORD: usize = 0x157;
const MIN_SIZE: usize = RECORD + 0xa0;
const REC_TIME: usize = 0x05; // f64, CLOCK_MONOTONIC seconds
const REC_GAZE: usize = 0x0d; // f32[2][3], left and right after fusion
const REC_FIXATION: usize = 0x3d; // f32[3], head-relative point between the eyes

/// Clicks further than this from the gaze weren't looked at
const MAX_ERROR_DEG: f32 = 15.0;
/// Residual beyond which a click is left out of the second fit
const OUTLIER_DEG: f32 = 5.0;
/// Keeps the gain near 1 until the clicks span the view (deg², like one click this far from the centre)
const GAIN_PRIOR: f32 = 400.0;
const MIN_SAMPLES: usize = 5;
const MAX_SAMPLES: usize = 150;

#[derive(Clone, Copy)]
struct Sample {
    target: [f32; 2], // yaw (right), pitch (up), degrees
    gaze: [f32; 2],
}

#[derive(Clone, Copy, PartialEq)]
struct Axis {
    gain: f32,
    offset: f32,
}

struct State {
    base: Option<*const u8>,
    gaze: VecDeque<(f64, Vec3A)>,
    pending: Vec<(f64, Vec3A)>,
    samples: VecDeque<Sample>,
    fit: Option<[Axis; 2]>,
}

unsafe impl Send for State {}

static STATE: Mutex<Option<State>> = Mutex::new(None);

fn path() -> Option<PathBuf> {
    let config = std::env::var_os("XDG_CONFIG_HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))?;
    Some(config.join("frame-shell").join("eye-calibration"))
}

fn angles(v: Vec3A) -> [f32; 2] {
    [
        v.x.atan2(-v.z).to_degrees(),
        v.y.atan2(v.x.hypot(v.z)).to_degrees(),
    ]
}

fn map() -> Option<*const u8> {
    unsafe {
        let fd = libc::open(SHM_PATH.as_ptr().cast(), libc::O_RDWR | libc::O_CLOEXEC);
        if fd < 0 {
            return None;
        }
        let mut st: libc::stat = std::mem::zeroed();
        let size = if libc::fstat(fd, &mut st) == 0 { st.st_size as usize } else { 0 };
        let mem = if size >= MIN_SIZE {
            libc::mmap(std::ptr::null_mut(), size, libc::PROT_READ | libc::PROT_WRITE, libc::MAP_SHARED, fd, 0)
        } else {
            libc::MAP_FAILED
        };
        libc::close(fd);
        if mem == libc::MAP_FAILED {
            return None;
        }
        let base = mem.cast::<u8>();
        if base.add(VERSION).cast::<u32>().read_unaligned() != 5 {
            log::warn!("Eye tracking shared memory has an unknown version, no gaze calibration");
            libc::munmap(mem, size);
            return None;
        }
        Some(base.cast_const())
    }
}

/// The gaze record's time and direction, the combined gaze Monado reports before its correction
fn read_gaze(base: *const u8) -> Option<(f64, Vec3A)> {
    let mut rec = [0u8; 0xa0];
    unsafe {
        if base.add(INITIALIZED).cast::<u32>().read_volatile() != 1 {
            return None;
        }
        let mutex = base.add(MUTEX).cast_mut().cast::<libc::pthread_mutex_t>();
        match libc::pthread_mutex_lock(mutex) {
            0 => {}
            libc::EOWNERDEAD => {
                libc::pthread_mutex_consistent(mutex);
            }
            _ => return None,
        }
        std::ptr::copy_nonoverlapping(base.add(RECORD), rec.as_mut_ptr(), rec.len());
        libc::pthread_mutex_unlock(mutex);
    }
    let f32_at = |o: usize| f32::from_le_bytes(rec[o..o + 4].try_into().unwrap());
    let vec_at = |o: usize| Vec3A::new(f32_at(o), f32_at(o + 4), f32_at(o + 8));
    let time = f64::from_le_bytes(rec[REC_TIME..REC_TIME + 8].try_into().unwrap());
    let mut dir = vec_at(REC_FIXATION);
    if !(dir.z < 0.0 && dir.length() > 1e-6) {
        dir = vec_at(REC_GAZE) + vec_at(REC_GAZE + 12);
    }
    let dir = dir.try_normalize()?;
    Some((time, dir))
}

fn monotonic_now() -> f64 {
    let mut now = libc::timespec { tv_sec: 0, tv_nsec: 0 };
    unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut now) };
    now.tv_sec as f64 + now.tv_nsec as f64 * 1e-9
}

fn fit_axis(samples: &[&Sample], axis: usize) -> Axis {
    // gaze = gain * target + offset, with the gain pulled towards 1
    let (mut stt, mut st, mut sg, mut stg, n) = (GAIN_PRIOR, 0.0, 0.0, GAIN_PRIOR, samples.len() as f32);
    for s in samples {
        let (t, g) = (s.target[axis], s.gaze[axis]);
        stt += t * t;
        st += t;
        sg += g;
        stg += t * g;
    }
    let det = stt * n - st * st;
    if det.abs() < 1e-6 {
        return Axis { gain: 1.0, offset: (sg - st) / n.max(1.0) };
    }
    Axis {
        gain: (stg * n - st * sg) / det,
        offset: (stt * sg - st * stg) / det,
    }
}

fn fit(samples: &VecDeque<Sample>) -> Option<[Axis; 2]> {
    let all: Vec<&Sample> = samples.iter().collect();
    if all.len() < MIN_SAMPLES {
        return None;
    }
    let first = [fit_axis(&all, 0), fit_axis(&all, 1)];
    let kept: Vec<&Sample> = all
        .into_iter()
        .filter(|s| {
            (0..2).all(|a| (s.gaze[a] - first[a].gain * s.target[a] - first[a].offset).abs() < OUTLIER_DEG)
        })
        .collect();
    if kept.len() < MIN_SAMPLES {
        return Some(first);
    }
    Some([fit_axis(&kept, 0), fit_axis(&kept, 1)])
}

fn save(state: &State) {
    let (Some(path), Some(fit)) = (path(), state.fit) else {
        return;
    };
    let mut text = String::from(
        "# Gaze correction from pointer clicks, written by the frame shell: corrected = (measured - offset) / gain,\n\
         # degrees (yaw right, pitch up); delete the file to start over\n",
    );
    text += &format!(
        "yaw_gain {:.4}\nyaw_offset {:.3}\npitch_gain {:.4}\npitch_offset {:.3}\n",
        fit[0].gain, fit[0].offset, fit[1].gain, fit[1].offset
    );
    for s in &state.samples {
        text += &format!(
            "sample {:.3} {:.3} {:.3} {:.3}\n",
            s.target[0], s.target[1], s.gaze[0], s.gaze[1]
        );
    }
    let _ = std::fs::create_dir_all(path.parent().unwrap());
    let tmp = path.with_extension("tmp");
    if std::fs::write(&tmp, text).is_ok() {
        let _ = std::fs::rename(&tmp, &path);
    }
}

fn load() -> VecDeque<Sample> {
    let Some(text) = path().and_then(|path| std::fs::read_to_string(path).ok()) else {
        return VecDeque::new();
    };
    text.lines()
        .filter_map(|line| {
            let mut words = line.split_whitespace();
            (words.next()? == "sample").then_some(())?;
            let v: Vec<f32> = words.filter_map(|w| w.parse().ok()).collect();
            (v.len() == 4).then(|| Sample {
                target: [v[0], v[1]],
                gaze: [v[2], v[3]],
            })
        })
        .collect()
}

fn with_state(f: impl FnOnce(&mut State)) {
    let mut state = STATE.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let state = state.get_or_insert_with(|| {
        let samples = load();
        let fit = fit(&samples);
        State {
            base: None,
            gaze: VecDeque::new(),
            pending: Vec::new(),
            samples,
            fit,
        }
    });
    f(state);
}

/// A pointer click (button down) at a point in head space relative to the eyes' midpoint (metres, -z forward)
pub fn click(target: Vec3A) {
    let now = monotonic_now();
    with_state(|state| {
        if state.pending.len() < 8 {
            state.pending.push((now, target));
        }
    });
}

/// Follows the gaze and matches pending clicks with it; call once per frame
pub fn tick() {
    with_state(|state| {
        if state.base.is_none() {
            if state.pending.is_empty() {
                return;
            }
            // The eye tracking service only runs while the headset tracks the eyes
            state.base = map();
            if state.base.is_some() {
                log::info!("Calibrating the eye tracking from pointer clicks");
            }
        }
        let Some(base) = state.base else {
            state.pending.clear();
            return;
        };
        if let Some(sample) = read_gaze(base)
            && state.gaze.back().is_none_or(|last| sample.0 > last.0)
        {
            state.gaze.push_back(sample);
            while state.gaze.len() > 64 {
                state.gaze.pop_front();
            }
        }
        let Some(&(newest, _)) = state.gaze.back() else {
            return;
        };
        let mut added = false;
        let gaze = &state.gaze;
        let samples = &mut state.samples;
        state.pending.retain(|&(time, target)| {
            if newest < time {
                // Wait for the gaze at the click
                return monotonic_now() - time < 1.0;
            }
            let Some(i) = gaze.iter().position(|&(t, _)| t >= time) else {
                return false;
            };
            let (t1, g1) = gaze[i];
            let dir = match i.checked_sub(1).map(|j| gaze[j]) {
                Some((t0, g0)) if t1 - t0 < 0.2 => g0.lerp(g1, ((time - t0) / (t1 - t0)) as f32).normalize(),
                _ if t1 - time < 0.1 => g1,
                _ => return false,
            };
            let Some(target_dir) = target.try_normalize() else {
                return false;
            };
            if target_dir.dot(dir).clamp(-1.0, 1.0).acos().to_degrees() < MAX_ERROR_DEG {
                samples.push_back(Sample {
                    target: angles(target_dir),
                    gaze: angles(dir),
                });
                while samples.len() > MAX_SAMPLES {
                    samples.pop_front();
                }
                added = true;
            }
            false
        });
        if added {
            state.fit = fit(&state.samples);
            if let Some(fit) = state.fit {
                log::info!(
                    "Eye calibration from {} clicks: yaw x{:.2} {:+.1}°, pitch x{:.2} {:+.1}°",
                    state.samples.len(),
                    fit[0].gain,
                    fit[0].offset,
                    fit[1].gain,
                    fit[1].offset
                );
            }
            save(state);
        }
    });
}
