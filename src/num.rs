//! Numeric conversions that `From`/`TryFrom` cannot express. These hold the crate's only `as`
//! casts; each states what it gives up.
#![allow(
    clippy::as_conversions,
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss
)]

// Mesh indices are `u32` and address `Vec`s, so they must fit in `usize`.
const _: () = assert!(usize::BITS >= 32);

/// Widening to `usize` for indexing. Lossless: the crate requires at least 32-bit pointers.
pub trait Idx {
    fn idx(self) -> usize;
}

impl Idx for u32 {
    #[inline]
    fn idx(self) -> usize {
        self as usize
    }
}

impl Idx for u16 {
    #[inline]
    fn idx(self) -> usize {
        usize::from(self)
    }
}

impl Idx for u8 {
    #[inline]
    fn idx(self) -> usize {
        usize::from(self)
    }
}

/// `usize` to `u64` in const contexts. Lossless: no supported target has pointers over 64 bits.
#[must_use]
pub const fn usize_to_u64(n: usize) -> u64 {
    n as u64
}

/// Conversion to `f32`, rounding to the nearest representable value (exact below 2^24).
pub trait ToF32 {
    fn to_f32(self) -> f32;
}

macro_rules! to_f32 {
    ($($t:ty),*) => {$(
        impl ToF32 for $t {
            #[inline]
            fn to_f32(self) -> f32 {
                self as f32
            }
        }
    )*};
}
to_f32!(usize, u32, u64, i32, i64, f64);

/// Float to integer, saturating at the target's bounds; NaN becomes 0. Fractions truncate toward
/// zero, so round first when nearest is wanted.
pub trait Saturate {
    fn sat_u8(self) -> u8;
    fn sat_u16(self) -> u16;
    fn sat_u32(self) -> u32;
    fn sat_i32(self) -> i32;
    fn sat_i64(self) -> i64;
    fn sat_usize(self) -> usize;
}

impl Saturate for f32 {
    #[inline]
    fn sat_u8(self) -> u8 {
        self as u8
    }
    #[inline]
    fn sat_u16(self) -> u16 {
        self as u16
    }
    #[inline]
    fn sat_u32(self) -> u32 {
        self as u32
    }
    #[inline]
    fn sat_i32(self) -> i32 {
        self as i32
    }
    #[inline]
    fn sat_i64(self) -> i64 {
        self as i64
    }
    #[inline]
    fn sat_usize(self) -> usize {
        self as usize
    }
}

/// Integer narrowing that clamps instead of failing, for counts and indices that are bounded by
/// construction but not by type.
pub trait Clamp {
    fn clamp_u8(self) -> u8;
    fn clamp_u16(self) -> u16;
    fn clamp_u32(self) -> u32;
}

macro_rules! clamp_int {
    ($($t:ty),*) => {$(
        impl Clamp for $t {
            #[inline]
            fn clamp_u8(self) -> u8 {
                u8::try_from(self).unwrap_or(if self > 0 { u8::MAX } else { 0 })
            }
            #[inline]
            fn clamp_u16(self) -> u16 {
                u16::try_from(self).unwrap_or(if self > 0 { u16::MAX } else { 0 })
            }
            #[inline]
            fn clamp_u32(self) -> u32 {
                u32::try_from(self).unwrap_or(if self > 0 { u32::MAX } else { 0 })
            }
        }
    )*};
}
clamp_int!(usize, u16, u32, u64, i32, i64);

/// Hermite step from 0 at `lo` to 1 at `hi`.
#[must_use]
pub fn smoothstep(lo: f32, hi: f32, x: f32) -> f32 {
    let t = ((x - lo) / (hi - lo).max(1e-6)).clamp(0.0, 1.0);
    t * t * 2.0f32.mul_add(-t, 3.0)
}

/// The smallest of `m`'s axis scales: what a local distance shrinks by at most once `m` places it.
#[must_use]
pub fn min_axis_scale(m: &bevy::math::Affine3A) -> f32 {
    (0..3).map(|c| m.matrix3.col(c).length()).fold(f32::INFINITY, f32::min)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn conversions_saturate() {
        assert_eq!((-1.0f32).sat_u32(), 0);
        assert_eq!(f32::NAN.sat_i32(), 0);
        assert_eq!(300.7f32.sat_u8(), u8::MAX);
        assert_eq!(70_000usize.clamp_u16(), u16::MAX);
        assert_eq!((-5i32).clamp_u8(), 0);
        assert_eq!(7u32.idx(), 7);
        assert_eq!(3usize.to_f32(), 3.0);
    }
}
