//! FSRS-6 memory model (Free Spaced Repetition Scheduler; Ye et al. 2022,
//! open-spaced-repetition/fsrs-rs v6), with the default parameters.
//!
//! This is a port of fsrs-rs v6.6.2's scalar `step`: stability `S` in days,
//! difficulty `D` in 1–10, and the power forgetting curve
//! `R(t) = (1 + factor · t / S)^(−w20)`, where `factor` makes `R(S) = 0.9`.
//! Elapsed time between reviews counts in whole days, rounded, as in the
//! reference: two reviews less than half a day apart are a same-day review,
//! which takes the short-term stability formula.
//!
//! The `fsrs` crate itself is not used. It drags in the `burn` training stack
//! for the optimizer, and the learner model only needs the forward pass,
//! which is these few lines. Refitting the parameters on the learner's own log
//! (arch §4.13, mitigation) would bring the crate in when it is built.

/// Default FSRS-6 parameters `w0..w20` (fsrs-rs `FSRS6_DEFAULT_PARAMETERS`).
pub const W: [f64; 21] = [
    0.212, 1.2931, 2.3065, 8.2956, 6.4133, 0.8334, 3.0194, 0.001, 1.8722, 0.1666, 0.796, 1.4835,
    0.0614, 0.2629, 1.6483, 0.6014, 1.8729, 0.5425, 0.0912, 0.0658, 0.1542,
];

const S_MIN: f64 = 0.0001;
const S_MAX: f64 = 36500.0;
const D_MIN: f64 = 1.0;
const D_MAX: f64 = 10.0;

/// An FSRS rating. A verdict maps to one in [`super::rating`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Rating {
    Again = 1,
    Hard = 2,
    Good = 3,
    Easy = 4,
}

impl Rating {
    fn value(self) -> f64 {
        f64::from(self as u8)
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Rating::Again => "again",
            Rating::Hard => "hard",
            Rating::Good => "good",
            Rating::Easy => "easy",
        }
    }
}

/// Stability (days until R falls to 0.9) and difficulty (1–10).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Memory {
    pub stability: f64,
    pub difficulty: f64,
}

/// Retrievability after `days` (fractional) with stability `stability`, on
/// the forgetting curve with decay `decay` (`w20` for this model).
pub fn forgetting_curve(days: f64, stability: f64, decay: f64) -> f64 {
    let factor = 0.9f64.powf(-1.0 / decay) - 1.0;
    (1.0 + factor * days.max(0.0) / stability.max(S_MIN)).powf(-decay)
}

/// Retrievability now, `days` after the last review.
pub fn retrievability(memory: Memory, days: f64) -> f64 {
    forgetting_curve(days, memory.stability, W[20])
}

/// Days after the last review at which R falls to `desired_retention`.
pub fn next_interval(stability: f64, desired_retention: f64) -> f64 {
    let decay = -W[20];
    let factor = 0.9f64.powf(1.0 / decay) - 1.0;
    let r = desired_retention.clamp(0.0001, 0.9999);
    (stability / factor * (r.powf(1.0 / decay) - 1.0)).clamp(0.0, S_MAX)
}

fn init_difficulty(rating: f64) -> f64 {
    W[4] - (W[5] * (rating - 1.0)).exp() + 1.0
}

/// The state after a node's first review.
pub fn init(rating: Rating) -> Memory {
    Memory {
        stability: W[rating as usize - 1].clamp(S_MIN, S_MAX),
        difficulty: init_difficulty(rating.value()).clamp(D_MIN, D_MAX),
    }
}

