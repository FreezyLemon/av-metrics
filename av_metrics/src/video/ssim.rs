//! Structural Similarity index.
//!
//! The SSIM index is a full reference metric; in other words, the measurement
//! or prediction of image quality is based on an initial uncompressed or
//! distortion-free image as reference. SSIM is designed to improve on
//! traditional methods such as peak signal-to-noise ratio (PSNR) and mean
//! squared error (MSE).
//!
//! See https://en.wikipedia.org/wiki/Structural_similarity for more details.

use crate::video::decode::Decoder;
use crate::video::pixel::CastFromPrimitive;
use crate::video::pixel::Pixel;
use crate::video::ChromaWeight;
use crate::video::{PlanarMetrics, VideoMetric};
use crate::MetricsError;
use std::cmp;
use std::error::Error;
use std::f64::consts::{E, PI};
use std::mem::size_of;
use v_frame::frame::Frame;
use v_frame::plane::Plane;
use v_frame::prelude::ChromaSampling;

use super::FrameCompare;

/// Calculates the SSIM score between two videos. Higher is better.
#[inline]
pub fn calculate_video_ssim<D: Decoder, F: Fn(usize) + Send>(
    decoder1: &mut D,
    decoder2: &mut D,
    frame_limit: Option<usize>,
    progress_callback: F,
) -> Result<PlanarMetrics, Box<dyn Error>> {
    let cweight = Some(
        decoder1
            .get_video_details()
            .chroma_sampling
            .get_chroma_weight(),
    );
    Ssim { cweight }.process_video(decoder1, decoder2, frame_limit, progress_callback)
}

/// Calculates the SSIM score between two video frames. Higher is better.
#[inline]
pub fn calculate_frame_ssim<T: Pixel>(
    frame1: &Frame<T>,
    frame2: &Frame<T>,
    bit_depth: usize,
    chroma_sampling: ChromaSampling,
) -> Result<PlanarMetrics, Box<dyn Error>> {
    let processor = Ssim::default();
    let result = processor.process_frame(frame1, frame2, bit_depth, chroma_sampling)?;
    let cweight = chroma_sampling.get_chroma_weight();
    Ok(PlanarMetrics {
        y: log10_convert(result.y, 1.0),
        u: log10_convert(result.u, 1.0),
        v: log10_convert(result.v, 1.0),
        avg: log10_convert(
            result.y + cweight * (result.u + result.v),
            1.0 + 2.0 * cweight,
        ),
    })
}

#[derive(Default)]
struct Ssim {
    pub cweight: Option<f64>,
}

impl VideoMetric for Ssim {
    type FrameResult = PlanarMetrics;
    type VideoResult = PlanarMetrics;

    /// Returns the *unweighted* scores. Depending on whether we output per-frame
    /// or per-video, these will be weighted at different points.
    fn process_frame<T: Pixel>(
        &self,
        frame1: &Frame<T>,
        frame2: &Frame<T>,
        bit_depth: usize,
        _chroma_sampling: ChromaSampling,
    ) -> Result<Self::FrameResult, Box<dyn Error>> {
        if (size_of::<T>() == 1 && bit_depth > 8) || (size_of::<T>() == 2 && bit_depth <= 8) {
            return Err(Box::new(MetricsError::InputMismatch {
                reason: "Bit depths does not match pixel width",
            }));
        }

        frame1.can_compare(frame2)?;

        const KERNEL_SHIFT: usize = 8;
        const KERNEL_WEIGHT: usize = 1 << KERNEL_SHIFT;
        let sample_max = (1 << bit_depth) - 1;

        let mut y = 0.0;
        let mut u = 0.0;
        let mut v = 0.0;

        rayon::scope(|s| {
            s.spawn(|_| {
                let y_kernel = build_gaussian_kernel(
                    frame1.planes[0].cfg.height as f64 * 1.5 / 256.0,
                    cmp::min(frame1.planes[0].cfg.width, frame1.planes[0].cfg.height),
                    KERNEL_WEIGHT,
                );
                y = calculate_plane_ssim(
                    &frame1.planes[0],
                    &frame2.planes[0],
                    sample_max,
                    &y_kernel,
                    &y_kernel,
                )
            });

            s.spawn(|_| {
                let u_kernel = build_gaussian_kernel(
                    frame1.planes[1].cfg.height as f64 * 1.5 / 256.0,
                    cmp::min(frame1.planes[1].cfg.width, frame1.planes[1].cfg.height),
                    KERNEL_WEIGHT,
                );
                u = calculate_plane_ssim(
                    &frame1.planes[1],
                    &frame2.planes[1],
                    sample_max,
                    &u_kernel,
                    &u_kernel,
                )
            });

            s.spawn(|_| {
                let v_kernel = build_gaussian_kernel(
                    frame1.planes[2].cfg.height as f64 * 1.5 / 256.0,
                    cmp::min(frame1.planes[2].cfg.width, frame1.planes[2].cfg.height),
                    KERNEL_WEIGHT,
                );
                v = calculate_plane_ssim(
                    &frame1.planes[2],
                    &frame2.planes[2],
                    sample_max,
                    &v_kernel,
                    &v_kernel,
                )
            });
        });

        Ok(PlanarMetrics {
            y,
            u,
            v,
            // Not used here
            avg: 0.,
        })
    }

