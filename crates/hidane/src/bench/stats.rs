//! Summary statistics of a metric's samples. Every metric reports all of them with the raw
//! samples; comparisons use the median (bench/README.md, "Claim policy").

use serde_json::{Value, json};

#[derive(Debug, Clone, PartialEq)]
pub struct Stats {
    pub n: usize,
    pub median: f64,
    pub p95: f64,
    pub min: f64,
    pub max: f64,
    pub mean: f64,
    /// Sample standard deviation (n − 1 in the denominator); 0 for a single sample.
    pub stddev: f64,
}

/// The statistics of `samples`, ignoring values that are not finite. `None` without samples.
pub fn summarize(samples: &[f64]) -> Option<Stats> {
    let mut sorted: Vec<f64> = samples.iter().copied().filter(|v| v.is_finite()).collect();
    if sorted.is_empty() {
        return None;
    }
    sorted.sort_by(f64::total_cmp);
    let n = sorted.len();
    let count = n as f64;
    let mean = sorted.iter().sum::<f64>() / count;
    let stddev = if n > 1 {
        (sorted.iter().map(|v| (v - mean).powi(2)).sum::<f64>() / (count - 1.0)).sqrt()
    } else {
        0.0
    };
    Some(Stats {
        n,
        median: percentile(&sorted, 0.5),
        p95: percentile(&sorted, 0.95),
        min: sorted[0],
        max: sorted[n - 1],
        mean,
        stddev,
    })
}

/// The `p` quantile (0 ≤ p ≤ 1) of sorted, non-empty samples, interpolated linearly between
/// the closest ranks (numpy's default, Hyndman and Fan's definition 7).
pub fn percentile(sorted: &[f64], p: f64) -> f64 {
    let rank = p.clamp(0.0, 1.0) * (sorted.len() - 1) as f64;
    let (lo, hi) = (rank.floor() as usize, rank.ceil() as usize);
    let fraction = rank - lo as f64;
    sorted[lo] + (sorted[hi] - sorted[lo]) * fraction
}

impl Stats {
    /// Coefficient of variation (stddev / mean), the noise measure of the stability checks.
    pub fn cv(&self) -> f64 {
        if self.mean == 0.0 {
            0.0
        } else {
            self.stddev / self.mean
        }
    }

    pub fn to_json(&self) -> Value {
        json!({
            "n": self.n,
            "median": round(self.median),
            "p95": round(self.p95),
            "min": round(self.min),
            "max": round(self.max),
            "mean": round(self.mean),
            "stddev": round(self.stddev),
        })
    }
}

/// Values are stored with three decimals (microseconds for milliseconds), which keeps the files
/// small without hiding anything a timer on a shared machine can resolve.
pub fn round(value: f64) -> f64 {
    (value * 1000.0).round() / 1000.0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-9
    }

    #[test]
    fn odd_and_even_medians() {
        assert_eq!(summarize(&[3.0, 1.0, 2.0]).unwrap().median, 2.0);
        assert_eq!(summarize(&[4.0, 1.0, 3.0, 2.0]).unwrap().median, 2.5);
    }

    #[test]
    fn p95_interpolates_between_ranks() {
        let samples: Vec<f64> = (1..=10).map(f64::from).collect();
        // rank 0.95 × 9 = 8.55: between 9 and 10.
        assert!(close(summarize(&samples).unwrap().p95, 9.55));
        let samples: Vec<f64> = (1..=101).map(f64::from).collect();
        assert!(close(summarize(&samples).unwrap().p95, 96.0));
    }

    #[test]
    fn spread_and_bounds() {
        let s = summarize(&[2.0, 4.0, 4.0, 4.0, 5.0, 5.0, 7.0, 9.0]).unwrap();
        assert_eq!((s.n, s.min, s.max), (8, 2.0, 9.0));
        assert!(close(s.mean, 5.0));
        // Sample standard deviation: sqrt(32 / 7).
        assert!(close(s.stddev, (32.0f64 / 7.0).sqrt()));
        assert!(close(s.cv(), (32.0f64 / 7.0).sqrt() / 5.0));
    }

    #[test]
    fn one_sample_and_none() {
        let s = summarize(&[7.5]).unwrap();
        assert_eq!((s.n, s.median, s.p95, s.stddev), (1, 7.5, 7.5, 0.0));
        assert_eq!(summarize(&[]), None);
        assert_eq!(summarize(&[f64::NAN]), None);
        assert_eq!(summarize(&[1.0, f64::INFINITY]).unwrap().n, 1);
    }

    #[test]
    fn json_keeps_every_statistic() {
        let json = summarize(&[1.0, 2.0, 3.0]).unwrap().to_json();
        for key in ["n", "median", "p95", "min", "max", "mean", "stddev"] {
            assert!(json.get(key).is_some(), "{key}");
        }
        assert_eq!(json["n"], 3);
        assert_eq!(round(1.234_567), 1.235);
    }
}
