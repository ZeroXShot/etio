//! Plain-language reasons for a ranking position.
//!
//! Reasons are generated deterministically from the numbers behind the
//! ranking, so every sentence can be traced to a feature value or a series
//! score. They are what an on-call engineer reads first, and they are also
//! the grounded facts handed to an optional language model when a longer
//! narrative is requested.

use std::collections::BTreeMap;

use super::Signal;

fn feature(features: &BTreeMap<String, f64>, name: &str) -> f64 {
    features.get(name).copied().unwrap_or(0.0)
}

/// Three significant digits: "22.1", "3.54", "0.00412", "12345".
fn significant(x: f64) -> String {
    if !x.is_finite() {
        return "n/a".to_owned();
    }
    if x == 0.0 {
        return "0".to_owned();
    }
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let decimals = (2.0 - x.abs().log10().floor()).clamp(0.0, 8.0) as usize;
    format!("{x:.decimals$}")
}

fn describe_onset(onset: Option<f64>) -> String {
    match onset {
        None => String::new(),
        Some(o) if o.abs() < 0.5 => ", starting when the incident began".to_owned(),
        Some(o) if o < 0.0 => format!(", starting {:.0}s before the incident was detected", -o),
        Some(o) => format!(", starting {o:.0}s after the incident began"),
    }
}

/// Builds up to five reasons, strongest first.
#[must_use]
pub fn reasons(
    features: &BTreeMap<String, f64>,
    contributions: &[(String, f64)],
    signals: &[Signal],
    threshold: f64,
) -> Vec<String> {
    let mut out = Vec::new();

    if let Some(top) = signals.first().filter(|s| s.score.max > threshold) {
        let direction = if top.score.peak_value >= top.score.reference_median { "rose" } else { "fell" };
        out.push(format!(
            "{} ({}) {} to {} against a baseline of {}: {:.1} robust deviations{}",
            top.name,
            top.category,
            direction,
            significant(top.score.peak_value),
            significant(top.score.reference_median),
            top.score.max,
            describe_onset(top.score.onset_s),
        ));
    }

    // Order the remaining evidence by how much it moved the model's score;
    // without a model (baseline methods) fall back to a fixed order.
    let mut keys: Vec<(String, f64)> = if contributions.is_empty() {
        ["rank_score", "log_resource", "log_trace_local", "onset_lead", "upstream_anomalous", "log_logs"]
            .iter()
            .map(|k| ((*k).to_owned(), 1.0))
            .collect()
    } else {
        contributions.iter().filter(|(_, c)| *c > 0.05).cloned().collect()
    };
    keys.sort_by(|a, b| b.1.total_cmp(&a.1));

    for (name, _) in keys {
        let v = feature(features, &name);
        let reason = match name.as_str() {
            "rel_max" | "rank_score" | "log_max" if feature(features, "rank_score") >= 1.0 => {
                Some("it is the most anomalous service in the window".to_owned())
            }
            "log_resource" if v.exp_m1() > threshold => Some(format!(
                "its own resources are saturated (up to {:.1} deviations): the problem is local, not inherited",
                v.exp_m1()
            )),
            "log_trace_local" if v.exp_m1() > threshold => {
                Some("traces show errors or self-time originating inside it".to_owned())
            }
            "onset_lead" if v >= 1.0 && feature(features, "has_onset") > 0.0 => {
                Some("its anomaly started before those of other services".to_owned())
            }
            "walk" if v >= 0.99 => Some("a random walk over the anomalous dependency graph converges on it".to_owned()),
            "upstream_anomalous" if v > 0.0 => {
                Some(format!("{:.0}% of the services that depend on it are also anomalous", 100.0 * v))
            }
            "log_logs" if v.exp_m1() > threshold => Some("its error logs increased".to_owned()),
            "frac_silent" if v >= 0.5 => Some(format!(
                "{:.0}% of its signals stopped reporting when the incident began (down or unreachable)",
                100.0 * v
            )),
            "frac_anomalous" if v >= 0.5 => Some(format!("{:.0}% of its signals are anomalous", 100.0 * v)),
            _ => None,
        };
        if let Some(r) = reason
            && !out.contains(&r)
        {
            out.push(r);
        }
        if out.len() >= 5 {
            break;
        }
    }

    if feature(features, "in_graph") > 0.0
        && feature(features, "callee_explained") < 0.25
        && feature(features, "log_latency").exp_m1() > threshold
    {
        let r = "none of its dependencies is anomalous enough to explain its latency".to_owned();
        if out.len() < 5 && !out.contains(&r) {
            out.push(r);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbers_have_three_significant_digits() {
        assert_eq!(significant(22.0873), "22.1");
        assert_eq!(significant(3.5431), "3.54");
        assert_eq!(significant(0.004_123), "0.00412");
        assert_eq!(significant(12_345.6), "12346");
        assert_eq!(significant(-0.5), "-0.500");
        assert_eq!(significant(0.0), "0");
        assert_eq!(significant(f64::NAN), "n/a");
    }
    use crate::rca::score::SeriesScore;
    use etio_core::SignalCategory;

    fn signal() -> Signal {
        Signal {
            service: "cart".into(),
            name: "cpu".into(),
            category: SignalCategory::Cpu,
            score: SeriesScore {
                max: 12.0,
                sustained: 9.0,
                shift: 9.0,
                onset_s: Some(0.0),
                reference_median: 0.3,
                scale: 0.05,
                peak_value: 0.9,
                peak_offset_s: 12.0,
            },
        }
    }

    #[test]
    fn reasons_follow_the_evidence() {
        let features = BTreeMap::from([
            ("rank_score".to_owned(), 1.0),
            ("log_resource".to_owned(), 12.0f64.ln_1p()),
            ("onset_lead".to_owned(), 1.0),
            ("has_onset".to_owned(), 1.0),
            ("upstream_anomalous".to_owned(), 0.5),
        ]);
        let contributions = vec![
            ("rank_score".to_owned(), 1.0),
            ("log_resource".to_owned(), 0.8),
            ("onset_lead".to_owned(), 0.4),
            ("upstream_anomalous".to_owned(), 0.2),
            ("is_entry".to_owned(), -0.5),
        ];
        let r = reasons(&features, &contributions, &[signal()], 3.0);
        assert!(r[0].starts_with("cpu (cpu) rose to 0.900"), "{r:?}");
        assert!(r[0].ends_with("starting when the incident began"));
        assert!(r.iter().any(|x| x.contains("most anomalous")));
        assert!(r.iter().any(|x| x.contains("local, not inherited")));
        assert!(r.iter().any(|x| x.contains("50% of the services")));
        assert!(r.len() <= 5);
    }

    #[test]
    fn no_signal_no_claims() {
        let r = reasons(&BTreeMap::new(), &[], &[], 3.0);
        assert!(r.is_empty(), "{r:?}");
    }
}