    fn aggregate_frame_results(
        &self,
        metrics: &[Self::FrameResult],
    ) -> Result<Self::VideoResult, Box<dyn Error>> {
        let cweight = self.cweight.unwrap_or(1.0);
        let y_sum = metrics.iter().map(|m| m.y).sum::<f64>();
        let u_sum = metrics.iter().map(|m| m.u).sum::<f64>();
        let v_sum = metrics.iter().map(|m| m.v).sum::<f64>();
        Ok(PlanarMetrics {
            y: log10_convert(y_sum, metrics.len() as f64),
            u: log10_convert(u_sum, metrics.len() as f64),
            v: log10_convert(v_sum, metrics.len() as f64),
            avg: log10_convert(
                y_sum + cweight * (u_sum + v_sum),
                (1. + 2. * cweight) * metrics.len() as f64,
            ),
        })
    }
}

/// Calculates the MSSSIM score between two videos. Higher is better.
///
/// MSSSIM is a variant of SSIM computed over subsampled versions
/// of an image. It is designed to be a more accurate metric
/// than SSIM.
#[inline]
pub fn calculate_video_msssim<D: Decoder, F: Fn(usize) + Send>(
    decoder1: &mut D,
    decoder2: &mut D,
    frame_limit: Option<usize>,
    progress_callback: F,
) -> Result<PlanarMetrics, Box<dyn Error>> {
    let cweight = Some(
        decoder1
            .get_video_details()
            .chroma_sampling
            .get_chroma_weight(),
    );
    MsSsim { cweight }.process_video(decoder1, decoder2, frame_limit, progress_callback)
}

/// Calculates the MSSSIM score between two video frames. Higher is better.
///
/// MSSSIM is a variant of SSIM computed over subsampled versions
/// of an image. It is designed to be a more accurate metric
/// than SSIM.
#[inline]
pub fn calculate_frame_msssim<T: Pixel>(
    frame1: &Frame<T>,
    frame2: &Frame<T>,
    bit_depth: usize,
    chroma_sampling: ChromaSampling,
) -> Result<PlanarMetrics, Box<dyn Error>> {
    let processor = MsSsim::default();
    let result = processor.process_frame(frame1, frame2, bit_depth, chroma_sampling)?;
    let cweight = chroma_sampling.get_chroma_weight();
    Ok(PlanarMetrics {
        y: log10_convert(result.y, 1.0),
        u: log10_convert(result.u, 1.0),
        v: log10_convert(result.v, 1.0),
        avg: log10_convert(
            result.y + cweight * (result.u + result.v),
            1.0 + 2.0 * cweight,
        ),
    })
}

#[derive(Default)]
struct MsSsim {
    pub cweight: Option<f64>,
}

impl VideoMetric for MsSsim {
    type FrameResult = PlanarMetrics;
    type VideoResult = PlanarMetrics;

    /// Returns the *unweighted* scores. Depending on whether we output per-frame
    /// or per-video, these will be weighted at different points.
    fn process_frame<T: Pixel>(
        &self,
        frame1: &Frame<T>,
        frame2: &Frame<T>,
        bit_depth: usize,
        _chroma_sampling: ChromaSampling,
    ) -> Result<Self::FrameResult, Box<dyn Error>> {
        if (size_of::<T>() == 1 && bit_depth > 8) || (size_of::<T>() == 2 && bit_depth <= 8) {
            return Err(Box::new(MetricsError::InputMismatch {
                reason: "Bit depths does not match pixel width",
            }));
        }

        frame1.can_compare(frame2)?;

        let mut y = 0.0;
        let mut u = 0.0;
        let mut v = 0.0;

        rayon::scope(|s| {
            s.spawn(|_| {
                y = calculate_plane_msssim(&frame1.planes[0], &frame2.planes[0], bit_depth)
            });
            s.spawn(|_| {
                u = calculate_plane_msssim(&frame1.planes[1], &frame2.planes[1], bit_depth)
            });
            s.spawn(|_| {
                v = calculate_plane_msssim(&frame1.planes[2], &frame2.planes[2], bit_depth)
            });
        });

        Ok(PlanarMetrics {
            y,
            u,
            v,
            // Not used here
            avg: 0.,
        })
    }

