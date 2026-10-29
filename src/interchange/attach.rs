//! Where a point sits on a part: the inverse of an `Along` or `Surface` attach.

use bevy::prelude::*;

use crate::anatomy::{AxisSample, sample_axis};

/// `1 / φ`, for the golden-section search.
const GOLDEN: f32 = 0.618_034;

/// Where `p` is along and around a part, as a `Surface` attach measures it.
///
/// `axis` is the part's axis samples (logical model space). The result is the `t` of the axis
/// point nearest to `p`, and the degrees around the axis that `p` lies at (`0` = dorsal, `90` =
/// left side, `180` = ventral). `None` for a part with no axis.
#[must_use]
pub fn axis_coords(axis: &[AxisSample], p: Vec3) -> Option<(f32, f32)> {
    // The best of 64 samples, then narrowed down between its neighbours (golden section), so a
    // part glides rather than steps.
    let d = |t: f32| sample_axis(axis, t).map_or(f32::INFINITY, |s| s.centre().distance_squared(p));
    let coarse = (0..=64u8).map(|i| f32::from(i) / 64.0).min_by(|a, b| d(*a).total_cmp(&d(*b)))?;
    let (mut lo, mut hi) = ((coarse - 1.0 / 64.0).max(0.0), (coarse + 1.0 / 64.0).min(1.0));
    for _ in 0..24 {
        let (a, b) = ((hi - lo).mul_add(-GOLDEN, hi), (hi - lo).mul_add(GOLDEN, lo));
        if d(a) < d(b) {
            hi = b;
        } else {
            lo = a;
        }
    }
    let t = f32::midpoint(lo, hi);
    let s = sample_axis(axis, t)?;
    let out = p - s.centre();
    let dorsal = s.model.rotation * Vec3::NEG_Z;
    let side = s.model.rotation * Vec3::X;
    Some((t, out.dot(side).atan2(out.dot(dorsal)).to_degrees()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::anatomy::BoneId;

    /// A straight axis up +Y from 0 to 1 m, dorsal toward -Z.
    fn axis() -> Vec<AxisSample> {
        (0..=4u8)
            .map(|i| {
                let t = f32::from(i) / 4.0;
                AxisSample::round(t, BoneId(0), Transform::from_xyz(0.0, t, 0.0), 0.1)
            })
            .collect()
    }

    #[test]
    fn finds_t_and_around() {
        let (t, around) = axis_coords(&axis(), Vec3::new(0.0, 0.3, -0.1)).unwrap();
        assert!((t - 0.3).abs() < 1e-3, "{t}");
        assert!(around.abs() < 1e-2, "{around}");
        let (t, around) = axis_coords(&axis(), Vec3::new(0.1, 0.75, 0.0)).unwrap();
        assert!((t - 0.75).abs() < 1e-3, "{t}");
        assert!((around - 90.0).abs() < 1e-2, "{around}");
    }
}
