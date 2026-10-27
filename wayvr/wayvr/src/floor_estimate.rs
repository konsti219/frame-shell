use std::{
    sync::{Arc, LazyLock, Mutex},
    time::{Duration, Instant},
};

use glam::Vec3;

#[derive(Default)]
pub struct FloorEstimate {
    pub timestamp: u64,
    pub height: Option<f32>,
    pub support: usize,
    pub points: Vec<(Vec3, bool)>,
}

static CACHE: LazyLock<Arc<Mutex<Arc<FloorEstimate>>>> = LazyLock::new(|| {
    let cache = Arc::new(Mutex::new(Arc::default()));
    let worker = cache.clone();
    std::thread::spawn(move || {
        loop {
            let estimate = Arc::new(read().unwrap_or_default());
            *worker.lock().unwrap() = estimate;
            std::thread::sleep(Duration::from_millis(200));
        }
    });
    cache
});

pub fn snapshot() -> Arc<FloorEstimate> {
    CACHE.lock().unwrap().clone()
}

fn monotonic_ns() -> Option<u64> {
    let mut now = std::mem::MaybeUninit::<libc::timespec>::uninit();
    if unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, now.as_mut_ptr()) } != 0 {
        return None;
    }
    let now = unsafe { now.assume_init() };
    Some(now.tv_sec as u64 * 1_000_000_000 + now.tv_nsec as u64)
}

static LAST_REQUEST: Mutex<Option<Instant>> = Mutex::new(None);

/// Keeps Monado's cameras (and so the room mesh) running while estimates are needed, even without passthrough:
/// a CLOCK_MONOTONIC deadline in $XDG_RUNTIME_DIR/monado-floor-wanted, refreshed while `wanted`
pub fn request(wanted: bool) {
    if !wanted {
        return;
    }
    let mut last = LAST_REQUEST.lock().unwrap();
    if last.is_some_and(|time| time.elapsed() < Duration::from_millis(500)) {
        return;
    }
    *last = Some(Instant::now());
    let (Some(dir), Some(now)) = (std::env::var_os("XDG_RUNTIME_DIR"), monotonic_ns()) else {
        return;
    };
    let path = std::path::PathBuf::from(dir).join("monado-floor-wanted");
    let tmp = path.with_extension("tmp");
    if std::fs::write(&tmp, format!("{}\n", now + 2_000_000_000)).is_ok() {
        let _ = std::fs::rename(&tmp, &path);
    }
}

fn read() -> Option<FloorEstimate> {
    let path =
        std::path::PathBuf::from(std::env::var_os("XDG_RUNTIME_DIR")?).join("monado-floor");
    let text = std::fs::read_to_string(path).ok()?;
    let mut lines = text.lines();
    let mut header = lines.next()?.split_whitespace();
    if header.next()? != "1" {
        return None;
    }
    let timestamp = header.next()?.parse::<u64>().ok()?;
    let height = header.next()?.parse::<f32>().ok()?;
    let support = header.next()?.parse::<usize>().ok()?;
    let now_ns = monotonic_ns()?;
    if timestamp > now_ns || now_ns - timestamp > 2_000_000_000 {
        return None;
    }
    let mut estimate = FloorEstimate {
        timestamp,
        height: height.is_finite().then_some(height),
        support,
        points: Vec::new(),
    };
    for line in lines.take(256) {
        let values: Vec<f32> = line
            .split_whitespace()
            .filter_map(|s| s.parse().ok())
            .collect();
        if values.len() == 4 && values.iter().all(|v| v.is_finite()) {
            estimate
                .points
                .push((Vec3::new(values[0], values[1], values[2]), values[3] > 0.5));
        }
    }
    Some(estimate)
}

pub struct FloorCalibration {
    deadline: Option<Instant>,
    weighted_height: f64,
    weight: f64,
    timestamp: u64,
    samples: u32,
    pub height: Option<f32>,
    pub needs_save: bool,
}

impl FloorCalibration {
    pub fn new(on_start: bool) -> Self {
        let mut calibration = Self {
            deadline: None,
            weighted_height: 0.0,
            weight: 0.0,
            timestamp: 0,
            samples: 0,
            height: None,
            needs_save: false,
        };
        if on_start {
            calibration.start(30);
        }
        calibration
    }

    pub fn start(&mut self, seconds: u64) {
        self.deadline = Some(Instant::now() + Duration::from_secs(seconds));
        log::info!("Collecting floor calibration for {seconds} seconds");
        self.samples = 0;
        self.weighted_height = 0.0;
        self.weight = 0.0;
        self.timestamp = snapshot().timestamp;
        self.needs_save = false;
    }

    pub fn stop(&mut self) {
        self.deadline = None;
        self.height = None;
        self.needs_save = false;
    }

    pub fn remaining_seconds(&self) -> Option<u32> {
        self.deadline.map(|deadline| {
            deadline
                .saturating_duration_since(Instant::now())
                .as_secs_f32()
                .ceil() as u32
        })
    }

    pub fn update(&mut self, estimate: &FloorEstimate) {
        let Some(deadline) = self.deadline else {
            return;
        };
        if Instant::now() >= deadline {
            self.deadline = None;
            crate::frame_shell::show_floor();
            self.needs_save = self.weight > 0.0;
            log::info!(
                "Floor calibration finished: height {:?} m, {} estimates, saved={}",
                self.height,
                self.samples,
                self.needs_save
            );
            return;
        }
        if estimate.timestamp == self.timestamp {
            return;
        }
        self.timestamp = estimate.timestamp;
        if let Some(height) = estimate.height.filter(|_| estimate.support >= 30) {
            self.samples += 1;
            self.weighted_height += f64::from(height) * estimate.support as f64;
            self.weight += estimate.support as f64;
            self.height = Some((self.weighted_height / self.weight) as f32);
        }
    }
}