    fn aggregate_frame_results(
        &self,
        metrics: &[Self::FrameResult],
    ) -> Result<Self::VideoResult, Box<dyn Error>> {
        let cweight = self.cweight.unwrap();
        let y_sum = metrics.iter().map(|m| m.y).sum::<f64>();
        let u_sum = metrics.iter().map(|m| m.u).sum::<f64>();
        let v_sum = metrics.iter().map(|m| m.v).sum::<f64>();
        Ok(PlanarMetrics {
            y: log10_convert(y_sum, metrics.len() as f64),
            u: log10_convert(u_sum, metrics.len() as f64),
            v: log10_convert(v_sum, metrics.len() as f64),
            avg: log10_convert(
                y_sum + cweight * (u_sum + v_sum),
                (1. + 2. * cweight) * metrics.len() as f64,
            ),
        })
    }
}

#[derive(Debug, Clone, Copy, Default)]
struct SsimMoments {
    mux: i64,
    muy: i64,
    x2: i64,
    xy: i64,
    y2: i64,
    w: i64,
}

const SSIM_K1: f64 = 0.01 * 0.01;
const SSIM_K2: f64 = 0.03 * 0.03;

fn calculate_plane_ssim<T: Pixel>(
    plane1: &Plane<T>,
    plane2: &Plane<T>,
    sample_max: u64,
    vert_kernel: &[i64],
    horiz_kernel: &[i64],
) -> f64 {
    let vec1 = plane_to_vec(plane1);
    let vec2 = plane_to_vec(plane2);
    calculate_plane_ssim_internal(
        &vec1,
        &vec2,
        plane1.cfg.width,
        plane1.cfg.height,
        sample_max,
        vert_kernel,
        horiz_kernel,
    )
    .0
}

fn calculate_plane_ssim_internal(
    plane1: &[u32],
    plane2: &[u32],
    width: usize,
    height: usize,
    sample_max: u64,
    vert_kernel: &[i64],
    horiz_kernel: &[i64],
) -> (f64, f64) {
    let vert_offset = vert_kernel.len() >> 1;
    let line_size = vert_kernel.len().next_power_of_two();
    let line_mask = line_size - 1;
    let mut lines = vec![vec![SsimMoments::default(); width]; line_size];
    let horiz_offset = horiz_kernel.len() >> 1;
    let mut ssim = 0.0;
    let mut ssimw = 0.0;
    let mut cs = 0.0;
    for y in 0..(height + vert_offset) {
        if y < height {
            let buf = &mut lines[y & line_mask];
            let line1 = &plane1[(y * width)..];
            let line2 = &plane2[(y * width)..];
            for x in 0..width {
                let mut moments = SsimMoments::default();
                let k_min = horiz_offset.saturating_sub(x);
                let tmp_offset = (x + horiz_offset + 1).saturating_sub(width);
                let k_max = horiz_kernel.len() - tmp_offset;
                for k in k_min..k_max {
                    let window = horiz_kernel[k];
                    let target_x = (x + k).saturating_sub(horiz_offset);
                    let pix1 = line1[target_x] as i64;
                    let pix2 = line2[target_x] as i64;
                    moments.mux += window * pix1;
                    moments.muy += window * pix2;
                    moments.x2 += window * pix1 * pix1;
                    moments.xy += window * pix1 * pix2;
                    moments.y2 += window * pix2 * pix2;
                    moments.w += window;
                }
                buf[x] = moments;
            }
        }
        if y >= vert_offset {
            let k_min = vert_kernel.len().saturating_sub(y + 1);
            let tmp_offset = (y + 1).saturating_sub(height);
            let k_max = vert_kernel.len() - tmp_offset;
            for x in 0..width {
                let mut moments = SsimMoments::default();
                for k in k_min..k_max {
                    let buf = lines[(y + 1 + k - vert_kernel.len()) & line_mask][x];
                    let window = vert_kernel[k];
                    moments.mux += window * buf.mux;
                    moments.muy += window * buf.muy;
                    moments.x2 += window * buf.x2;
                    moments.xy += window * buf.xy;
                    moments.y2 += window * buf.y2;
                    moments.w += window * buf.w;
                }
                let w = moments.w as f64;
                let c1 = sample_max.pow(2) as f64 * SSIM_K1 * w.powi(2);
                let c2 = sample_max.pow(2) as f64 * SSIM_K2 * w.powi(2);
                let mx2 = (moments.mux as f64).powi(2);
                let mxy = moments.mux as f64 * moments.muy as f64;
                let my2 = (moments.muy as f64).powi(2);
                let cs_tmp = w * (c2 + 2.0 * (moments.xy as f64 * w - mxy))
                    / (moments.x2 as f64 * w - mx2 + moments.y2 as f64 * w - my2 + c2);
                cs += cs_tmp;
                ssim += cs_tmp * (2.0 * mxy + c1) / (mx2 + my2 + c1);
                ssimw += w;
            }
        }
    }

    (ssim / ssimw, cs / ssimw)
}

