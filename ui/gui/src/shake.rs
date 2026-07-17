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
            min_velocity: 600.0,
            reversal_window_ms: 400,
            min_reversals: 3,
            cooldown_ms: 1000,
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

    /// Detect shake: count direction reversals in the recent window.
    /// A reversal is when the sign of velocity changes (left→right or right→left).
    fn detect_shake(&self, config: &ShakeConfig) -> bool {
        if self.samples.len() < 4 {
            return false;
        }

        let now = self.samples.last().unwrap().2;
        let window_start = now - Duration::from_millis(config.reversal_window_ms);

        // Get samples within the time window
        let windowed: Vec<&(f64, f64, Instant)> = self
            .samples
            .iter()
            .filter(|s| s.2 >= window_start)
            .collect();

        if windowed.len() < 4 {
            return false;
        }

        // Calculate velocities between consecutive samples
        let mut velocities: Vec<(f64, f64)> = Vec::new();
        for i in 1..windowed.len() {
            let (px, py, pt) = *windowed[i - 1];
            let (cx, cy, ct) = *windowed[i];
            let dt = ct.duration_since(pt).as_secs_f64();
            if dt > 0.0 {
                let vx = (cx - px) / dt;
                let vy = (cy - py) / dt;
                velocities.push((vx, vy));
            }
        }

        if velocities.is_empty() {
            return false;
        }

        // Check that movements are fast enough
        let avg_speed: f64 = velocities
            .iter()
            .map(|(vx, vy)| (vx * vx + vy * vy).sqrt())
            .sum::<f64>()
            / velocities.len() as f64;

        if avg_speed < config.min_velocity {
            return false;
        }

        // Count direction reversals (sign changes in dominant axis)
        let mut reversals = 0;
        let mut prev_sign_x: i32 = 0;
        let mut prev_sign_y: i32 = 0;

        for (vx, vy) in &velocities {
            let sign_x = if vx.abs() > vy.abs() {
                if *vx > 0.0 { 1 } else if *vx < 0.0 { -1 } else { 0 }
            } else {
                0
            };
            let sign_y = if vy.abs() >= vx.abs() {
                if *vy > 0.0 { 1 } else if *vy < 0.0 { -1 } else { 0 }
            } else {
                0
            };

            if sign_x != 0 && prev_sign_x != 0 && sign_x != prev_sign_x {
                reversals += 1;
            }
            if sign_y != 0 && prev_sign_y != 0 && sign_y != prev_sign_y {
                reversals += 1;
            }

            if sign_x != 0 {
                prev_sign_x = sign_x;
            }
            if sign_y != 0 {
                prev_sign_y = sign_y;
            }
        }

        reversals >= config.min_reversals
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
            let mut buffer = SampleBuffer::new(30);
            let mut last_triggered = Instant::now() - Duration::from_secs(10);

            while running_clone.load(Ordering::Relaxed) {
                std::thread::sleep(Duration::from_millis(config.poll_interval_ms));

                let (x, y) = current_mouse_position();
                let now = Instant::now();
                buffer.push(x, y, now);

                if buffer.detect_shake(&config) {
                    let elapsed = now.duration_since(last_triggered);
                    if elapsed.as_millis() as u64 > config.cooldown_ms {
                        last_triggered = now;
                        on_shake(x, y);
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
