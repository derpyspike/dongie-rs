// SPDX-License-Identifier: EUPL-1.2
//! In-RAM history (24 h) for charts: signal, band, temperatures, memory.
//! One sample per status poll; queries are averaged down into buckets.

use serde::Serialize;
use std::collections::{BTreeMap, VecDeque};

#[derive(Debug, Clone, Default)]
pub struct Sample {
    pub t: u64,
    pub connected: bool,
    pub tech: Option<String>,
    /// "LTE B3", "3G 2100", ...
    pub band: Option<String>,
    pub rsrp: Option<f64>,
    pub rsrq: Option<f64>,
    pub snr: Option<f64>,
    pub w_rssi: Option<f64>,
    pub w_ecio: Option<f64>,
    pub neighbors: u32,
    pub temp_cpu: Option<f64>,
    pub temp_modem: Option<f64>,
    pub mem_used_pct: Option<f64>,
    /// KiB written to the eMMC per minute since the previous sample
    pub flash_kib_min: Option<f64>,
}

#[derive(Debug, Serialize, Default)]
pub struct Point {
    pub t: u64,
    pub rsrp: Option<f64>,
    pub rsrq: Option<f64>,
    pub snr: Option<f64>,
    pub w_rssi: Option<f64>,
    pub w_ecio: Option<f64>,
    pub temp_cpu: Option<f64>,
    pub temp_modem: Option<f64>,
    pub mem_used_pct: Option<f64>,
    pub flash_kib_min: Option<f64>,
    pub neighbors: Option<f64>,
    /// most common tech in the bucket ("lte", "umts", ... or None = no network)
    pub tech: Option<String>,
    pub band: Option<String>,
}

#[derive(Debug, Serialize, Default)]
pub struct HistoryView {
    pub range: u64,
    pub step: u64,
    pub samples: usize,
    pub points: Vec<Point>,
    /// seconds spent per band label ("no network" included), descending
    pub bands: Vec<(String, u64)>,
    /// number of band/tech changes in the range
    pub switches: u32,
}

pub struct History {
    buf: VecDeque<Sample>,
    cap: usize,
    pub interval: u64,
}

fn avg(v: &[Option<f64>]) -> Option<f64> {
    let xs: Vec<f64> = v.iter().flatten().copied().collect();
    if xs.is_empty() {
        None
    } else {
        Some((xs.iter().sum::<f64>() / xs.len() as f64 * 10.0).round() / 10.0)
    }
}

fn mode(v: &[Option<String>]) -> Option<String> {
    let mut m: BTreeMap<&str, usize> = BTreeMap::new();
    for s in v.iter().flatten() {
        *m.entry(s.as_str()).or_default() += 1;
    }
    m.into_iter().max_by_key(|x| x.1).map(|x| x.0.to_string())
}

impl History {
    pub fn new(interval: u64) -> Self {
        let interval = interval.max(1);
        let cap = (24 * 3600 / interval) as usize + 1;
        History { buf: VecDeque::with_capacity(cap), cap, interval }
    }

    pub fn push(&mut self, s: Sample) {
        if self.buf.len() >= self.cap {
            self.buf.pop_front();
        }
        self.buf.push_back(s);
    }

    pub fn query(&self, range: u64, now: u64, max_points: usize) -> HistoryView {
        let range = range.clamp(60, 24 * 3600);
        let from = now.saturating_sub(range);
        let sel: Vec<&Sample> = self.buf.iter().filter(|s| s.t >= from).collect();
        let step = (range / max_points.max(1) as u64).max(self.interval);
        let mut v = HistoryView { range, step, samples: sel.len(), ..Default::default() };

        // band time + switches
        let mut secs: BTreeMap<String, u64> = BTreeMap::new();
        let mut last: Option<String> = None;
        for s in &sel {
            let label = if s.connected || s.band.is_some() {
                s.band.clone().unwrap_or_else(|| "unknown band".into())
            } else {
                "no network".into()
            };
            *secs.entry(label.clone()).or_default() += self.interval;
            if last.as_ref().is_some_and(|l| *l != label) {
                v.switches += 1;
            }
            last = Some(label);
        }
        let mut bands: Vec<(String, u64)> = secs.into_iter().collect();
        bands.sort_by(|a, b| b.1.cmp(&a.1));
        v.bands = bands;

        // bucket
        let mut i = 0;
        let mut t0 = from;
        while t0 < now {
            let t1 = t0 + step;
            let mut b: Vec<&Sample> = vec![];
            while i < sel.len() && sel[i].t < t1 {
                if sel[i].t >= t0 {
                    b.push(sel[i]);
                }
                i += 1;
            }
            if !b.is_empty() {
                let f = |g: fn(&Sample) -> Option<f64>| avg(&b.iter().map(|s| g(s)).collect::<Vec<_>>());
                v.points.push(Point {
                    t: t0,
                    rsrp: f(|s| s.rsrp),
                    rsrq: f(|s| s.rsrq),
                    snr: f(|s| s.snr),
                    w_rssi: f(|s| s.w_rssi),
                    w_ecio: f(|s| s.w_ecio),
                    temp_cpu: f(|s| s.temp_cpu),
                    temp_modem: f(|s| s.temp_modem),
                    mem_used_pct: f(|s| s.mem_used_pct),
                    flash_kib_min: f(|s| s.flash_kib_min),
                    neighbors: f(|s| Some(s.neighbors as f64)),
                    tech: mode(&b.iter().map(|s| s.tech.clone()).collect::<Vec<_>>()),
                    band: mode(&b.iter().map(|s| s.band.clone()).collect::<Vec<_>>()),
                });
            }
            t0 = t1;
        }
        v
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn buckets_and_bands() {
        let mut h = History::new(10);
        for k in 0..60u64 {
            h.push(Sample {
                t: 1000 + k * 10,
                connected: k < 50,
                tech: (k < 50).then(|| "lte".into()),
                band: (k < 50).then(|| if k < 30 { "LTE B3".into() } else { "LTE B20".into() }),
                rsrp: Some(-100.0 - (k % 2) as f64),
                ..Default::default()
            });
        }
        let v = h.query(600, 1600, 30);
        assert_eq!(v.samples, 60);
        assert_eq!(v.step, 20);
        assert_eq!(v.points.len(), 30);
        assert_eq!(v.points[0].rsrp, Some(-100.5));
        assert_eq!(v.bands[0], ("LTE B3".to_string(), 300));
        assert_eq!(v.switches, 2);
    }

    #[test]
    fn ring_cap() {
        let mut h = History::new(3600);
        for k in 0..100 {
            h.push(Sample { t: k, ..Default::default() });
        }
        assert_eq!(h.buf.len(), 25);
    }
}
