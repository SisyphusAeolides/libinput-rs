//! Stateful velocity tracking shared by mouse, TrackPoint, and touchpad filters.

const MOTION_TIMEOUT_USEC: u64 = 1_000_000;
const MAX_VELOCITY_DIFF: f64 = 0.001;
const UNDEFINED_DIRECTION: u8 = 0xff;

#[derive(Clone, Copy, Default)]
struct Tracker {
    dx: f64,
    dy: f64,
    time_usec: u64,
    direction: u8,
}

pub struct MotionHistory {
    trackers: [Tracker; 16],
    current: usize,
    pub last_velocity: f64,
}

impl Default for MotionHistory {
    fn default() -> Self {
        Self {
            trackers: [Tracker::default(); 16],
            current: 0,
            last_velocity: 0.0,
        }
    }
}

impl MotionHistory {
    pub fn restart(&mut self) {
        *self = Self::default();
    }

    /// Restart at a known device timestamp.
    ///
    /// Upstream seeds the current tracker when a contact begins. Without
    /// that seed, the first motion frame is compared with timestamp zero and
    /// is incorrectly treated as motion after the one-second timeout,
    /// heavily decelerating the first gesture update.
    pub fn restart_at(&mut self, time_usec: u64) {
        self.restart();
        self.trackers[self.current] = Tracker {
            time_usec,
            direction: UNDEFINED_DIRECTION,
            ..Tracker::default()
        };
    }

    fn by_offset(&self, offset: usize) -> &Tracker {
        &self.trackers[(self.current + self.trackers.len() - offset) % self.trackers.len()]
    }

    pub fn feed_velocity(&mut self, dx: f64, dy: f64, time_usec: u64, smooth_10ms: bool) -> f64 {
        for tracker in &mut self.trackers {
            tracker.dx += dx;
            tracker.dy += dy;
        }
        self.current = (self.current + 1) % self.trackers.len();
        self.trackers[self.current] = Tracker {
            dx: 0.0,
            dy: 0.0,
            time_usec,
            direction: direction(dx, dy),
        };

        let mut result = 0.0;
        let mut initial_velocity = 0.0;
        let mut common_direction = self.by_offset(0).direction;
        for offset in 1..self.trackers.len() {
            let tracker = self.by_offset(offset);
            if tracker.time_usec > time_usec {
                break;
            }
            let elapsed = time_usec - tracker.time_usec;
            if elapsed > MOTION_TIMEOUT_USEC {
                if offset == 1 {
                    result = tracker_velocity(tracker, MOTION_TIMEOUT_USEC, smooth_10ms);
                }
                break;
            }
            let velocity = tracker_velocity(tracker, elapsed, smooth_10ms);
            common_direction &= tracker.direction;
            if common_direction == 0 {
                if offset == 1 {
                    result = velocity;
                }
                break;
            }
            if initial_velocity == 0.0 || offset <= 2 {
                initial_velocity = velocity;
                result = velocity;
            } else if (initial_velocity - velocity).abs() > MAX_VELOCITY_DIFF {
                break;
            } else {
                result = velocity;
            }
        }
        result
    }

    pub fn simpson_factor(&mut self, velocity: f64, mut profile: impl FnMut(f64) -> f64) -> f64 {
        let previous = self.last_velocity;
        self.last_velocity = velocity;
        (profile(velocity) + profile(previous) + 4.0 * profile((velocity + previous) / 2.0)) / 6.0
    }
}

fn tracker_velocity(tracker: &Tracker, elapsed_usec: u64, smooth_10ms: bool) -> f64 {
    let mut elapsed = elapsed_usec.saturating_add(1);
    if smooth_10ms && elapsed < 10_000 {
        elapsed = 10_000;
    }
    tracker.dx.hypot(tracker.dy) / elapsed as f64
}

fn direction(x: f64, y: f64) -> u8 {
    if x.abs() < 2.0 && y.abs() < 2.0 {
        return match (x.total_cmp(&0.0), y.total_cmp(&0.0)) {
            (std::cmp::Ordering::Greater, std::cmp::Ordering::Greater) => 0x1c,
            (std::cmp::Ordering::Greater, std::cmp::Ordering::Less) => 0x07,
            (std::cmp::Ordering::Less, std::cmp::Ordering::Greater) => 0x70,
            (std::cmp::Ordering::Less, std::cmp::Ordering::Less) => 0xc1,
            (std::cmp::Ordering::Greater, _) => 0x0e,
            (std::cmp::Ordering::Less, _) => 0xe0,
            (_, std::cmp::Ordering::Greater) => 0x38,
            (_, std::cmp::Ordering::Less) => 0x83,
            _ => UNDEFINED_DIRECTION,
        };
    }
    let r = (y.atan2(x) + 2.5 * std::f64::consts::PI).rem_euclid(2.0 * std::f64::consts::PI) * 4.0
        / std::f64::consts::PI;
    let first = ((r + 0.9) as usize % 8) as u8;
    let second = ((r + 0.1) as usize % 8) as u8;
    (1 << first) | (1 << second)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn steady_motion_uses_multi_frame_velocity() {
        let mut history = MotionHistory::default();
        let first = history.feed_velocity(1.0, 0.0, 10_000, false);
        let second = history.feed_velocity(1.0, 0.0, 20_000, false);
        assert!(first > 0.0);
        assert!((first - 0.000_1).abs() < 0.000_001);
        assert!((second - 0.000_1).abs() < 0.000_001);
    }

    #[test]
    fn simpson_integration_averages_profile_transition() {
        let mut history = MotionHistory {
            last_velocity: 1.0,
            ..MotionHistory::default()
        };
        assert_eq!(history.simpson_factor(3.0, |velocity| velocity), 2.0);
    }

    #[test]
    fn timestamped_restart_preserves_first_frame_velocity() {
        let mut history = MotionHistory::default();
        history.restart_at(10_000);
        let velocity = history.feed_velocity(10.0, 0.0, 20_000, false);
        assert!((velocity - 0.001).abs() < 0.000_001);
    }
}
