//! Normal distribution functions.

/// Standard normal cumulative distribution function Φ(x).
#[must_use]
pub fn normal_cdf(x: f64) -> f64 {
    0.5 * libm::erfc(-x / std::f64::consts::SQRT_2)
}

/// Standard normal survival function 1 − Φ(x), accurate far into the tail.
#[must_use]
pub fn normal_sf(x: f64) -> f64 {
    0.5 * libm::erfc(x / std::f64::consts::SQRT_2)
}

/// Inverse of the standard normal CDF, Φ⁻¹(p).
///
/// Uses Acklam's rational approximation followed by one Halley refinement
/// step, which brings the relative error to the level of `f64` rounding.
/// Returns ±∞ at the endpoints and NaN outside `[0, 1]`.
#[must_use]
pub fn normal_quantile(p: f64) -> f64 {
    const A: [f64; 6] = [
        -3.969_683_028_665_376e1,
        2.209_460_984_245_205e2,
        -2.759_285_104_469_687e2,
        1.383_577_518_672_69e2,
        -3.066_479_806_614_716e1,
        2.506_628_277_459_239,
    ];
    const B: [f64; 5] = [
        -5.447_609_879_822_406e1,
        1.615_858_368_580_409e2,
        -1.556_989_798_598_866e2,
        6.680_131_188_771_972e1,
        -1.328_068_155_288_572e1,
    ];
    const C: [f64; 6] = [
        -7.784_894_002_430_293e-3,
        -3.223_964_580_411_365e-1,
        -2.400_758_277_161_838,
        -2.549_732_539_343_734,
        4.374_664_141_464_968,
        2.938_163_982_698_783,
    ];
    const D: [f64; 4] =
        [7.784_695_709_041_462e-3, 3.224_671_290_700_398e-1, 2.445_134_137_142_996, 3.754_408_661_907_416];
    const P_LOW: f64 = 0.024_25;

    if p.is_nan() || !(0.0..=1.0).contains(&p) {
        return f64::NAN;
    }
    if p == 0.0 {
        return f64::NEG_INFINITY;
    }
    if p >= 1.0 {
        return f64::INFINITY;
    }

    let x = if p < P_LOW {
        let q = (-2.0 * p.ln()).sqrt();
        (((((C[0] * q + C[1]) * q + C[2]) * q + C[3]) * q + C[4]) * q + C[5])
            / ((((D[0] * q + D[1]) * q + D[2]) * q + D[3]) * q + 1.0)
    } else if p <= 1.0 - P_LOW {
        let q = p - 0.5;
        let r = q * q;
        (((((A[0] * r + A[1]) * r + A[2]) * r + A[3]) * r + A[4]) * r + A[5]) * q
            / (((((B[0] * r + B[1]) * r + B[2]) * r + B[3]) * r + B[4]) * r + 1.0)
    } else {
        let q = (-2.0 * (1.0 - p).ln()).sqrt();
        -(((((C[0] * q + C[1]) * q + C[2]) * q + C[3]) * q + C[4]) * q + C[5])
            / ((((D[0] * q + D[1]) * q + D[2]) * q + D[3]) * q + 1.0)
    };

    // One step of Halley's method on Φ(x) − p.
    let e = normal_cdf(x) - p;
    let u = e * (2.0 * std::f64::consts::PI).sqrt() * (x * x / 2.0).exp();
    x - u / (1.0 + x * u / 2.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cdf_known_values() {
        assert!((normal_cdf(0.0) - 0.5).abs() < 1e-15);
        assert!((normal_cdf(1.959_963_984_540_054) - 0.975).abs() < 1e-12);
        assert!((normal_sf(5.0) - 2.866_515_718_791_939e-7).abs() < 1e-18);
    }

    #[test]
    fn quantile_inverts_cdf() {
        for &p in &[1e-12, 1e-6, 0.001, 0.02, 0.1, 0.25, 0.5, 0.75, 0.9, 0.98, 0.999, 1.0 - 1e-9] {
            let x = normal_quantile(p);
            let back = normal_cdf(x);
            assert!((back - p).abs() / p.min(1.0 - p) < 1e-9, "p={p} x={x} back={back}");
        }
        assert!((normal_quantile(0.75) - 0.674_489_750_196_081_7).abs() < 1e-13);
        assert!(normal_quantile(-0.1).is_nan());
        let lo = normal_quantile(0.0);
        assert!(lo.is_infinite() && lo.is_sign_negative());
    }

    #[test]
    fn scale_constants_match_quantiles() {
        let q75 = normal_quantile(0.75);
        assert!((1.0 / q75 - super::super::MAD_TO_SIGMA).abs() < 1e-12);
        assert!((1.0 / (2.0 * q75) - super::super::IQR_TO_SIGMA).abs() < 1e-12);
    }
}
