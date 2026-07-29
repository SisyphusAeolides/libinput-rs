//! Touchpad motion filtering shared by pointer, click-drag, and gesture paths.

use crate::ffi_types::{LibinputDevice, PointerMotionEvent};

pub fn speed_factor(speed: f64) -> f64 {
    (speed + 1.0).powf(2.38) * 0.95 + 0.05
}

const DEFAULT_DPI: f64 = 1_000.0;
const MM_PER_INCH: f64 = 25.4;
const MAGIC_SLOWDOWN: f64 = 0.2968;
const BASELINE: f64 = 0.9;

unsafe fn resolutions(device: *mut LibinputDevice) -> (f64, f64) {
    let x = (*device)
        .abs_x_resolution
        .filter(|resolution| *resolution > 0)
        .map(f64::from)
        .unwrap_or(DEFAULT_DPI / MM_PER_INCH);
    let y = (*device)
        .abs_y_resolution
        .filter(|resolution| *resolution > 0)
        .map(f64::from)
        .unwrap_or(x);
    (x, y)
}

/// Apply libinput's touchpad coordinate contract.
///
/// Both public channels are expressed in libinput's normalized coordinate
/// space. The adaptive profile's unaccelerated channel uses the constant
/// 0.9-baseline filter; flat profiles intentionally return the same value on
/// both channels, and custom profiles use their fallback curve.
pub unsafe fn configured_motion(
    device: *mut LibinputDevice,
    dx: f64,
    dy: f64,
    time_usec: u64,
    history: &mut crate::motion::MotionHistory,
    lenovo_x230: bool,
) -> PointerMotionEvent {
    let (x_resolution, y_resolution) = resolutions(device);
    let raw_x = dx;
    let raw_y = dy * x_resolution / y_resolution;

    if (*device).accel_profile == 4 {
        let (fallback_factor, motion_factor) =
            (*device)
                .accel_custom
                .as_mut()
                .map_or((1.0, 1.0), |config| {
                    let fallback = config
                        .curve_mut(0)
                        .map(|curve| curve.factor(raw_x, raw_y, time_usec))
                        .unwrap_or(1.0);
                    let motion = config
                        .curve_mut(1)
                        .map(|curve| curve.factor(raw_x, raw_y, time_usec))
                        .unwrap_or(fallback);
                    (fallback, motion)
                });
        return PointerMotionEvent {
            time_usec,
            dx: raw_x * motion_factor,
            dy: raw_y * motion_factor,
            dx_unaccel: raw_x * fallback_factor,
            dy_unaccel: raw_y * fallback_factor,
        };
    }

    let normalize = DEFAULT_DPI / (x_resolution * MM_PER_INCH);
    if (*device).accel_profile == 1 {
        let factor = MAGIC_SLOWDOWN * speed_factor((*device).accel_speed) * normalize;
        return PointerMotionEvent {
            time_usec,
            dx: raw_x * factor,
            dy: raw_y * factor,
            dx_unaccel: raw_x * factor,
            dy_unaccel: raw_y * factor,
        };
    }
    if lenovo_x230 {
        let normalized_x = raw_x * normalize;
        let normalized_y = raw_y * normalize;
        let velocity = history.feed_velocity(normalized_x, normalized_y, time_usec, false);
        let profile_factor = history.simpson_factor(velocity, |velocity| {
            lenovo_x230_profile(velocity, (*device).accel_speed)
        });
        return PointerMotionEvent {
            time_usec,
            dx: normalized_x * profile_factor,
            dy: normalized_y * profile_factor,
            dx_unaccel: normalized_x * 4.0,
            dy_unaccel: normalized_y * 4.0,
        };
    }
    let velocity = history.feed_velocity(raw_x, raw_y, time_usec, false);
    let profile_factor = history.simpson_factor(velocity, |velocity| {
        let speed_mm_s = velocity * 1_000_000.0 / x_resolution;
        let threshold = 130.0;
        let adaptive = if speed_mm_s < 7.0 {
            (0.1 * speed_mm_s + 0.3).min(BASELINE)
        } else if speed_mm_s < threshold {
            BASELINE
        } else {
            let speed = speed_mm_s.min(threshold * 4.0);
            0.0025 * (speed / threshold) * (speed - threshold) + BASELINE
        };
        adaptive * speed_factor((*device).accel_speed) * MAGIC_SLOWDOWN
    });
    let constant_factor = BASELINE * MAGIC_SLOWDOWN * normalize;

    PointerMotionEvent {
        time_usec,
        dx: raw_x * profile_factor * normalize,
        dy: raw_y * profile_factor * normalize,
        dx_unaccel: raw_x * constant_factor,
        dy_unaccel: raw_y * constant_factor,
    }
}

fn lenovo_x230_profile(velocity_units_per_usec: f64, speed: f64) -> f64 {
    let speed = speed.clamp(-1.0, 1.0);
    let threshold = (0.4 - 0.25 * speed).max(0.2) / 4.0;
    let maximum = (2.0 + speed * 1.5) * 4.0;
    let incline = (1.1 + speed * 0.75) * 4.0;
    let slowed_speed_ms = velocity_units_per_usec * 0.1 * 1_000.0;
    let f1 = (slowed_speed_ms * 5.0).min(1.0);
    let f2 = 1.0 + (slowed_speed_ms - threshold) * incline;
    maximum.min(if f2 > 1.0 { f2 } else { f1 }) * 0.1
}

/// Apply the same resolution normalization and constant touchpad filter used
/// by upstream for finger and edge scrolling. Wheel events use a different
/// path and remain unfiltered.
pub unsafe fn configured_scroll(
    device: *mut LibinputDevice,
    dx: f64,
    dy: f64,
    time_usec: u64,
) -> (f64, f64) {
    let (x_resolution, y_resolution) = resolutions(device);
    let dx_unaccel = dx;
    let dy_unaccel = dy * x_resolution / y_resolution;
    let normalize = DEFAULT_DPI / (x_resolution * MM_PER_INCH);
    let mut factor = BASELINE * MAGIC_SLOWDOWN * normalize;
    if (*device).accel_profile == 4 {
        factor *= (*device)
            .accel_custom
            .as_mut()
            .and_then(|config| config.curve_mut(2))
            .map(|curve| curve.factor(dx_unaccel, dy_unaccel, time_usec))
            .unwrap_or(1.0);
    }
    (dx_unaccel * factor, dy_unaccel * factor)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn upstream_speed_curve_preserves_default_and_nonzero_minimum() {
        assert!((speed_factor(0.0) - 1.0).abs() < 1e-12);
        assert!((speed_factor(-1.0) - 0.05).abs() < 1e-12);
        assert!((speed_factor(1.0) - 5.0).abs() < 0.01);
    }

    #[test]
    fn lenovo_x230_profile_retains_legacy_low_resolution_curve() {
        let slow = lenovo_x230_profile(0.000_1, 0.0);
        let fast = lenovo_x230_profile(0.01, 0.0);
        assert!(slow >= 0.0);
        assert!(fast > slow);
        assert!(fast <= 0.8);
    }
}