fn calculate_plane_msssim<T: Pixel>(plane1: &Plane<T>, plane2: &Plane<T>, bit_depth: usize) -> f64 {
    const KERNEL_SHIFT: usize = 10;
    const KERNEL_WEIGHT: usize = 1 << KERNEL_SHIFT;
    // These come from the original MS-SSIM implementation paper:
    // https://ece.uwaterloo.ca/~z70wang/publications/msssim.pdf
    // They don't add up to 1 due to rounding done in the paper.
    const MS_WEIGHT: [f64; 5] = [0.0448, 0.2856, 0.3001, 0.2363, 0.1333];

    let mut sample_max = (1 << bit_depth) - 1;
    let mut ssim = [0.0; 5];
    let mut cs = [0.0; 5];
    let mut width = plane1.cfg.width;
    let mut height = plane1.cfg.height;
    let mut plane1 = plane_to_vec(plane1);
    let mut plane2 = plane_to_vec(plane2);

    let kernel = build_gaussian_kernel(1.5, 5, KERNEL_WEIGHT);
    let res = calculate_plane_ssim_internal(
        &plane1, &plane2, width, height, sample_max, &kernel, &kernel,
    );
    ssim[0] = res.0;
    cs[0] = res.1;
    for i in 1..5 {
        plane1 = msssim_downscale(&plane1, width, height);
        plane2 = msssim_downscale(&plane2, width, height);
        width /= 2;
        height /= 2;
        sample_max *= 4;
        let res = calculate_plane_ssim_internal(
            &plane1, &plane2, width, height, sample_max, &kernel, &kernel,
        );
        ssim[i] = res.0;
        cs[i] = res.1;
    }

    cs.iter()
        .zip(MS_WEIGHT.iter())
        .take(4)
        .map(|(cs, weight)| cs.powf(*weight))
        .fold(1.0, |acc, val| acc * val)
        * ssim[4].powf(MS_WEIGHT[4])
}

fn build_gaussian_kernel(sigma: f64, max_len: usize, kernel_weight: usize) -> Vec<i64> {
    let scale = 1.0 / ((2.0 * PI).sqrt() * sigma);
    let nhisigma2 = -0.5 / sigma.powi(2);
    // Compute the kernel size so that the error in the first truncated
    // coefficient is no larger than 0.5*KERNEL_WEIGHT.
    // There is no point in going beyond this given our working precision.
    let s = (0.5 * PI).sqrt() * sigma * (1.0 / kernel_weight as f64);
    let len = if s >= 1.0 {
        0
    } else {
        (sigma * (-2.0 * s.log(E)).sqrt()).floor() as usize
    };
    let kernel_len = if len >= max_len { max_len - 1 } else { len };
    let kernel_size = (kernel_len << 1) | 1;
    let mut kernel = vec![0; kernel_size];
    let mut sum = 0;
    for ci in 1..=kernel_len {
        let val = kernel_weight as f64 * scale * E.powf(nhisigma2 * ci.pow(2) as f64) + 0.5;
        let val = val as i64;
        kernel[kernel_len - ci] = val;
        kernel[kernel_len + ci] = val;
        sum += val;
    }
    kernel[kernel_len] = kernel_weight as i64 - (sum << 1);
    kernel
}

fn plane_to_vec<T: Pixel>(input: &Plane<T>) -> Vec<u32> {
    input.data.iter().map(|pix| u32::cast_from(*pix)).collect()
}

// This acts differently from downscaling a plane, and is what
// requires us to pass around slices of bytes, instead of `Plane`s.
// Instead of averaging the four pixels, it sums them.
// In effect, this gives us much more precision when we downscale.
fn msssim_downscale(input: &[u32], input_width: usize, input_height: usize) -> Vec<u32> {
    let output_width = input_width / 2;
    let output_height = input_height / 2;
    let mut output = vec![0; output_width * output_height];
    for j in 0..output_height {
        let j0 = 2 * j;
        let j1 = cmp::min(j0 + 1, input_height - 1);
        for i in 0..output_width {
            let i0 = 2 * i;
            let i1 = cmp::min(i0 + 1, input_width - 1);
            output[j * output_width + i] = input[j0 * input_width + i0]
                + input[j0 * input_width + i1]
                + input[j1 * input_width + i0]
                + input[j1 * input_width + i1];
        }
    }
    output
}

fn log10_convert(score: f64, weight: f64) -> f64 {
    10.0 * (weight.log10() - (weight - score).log10())
}
