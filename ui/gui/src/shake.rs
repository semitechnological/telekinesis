//! Cursor shake detection via CoreGraphics mouse polling.
//!
//! Samples mouse position at ~60fps, tracks direction reversals, and fires
//! a callback when the user shakes their cursor rapidly back and forth.
//! No accessibility permissions needed — uses CGEventCreate(nil) to read
//! the current mouse location.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

#[cfg(target_os = "macos")]
#[repr(C)]
struct CGPointF64 {
    x: f64,
    y: f64,
}

#[cfg(target_os = "macos")]
#[link(name = "CoreGraphics", kind = "framework")]
extern "C" {
    fn CGEventCreate(source: *mut std::ffi::c_void) -> *mut std::ffi::c_void;
    fn CGEventGetLocation(event: *mut std::ffi::c_void) -> CGPointF64;
    fn CFRelease(cf: *mut std::ffi::c_void);
}

#[cfg(target_os = "macos")]
fn current_mouse_position() -> (f64, f64) {
    unsafe {
        let event = CGEventCreate(std::ptr::null_mut());
        if event.is_null() {
            return (0.0, 0.0);
        }
        let loc = CGEventGetLocation(event);
        CFRelease(event);
        (loc.x, loc.y)
    }
}

#[cfg(not(target_os = "macos"))]
fn current_mouse_position() -> (f64, f64) {
    (0.0, 0.0)
}

/// Configuration for shake detection.
struct ShakeConfig {
    /// Minimum velocity (px/s) for a movement to count as "fast"
    min_velocity: f64,
    /// Time window for counting reversals (ms)
    reversal_window_ms: u64,
    /// Number of direction reversals needed to trigger shake
    min_reversals: usize,
    /// Cooldown between shake triggers (ms)
    cooldown_ms: u64,
    /// Polling interval (ms)
    poll_interval_ms: u64,
}

impl Default for ShakeConfig {
    fn default() -> Self {
        Self {
            min_velocity: 300.0,
            reversal_window_ms: 600,
            min_reversals: 2,
            cooldown_ms: 600,
            poll_interval_ms: 16,
        }
    }
}

/// Ring buffer of recent mouse samples for shake detection.
struct SampleBuffer {
    samples: Vec<(f64, f64, Instant)>,
    capacity: usize,
}

impl SampleBuffer {
    fn new(capacity: usize) -> Self {
        Self {
            samples: Vec::with_capacity(capacity),
            capacity,
        }
    }

    fn push(&mut self, x: f64, y: f64, time: Instant) {
        if self.samples.len() >= self.capacity {
            self.samples.remove(0);
        }
        self.samples.push((x, y, time));
    }

    /// Detect shake: total distance traveled + direction reversals in a time window.
    /// Simpler approach: if the cursor traveled a lot AND changed direction at least
    /// once, it's a shake.
    fn detect_shake(&self, config: &ShakeConfig) -> bool {
        if self.samples.len() < 3 {
            return false;
        }

        let now = self.samples.last().unwrap().2;
        let window_start = now - Duration::from_millis(config.reversal_window_ms);

        let windowed: Vec<&(f64, f64, Instant)> = self
            .samples
            .iter()
            .filter(|s| s.2 >= window_start)
            .collect();

        if windowed.len() < 3 {
            return false;
        }

        // Total distance traveled (sum of segment lengths)
        let mut total_distance = 0.0f64;
        let (mut prev_x, mut prev_y) = (windowed[0].0, windowed[0].1);
        for s in windowed.iter().skip(1) {
            let dx = s.0 - prev_x;
            let dy = s.1 - prev_y;
            total_distance += (dx * dx + dy * dy).sqrt();
            prev_x = s.0;
            prev_y = s.1;
        }

        // Net displacement (start to end)
        let net_dx = windowed.last().unwrap().0 - windowed[0].0;
        let net_dy = windowed.last().unwrap().1 - windowed[0].1;
        let net_distance = (net_dx * net_dx + net_dy * net_dy).sqrt();

        // Shake = high total distance but low net displacement (back-and-forth)
        // Ratio > 3 means the cursor went back and forth a lot
        let ratio = if net_distance > 1.0 {
            total_distance / net_distance
        } else {
            total_distance
        };

        // Also count direction reversals
        let mut reversals = 0;
        let mut prev_sx: i32 = 0;
        let mut prev_sy: i32 = 0;
        for i in 1..windowed.len() {
            let dx = windowed[i].0 - windowed[i - 1].0;
            let dy = windowed[i].1 - windowed[i - 1].1;
            let sx: i32 = if dx > 2.0 { 1 } else if dx < -2.0 { -1 } else { 0 };
            let sy: i32 = if dy > 2.0 { 1 } else if dy < -2.0 { -1 } else { 0 };
            if sx != 0 && prev_sx != 0 && sx != prev_sx { reversals += 1; }
            if sy != 0 && prev_sy != 0 && sy != prev_sy { reversals += 1; }
            if sx != 0 { prev_sx = sx; }
            if sy != 0 { prev_sy = sy; }
        }

        let is_shake = total_distance > 200.0 && (ratio > 2.5 || reversals >= 2);

        if is_shake {
            eprintln!("[shake] detect: total_dist={total_distance:.0}, net_dist={net_distance:.0}, ratio={ratio:.1}, reversals={reversals}, samples={}", windowed.len());
        }

        is_shake
    }
}

/// Starts a background thread that polls mouse position and calls `on_shake`
/// with the cursor position when the cursor is shaken.
/// Returns a handle that can stop the detector.
pub struct ShakeDetector {
    running: Arc<AtomicBool>,
}

impl ShakeDetector {
    pub fn start<F>(on_shake: F) -> Self
    where
        F: Fn(f64, f64) + Send + 'static,
    {
        let running = Arc::new(AtomicBool::new(true));
        let running_clone = running.clone();

        std::thread::spawn(move || {
            let config = ShakeConfig::default();
            let mut buffer = SampleBuffer::new(60);
            let mut last_triggered = Instant::now() - Duration::from_secs(10);
            let mut sample_count = 0u32;

            while running_clone.load(Ordering::Relaxed) {
                std::thread::sleep(Duration::from_millis(config.poll_interval_ms));

                let (x, y) = current_mouse_position();
                let now = Instant::now();
                buffer.push(x, y, now);
                sample_count += 1;

                // Log every ~2 seconds that we're alive
                if sample_count % 120 == 0 {
                    eprintln!("[shake] alive, pos=({x:.0},{y:.0}), samples={sample_count}");
                }

                if buffer.detect_shake(&config) {
                    let elapsed = now.duration_since(last_triggered);
                    eprintln!("[shake] detected! elapsed={:.0}ms, cooldown={}ms", elapsed.as_millis(), config.cooldown_ms);
                    if elapsed.as_millis() as u64 > config.cooldown_ms {
                        last_triggered = now;
                        eprintln!("[shake] firing callback at ({x:.0},{y:.0})");
                        on_shake(x, y);
                    } else {
                        eprintln!("[shake] on cooldown, skipping");
                    }
                }
            }
        });

        Self { running }
    }

    pub fn stop(&self) {
        self.running.store(false, Ordering::Relaxed);
    }
}

impl Drop for ShakeDetector {
    fn drop(&mut self) {
        self.stop();
    }
}
