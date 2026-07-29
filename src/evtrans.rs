//! Evdev-to-libinput translation policy shared by every device role.
//!
//! Byte framing and SYN_DROPPED recovery remain in libevdev. This module owns
//! the cross-role rules that must stay identical for keyboard, pointer,
//! touchpad, tablet, timeout, and removal paths.

pub const KEY_MAX: usize = 0x2ff;
pub type SeatCodeCounts = [u32; KEY_MAX + 1];

pub const fn empty_seat_code_counts() -> SeatCodeCounts {
    [0; KEY_MAX + 1]
}

pub fn is_button_code(code: u16) -> bool {
    matches!(code, 0x100..=0x15f | 0x2c0..=0x2ff)
}

pub fn update_seat_count(counts: &mut SeatCodeCounts, code: u16, pressed: bool) -> u32 {
    let Some(count) = counts.get_mut(usize::from(code)) else {
        return 0;
    };
    if pressed {
        *count = count.saturating_add(1);
    } else {
        *count = count.saturating_sub(1);
    }
    *count
}

pub fn transition_button(held: &mut Vec<u16>, code: u16, pressed: bool) -> bool {
    if pressed {
        if held.contains(&code) {
            return false;
        }
        held.push(code);
    } else {
        if !held.contains(&code) {
            return false;
        }
        held.retain(|button| *button != code);
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mixed_nodes_keep_keyboard_and_button_ranges_separate() {
        assert!(!is_button_code(30));
        assert!(!is_button_code(42));
        assert!(is_button_code(0x100));
        assert!(is_button_code(0x110));
        assert!(is_button_code(0x2c0));
    }

    #[test]
    fn counts_are_independent_and_unmatched_releases_are_safe() {
        let mut counts = empty_seat_code_counts();
        assert_eq!(update_seat_count(&mut counts, 30, true), 1);
        assert_eq!(update_seat_count(&mut counts, 42, true), 1);
        assert_eq!(update_seat_count(&mut counts, 30, true), 2);
        assert_eq!(update_seat_count(&mut counts, 42, false), 0);
        assert_eq!(update_seat_count(&mut counts, 30, false), 1);
        assert_eq!(update_seat_count(&mut counts, 30, false), 0);
        assert_eq!(update_seat_count(&mut counts, 30, false), 0);
    }

    fn press_chord(counts: &mut SeatCodeCounts, codes: &[u16]) {
        for code in codes {
            assert_eq!(update_seat_count(counts, *code, true), 1);
        }
        for code in codes.iter().rev() {
            assert_eq!(update_seat_count(counts, *code, false), 0);
        }
    }

    #[test]
    fn keyboard_chords_keep_independent_per_code_state() {
        let mut counts = empty_seat_code_counts();
        press_chord(&mut counts, &[42, 2]); // Shift+1
        press_chord(&mut counts, &[42, 30]); // Shift+A
        press_chord(&mut counts, &[29, 56]); // Ctrl+Alt
        press_chord(&mut counts, &[30, 48]); // A+B
    }

    #[test]
    fn duplicate_keyboards_count_the_same_key_per_device() {
        let mut counts = empty_seat_code_counts();
        assert_eq!(update_seat_count(&mut counts, 30, true), 1);
        assert_eq!(update_seat_count(&mut counts, 30, true), 2);
        assert_eq!(update_seat_count(&mut counts, 30, false), 1);
        assert_eq!(update_seat_count(&mut counts, 30, false), 0);
    }

    #[test]
    fn simultaneous_mouse_buttons_are_independent() {
        let mut counts = empty_seat_code_counts();
        for button in [0x110, 0x111, 0x112] {
            assert_eq!(update_seat_count(&mut counts, button, true), 1);
        }
        assert_eq!(counts[0x110], 1);
        assert_eq!(counts[0x111], 1);
        assert_eq!(counts[0x112], 1);
        for button in [0x111, 0x110, 0x112] {
            assert_eq!(update_seat_count(&mut counts, button, false), 0);
        }
    }

    #[test]
    fn out_of_range_codes_cannot_corrupt_seat_state() {
        let mut counts = empty_seat_code_counts();
        assert_eq!(update_seat_count(&mut counts, u16::MAX, true), 0);
        assert!(counts.iter().all(|count| *count == 0));
    }

    #[test]
    fn button_lifecycle_is_exactly_once() {
        let mut held = Vec::new();
        assert!(!transition_button(&mut held, 0x110, false));
        assert!(transition_button(&mut held, 0x110, true));
        assert!(!transition_button(&mut held, 0x110, true));
        assert!(transition_button(&mut held, 0x110, false));
        assert!(held.is_empty());
    }
}
