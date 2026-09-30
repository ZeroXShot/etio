//! Streaming and robust statistics.
//!
//! * [`robust`]: order statistics on slices (median, MAD, quantiles, IQR).
//! * [`window`]: an exact sliding window with `O(log n)` median and MAD.
//! * [`evt`]: generalised Pareto fitting and the SPOT extreme-value detector.
//! * [`cusum`]: two-sided CUSUM change detection with onset estimation.
//! * [`online`]: Welford moments and exponentially weighted averages.
//! * [`special`]: normal distribution functions.

pub mod cusum;
pub mod evt;
pub mod online;
pub mod robust;
pub mod special;
pub mod window;

/// Scale factor that makes the MAD a consistent estimator of the standard
/// deviation for normally distributed data: `1 / Φ⁻¹(3/4)`.
pub const MAD_TO_SIGMA: f64 = 1.482_602_218_505_602;

/// Scale factor that makes the IQR a consistent estimator of the standard
/// deviation for normally distributed data: `1 / (2 Φ⁻¹(3/4))`.
pub const IQR_TO_SIGMA: f64 = 0.741_301_109_252_801;
