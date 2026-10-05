//! ICC profiles: parsing (header, tags, descriptions) and conversion of
//! pixels to sRGB the way Pillow's `ImageCms.profileToProfile(...,
//! createProfile("sRGB"), outputMode="RGB")` does with LittleCMS 2:
//! perceptual intent, black point compensation forced by the v4 sRGB
//! output, and lcms's 8-bit matrix-shaper optimization (1.14 fixed point).
#![forbid(unsafe_code)]

mod curve;
mod lut;
mod profile;
mod srgb;
mod transform;

pub use curve::Curve;
pub use profile::{Profile, Tag};
pub use transform::Transform;
