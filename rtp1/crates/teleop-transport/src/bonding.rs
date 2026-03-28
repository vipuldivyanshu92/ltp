//! Duplicate vs split send policies and path weighting.

use crate::header::{PayloadType, PriorityClass};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SendMode {
    /// Replicate the same datagram on every path (control / haptic).
    Duplicate,
    /// Choose one path by weighted schedule (video / bulk telemetry).
    Split,
}

#[derive(Clone, Debug)]
pub struct BondingPolicy {
    pub loss_threshold: f64,
    pub rtt_spike_factor: f64,
}

impl Default for BondingPolicy {
    fn default() -> Self {
        Self {
            loss_threshold: 0.08,
            rtt_spike_factor: 2.0,
        }
    }
}

pub fn mode_for(priority: PriorityClass, payload_type: PayloadType) -> SendMode {
    match (priority, payload_type) {
        (PriorityClass::Control | PriorityClass::Haptic, _) => SendMode::Duplicate,
        (_, PayloadType::VideoSlice | PayloadType::Telemetry) => SendMode::Split,
        _ => SendMode::Split,
    }
}

/// Pick path index for split mode using inverse RTT weights (lower RTT → higher weight).
pub fn pick_split_path(rtt_ms: &[f64]) -> usize {
    if rtt_ms.is_empty() {
        return 0;
    }
    let mut best = 0usize;
    let mut best_w = f64::NEG_INFINITY;
    for (i, &r) in rtt_ms.iter().enumerate() {
        let w = 1.0 / r.max(0.5);
        if w > best_w {
            best_w = w;
            best = i;
        }
    }
    best
}

/// If path `i` is unhealthy, return preferred alive path index or 0.
pub fn fast_switch_path(
    i: usize,
    path_loss: &[f64],
    path_rtt: &[f64],
    baseline_rtt: &[f64],
    policy: &BondingPolicy,
) -> usize {
    if path_loss.get(i).copied().unwrap_or(0.0) > policy.loss_threshold {
        return (0..path_loss.len())
            .filter(|&j| j != i && path_loss[j] <= policy.loss_threshold)
            .min_by(|&a, &b| path_rtt[a].partial_cmp(&path_rtt[b]).unwrap())
            .unwrap_or(0);
    }
    let br = baseline_rtt.get(i).copied().unwrap_or(10.0);
    let rt = path_rtt.get(i).copied().unwrap_or(br);
    if rt > br * policy.rtt_spike_factor {
        return (0..path_rtt.len())
            .filter(|&j| j != i)
            .min_by(|&a, &b| path_rtt[a].partial_cmp(&path_rtt[b]).unwrap())
            .unwrap_or(0);
    }
    i
}
