/// Pure helper functions for analog axis normalization.
///
/// All Steam Deck analog axes use signed 16-bit values (−32767..+32767).
/// These functions normalize them to the −1.0…+1.0 / 0.0…1.0 range used
/// in state.json so the frontend needs no knowledge of hardware scaling.

/// Hardware axis full-scale value (signed 16-bit max).
pub const AXIS_SCALE: f32 = 32767.0;

/// Round a float to 3 decimal places.
///
/// Used before writing to state.json so that sub-threshold noise (e.g. the
/// 4th decimal place changing) does not cause spurious file writes.
#[inline]
pub fn r3(v: f32) -> f32 {
    (v * 1000.0).round() / 1000.0
}

/// Normalize a signed 16-bit hardware axis value to −1.0…+1.0, rounded to 3 dp.
#[inline]
pub fn normalize(raw: i32) -> f32 {
    r3(raw as f32 / AXIS_SCALE)
}

/// Normalize a signed 16-bit hardware axis value to −1.0…+1.0, with Y-axis
/// negated. Steam Deck hardware reports up as negative ABS_Y; this corrects
/// the convention so that up = positive in state.json.
#[inline]
pub fn normalize_y(raw: i32) -> f32 {
    r3(-raw as f32 / AXIS_SCALE)
}

/// Normalize a deadzone config value to the same −1.0…+1.0 space.
///
/// Internally makima stores deadzones as raw hardware units × 200 (see
/// `get_axis_value`). Dividing by `AXIS_SCALE` brings it into the same
/// normalized space as stick positions, so the frontend can use it directly
/// as a circle radius without any further scaling.
#[inline]
pub fn normalize_dz(raw: i32) -> f32 {
    r3((raw * 200) as f32 / AXIS_SCALE)
}

/// True when a trackpad has a finger on it.
///
/// `hid-steam` reports (0, 0) when no finger is present; any non-zero
/// position means a finger is touching the pad.
#[inline]
pub fn is_touching(x: i32, y: i32) -> bool {
    x != 0 || y != 0
}

/// True when the stick position exceeds the deadzone on either axis.
///
/// `x`, `y`, and `dz` must all be in the same normalized space (−1.0…+1.0).
#[inline]
pub fn is_active(x: f32, y: f32, dz: f32) -> bool {
    x.abs() > dz || y.abs() > dz
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // --- r3 ---

    #[test]
    fn r3_rounds_down() {
        assert_eq!(r3(0.1234), 0.123);
    }

    #[test]
    fn r3_rounds_up() {
        assert_eq!(r3(0.1235), 0.124);
    }

    #[test]
    fn r3_negative() {
        assert_eq!(r3(-0.5678), -0.568);
    }

    #[test]
    fn r3_zero() {
        assert_eq!(r3(0.0), 0.0);
    }

    #[test]
    fn r3_exact_boundary() {
        // 0.0005 rounds up to 0.001
        assert_eq!(r3(0.0005), 0.001);
        // 0.00049 rounds down to 0.0
        assert_eq!(r3(0.00049), 0.0);
    }

    // --- normalize ---

    #[test]
    fn normalize_max_positive() {
        assert_eq!(normalize(32767), 1.0);
    }

    #[test]
    fn normalize_max_negative() {
        // −32767 / 32767 = −1.0 exactly
        assert_eq!(normalize(-32767), -1.0);
    }

    #[test]
    fn normalize_zero() {
        assert_eq!(normalize(0), 0.0);
    }

    #[test]
    fn normalize_midrange() {
        // 16384 / 32767 ≈ 0.5 — just check it's in range and rounded
        let v = normalize(16384);
        assert!(v > 0.499 && v < 0.501);
    }

    // --- normalize_y ---

    #[test]
    fn normalize_y_negates_positive_raw() {
        // Hardware up = negative ABS_Y; pushing up gives raw = −32767 → +1.0
        assert_eq!(normalize_y(-32767), 1.0);
    }

    #[test]
    fn normalize_y_negates_negative_raw() {
        // Pushing down gives raw = +32767 → −1.0
        assert_eq!(normalize_y(32767), -1.0);
    }

    #[test]
    fn normalize_y_zero_unchanged() {
        assert_eq!(normalize_y(0), 0.0);
    }

    // --- normalize_dz ---

    #[test]
    fn normalize_dz_typical() {
        // deadzone=15 → 15*200/32767 ≈ 0.0916 → rounds to 0.092
        assert_eq!(normalize_dz(15), 0.092);
    }

    #[test]
    fn normalize_dz_zero() {
        assert_eq!(normalize_dz(0), 0.0);
    }

    #[test]
    fn normalize_dz_same_scale_as_normalize() {
        // A position exactly at the hardware deadzone threshold should equal
        // the normalized deadzone value so `is_active` comparisons are consistent.
        let dz_raw = 20;
        let pos_at_threshold = dz_raw * 200; // hardware units at threshold
        let normalized_pos = normalize(pos_at_threshold);
        let normalized_dz = normalize_dz(dz_raw);
        // pos is exactly at threshold → not active (strict >)
        assert!(!is_active(normalized_pos, 0.0, normalized_dz));
    }

    // --- is_touching ---

    #[test]
    fn is_touching_both_zero_is_false() {
        assert!(!is_touching(0, 0));
    }

    #[test]
    fn is_touching_nonzero_x() {
        assert!(is_touching(100, 0));
    }

    #[test]
    fn is_touching_nonzero_y() {
        assert!(is_touching(0, -500));
    }

    #[test]
    fn is_touching_both_nonzero() {
        assert!(is_touching(1234, -5678));
    }

    // --- is_active ---

    #[test]
    fn is_active_inside_deadzone_false() {
        assert!(!is_active(0.05, 0.03, 0.1));
    }

    #[test]
    fn is_active_x_exceeds_deadzone() {
        assert!(is_active(0.15, 0.0, 0.1));
    }

    #[test]
    fn is_active_y_exceeds_deadzone() {
        assert!(is_active(0.0, -0.15, 0.1));
    }

    #[test]
    fn is_active_exactly_at_deadzone_is_false() {
        // Strict > means sitting exactly on the boundary is not active
        assert!(!is_active(0.1, 0.0, 0.1));
    }

    #[test]
    fn is_active_zero_deadzone_nonzero_pos() {
        assert!(is_active(0.001, 0.0, 0.0));
    }

    #[test]
    fn is_active_negative_x_exceeds_deadzone() {
        assert!(is_active(-0.15, 0.0, 0.1));
    }
}
