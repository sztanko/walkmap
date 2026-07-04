/// Walking-time edge weights: Tobler's hiking function, directional.

pub fn haversine_m(a: [f64; 2], b: [f64; 2]) -> f64 {
    const R: f64 = 6_371_000.0;
    let (l1, l2) = (a[1].to_radians(), b[1].to_radians());
    let dlat = l2 - l1;
    let dlng = (b[0] - a[0]).to_radians();
    let h = (dlat / 2.0).sin().powi(2) + l1.cos() * l2.cos() * (dlng / 2.0).sin().powi(2);
    2.0 * R * h.sqrt().asin()
}

/// Tobler's hiking function: speed in m/s for a given slope (dh/d).
/// ≈1.40 m/s (5.04 km/h) on the flat, fastest on a gentle −5% descent.
pub fn tobler_ms(slope: f64) -> f64 {
    6.0 * (-3.5 * (slope + 0.05).abs()).exp() / 3.6
}

/// Directed walking times in deciseconds for one segment of horizontal length
/// `d` meters with elevation change `dh = elev(b) − elev(a)`.
/// Returns (t_a_to_b, t_b_to_a).
pub fn segment_times_ds(d: f64, dh: f64, steps: bool, flat: bool) -> (u32, u32) {
    if d <= 0.0 {
        return (1, 1);
    }
    // below-DEM-resolution segments and bridges/tunnels are treated as level
    let dh = if flat || d < 3.0 { 0.0 } else { dh };
    let t = |dh: f64| -> u32 {
        let v = if steps {
            if dh > 0.0 {
                0.45 // m/s climbing stairs
            } else {
                0.6 // m/s descending
            }
        } else {
            tobler_ms((dh / d).clamp(-0.35, 0.35))
        };
        ((10.0 * d / v).round() as u32).max(1)
    };
    (t(dh), t(-dh))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tobler_flat_speed() {
        // 6·exp(−0.175)/3.6 ≈ 1.399 m/s ≈ 5.04 km/h
        assert!((tobler_ms(0.0) - 1.399).abs() < 0.01);
    }

    #[test]
    fn tobler_downhill_faster_than_flat() {
        assert!(tobler_ms(-0.05) > tobler_ms(0.0));
        assert!(tobler_ms(-0.30) < tobler_ms(0.0)); // steep descent is slow again
    }

    #[test]
    fn directed_asymmetry() {
        let (up, down) = segment_times_ds(100.0, 10.0, false, false);
        assert!(up > down);
        // flat: symmetric
        let (a, b) = segment_times_ds(100.0, 10.0, false, true);
        assert_eq!(a, b);
        // ~100m at 1.4 m/s ≈ 71s ≈ 715 ds
        assert!((a as i64 - 715).abs() < 20);
    }

    #[test]
    fn steps_times() {
        let (up, down) = segment_times_ds(10.0, 5.0, true, false);
        assert_eq!(up, (10.0_f64 * 10.0 / 0.45).round() as u32);
        assert_eq!(down, (10.0_f64 * 10.0 / 0.6).round() as u32);
    }
}
