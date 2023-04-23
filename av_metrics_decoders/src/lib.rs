//! This crate providers ready-made decoders for use with av-metrics or other tools as needed.
//!
//! No decoders are enabled by default. They must be enabled via Cargo features.
//!
//! Currently supported decoder features: y4m

#![deny(missing_docs)]

#[cfg(feature = "y4m")]
/// Items related to decoding y4m video
pub mod y4m;

#[cfg(feature = "y4m")]
pub use crate::y4m::Y4MDecoder;

#[cfg(any(
    feature = "ffmpeg",
    feature = "ffmpeg_static",
    feature = "ffmpeg_build"
))]
mod ffmpeg;

#[cfg(any(
    feature = "ffmpeg",
    feature = "ffmpeg_static",
    feature = "ffmpeg_build"
))]
pub use crate::ffmpeg::FfmpegDecoder;

#[cfg(feature = "vapoursynth")]
mod vapoursynth;

#[cfg(feature = "vapoursynth")]
pub use crate::vapoursynth::VapoursynthDecoder;

pub use av_metrics::video::decode::{Decoder, VideoDetails};
pub use av_metrics::video::{CastFromPrimitive, ChromaSampling, Frame, Pixel, Plane};
