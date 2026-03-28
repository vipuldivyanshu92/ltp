//! Per-path RTT and loss EWMA (lightweight).

#[derive(Clone, Debug)]
pub struct PathMetrics {
    pub rtt_ewma_ms: f64,
    pub loss_ewma: f64,
    alpha: f64,
}

impl Default for PathMetrics {
    fn default() -> Self {
        Self {
            rtt_ewma_ms: 10.0,
            loss_ewma: 0.0,
            alpha: 0.2,
        }
    }
}

impl PathMetrics {
    pub fn new(alpha: f64) -> Self {
        Self {
            rtt_ewma_ms: 10.0,
            loss_ewma: 0.0,
            alpha: alpha.clamp(0.01, 1.0),
        }
    }

    pub fn observe_rtt_ms(&mut self, sample_ms: f64) {
        let a = self.alpha;
        self.rtt_ewma_ms = a * sample_ms + (1.0 - a) * self.rtt_ewma_ms;
    }

    /// `lost` true if this slot was a loss (no sample).
    pub fn observe_loss(&mut self, lost: bool) {
        let v = if lost { 1.0 } else { 0.0 };
        let a = self.alpha;
        self.loss_ewma = a * v + (1.0 - a) * self.loss_ewma;
    }
}