/// The state after one more review, `days` after the previous one.
pub fn step(last: Memory, days: f64, rating: Rating) -> Memory {
    let s = last.stability.clamp(S_MIN, S_MAX);
    let d = last.difficulty.clamp(D_MIN, D_MAX);
    let t = days.max(0.0).round();
    let g = rating.value();
    let stability = if t == 0.0 {
        let sinc = (W[17] * (g - 3.0 + W[18])).exp() * s.powf(-W[19]);
        s * if rating >= Rating::Hard {
            sinc.max(1.0)
        } else {
            sinc
        }
    } else {
        let r = forgetting_curve(t, s, W[20]);
        if rating == Rating::Again {
            let new_s =
                W[11] * d.powf(-W[12]) * ((s + 1.0).powf(W[13]) - 1.0) * ((1.0 - r) * W[14]).exp();
            new_s.min(s / (W[17] * W[18]).exp())
        } else {
            let hard = if rating == Rating::Hard { W[15] } else { 1.0 };
            let easy = if rating == Rating::Easy { W[16] } else { 1.0 };
            s * (W[8].exp()
                * (11.0 - d)
                * s.powf(-W[9])
                * (((1.0 - r) * W[10]).exp() - 1.0)
                * hard
                * easy
                + 1.0)
        }
    };
    let damped = d + (10.0 - d) * (-W[6] * (g - 3.0)) / 9.0;
    let difficulty = W[7] * (init_difficulty(4.0) - damped) + damped;
    Memory {
        stability: stability.clamp(S_MIN, S_MAX),
        difficulty: difficulty.clamp(D_MIN, D_MAX),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// fsrs-rs v6.6.2 `test_memory_state`: Again, then five Goods at 0, 1,
    /// 3, 8 and 21 days, with the default parameters.
    #[test]
    fn matches_the_reference_memory_state() {
        let ratings = [
            Rating::Again,
            Rating::Good,
            Rating::Good,
            Rating::Good,
            Rating::Good,
            Rating::Good,
        ];
        let days = [0.0, 0.0, 1.0, 3.0, 8.0, 21.0];
        let mut m = init(ratings[0]);
        for (r, t) in ratings.iter().zip(days).skip(1) {
            m = step(m, t, *r);
        }
        assert!((m.stability - 53.62691).abs() < 1e-3, "{m:?}");
        assert!((m.difficulty - 6.3574867).abs() < 1e-4, "{m:?}");
    }

    /// fsrs-rs v6.6.2 `test_next_interval`: S = 1 day at desired retention
    /// 0.1 … 1.0, rounded and at least one day. Current fsrs-rs caps an
    /// interval at the maximum stability (36,500 days), which only the first
    /// one (v6.6.2: 3,116,766) reaches.
    #[test]
    fn matches_the_reference_intervals() {
        let got: Vec<i64> = (1..=10)
            .map(|i| (next_interval(1.0, f64::from(i) / 10.0).round().max(1.0)) as i64)
            .collect();
        assert_eq!(got, [36500, 34793, 2508, 387, 90, 27, 9, 3, 1, 1]);
        assert!((next_interval(121.01552, 0.9) - 121.01552).abs() < 1e-3);
    }

    /// fsrs-rs v6.6.2 `test_current_retrievability` (decay 0.2, S = 1).
    #[test]
    fn matches_the_reference_forgetting_curve() {
        let r: Vec<f64> = [0.0, 1.0, 2.0, 3.0]
            .iter()
            .map(|t| forgetting_curve(*t, 1.0, 0.2))
            .collect();
        for (got, want) in r.iter().zip([1.0, 0.9, 0.84028935, 0.7985001]) {
            assert!((got - want).abs() < 1e-6, "{r:?}");
        }
        // R(S) = 0.9 for the default decay too.
        assert!((retrievability(init(Rating::Good), W[2]) - 0.9).abs() < 1e-9);
    }

    #[test]
    fn ratings_order_the_next_stability() {
        let m = init(Rating::Good);
        let next: Vec<f64> = [Rating::Again, Rating::Hard, Rating::Good, Rating::Easy]
            .iter()
            .map(|r| step(m, 5.0, *r).stability)
            .collect();
        assert!(next.windows(2).all(|w| w[0] < w[1]), "{next:?}");
        assert!(next[0] < m.stability, "a lapse shrinks stability");
    }
}
