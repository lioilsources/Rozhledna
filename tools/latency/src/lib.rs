//! Per-frame latency records and P50/P95 stats (plan T0.5, chapter 12).
//!
//! All timestamps in a [`FrameRecord`] are in the viewer's monotonic clock (ns); agent timestamps
//! are mapped through clock sync before being recorded.
//!
//! Pipeline segments: capture → encode → send/network → recv → decode → present.

use std::fmt::Write as _;
use std::io::{BufRead, Write};

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct FrameRecord {
    pub frame_no: u64,
    pub keyframe: bool,
    pub bytes: u32,
    pub width: u16,
    pub height: u16,
    pub capture_ns: u64,
    pub encode_done_ns: u64,
    pub recv_ns: u64,
    pub decode_done_ns: u64,
    pub present_ns: u64,
}

pub const CSV_HEADER: &str =
    "frame_no,keyframe,bytes,width,height,capture_ns,encode_done_ns,recv_ns,decode_done_ns,present_ns";

/// Extracts one segment duration (ms) from a record.
pub type SegmentFn = fn(&FrameRecord) -> f64;

/// Names and extractors of the pipeline segments, in ms.
pub const SEGMENTS: &[(&str, SegmentFn)] = &[
    ("encode", |r| ms(r.encode_done_ns, r.capture_ns)),
    ("network", |r| ms(r.recv_ns, r.encode_done_ns)),
    ("decode", |r| ms(r.decode_done_ns, r.recv_ns)),
    ("present", |r| ms(r.present_ns, r.decode_done_ns)),
    ("total", |r| ms(r.present_ns, r.capture_ns)),
];

fn ms(a: u64, b: u64) -> f64 {
    (a as i64 - b as i64) as f64 / 1e6
}

impl FrameRecord {
    pub fn to_csv(&self) -> String {
        format!(
            "{},{},{},{},{},{},{},{},{},{}",
            self.frame_no,
            self.keyframe as u8,
            self.bytes,
            self.width,
            self.height,
            self.capture_ns,
            self.encode_done_ns,
            self.recv_ns,
            self.decode_done_ns,
            self.present_ns
        )
    }

    pub fn from_csv(line: &str) -> Option<Self> {
        let f: Vec<&str> = line.trim().split(',').collect();
        if f.len() != 10 {
            return None;
        }
        Some(Self {
            frame_no: f[0].parse().ok()?,
            keyframe: f[1] == "1",
            bytes: f[2].parse().ok()?,
            width: f[3].parse().ok()?,
            height: f[4].parse().ok()?,
            capture_ns: f[5].parse().ok()?,
            encode_done_ns: f[6].parse().ok()?,
            recv_ns: f[7].parse().ok()?,
            decode_done_ns: f[8].parse().ok()?,
            present_ns: f[9].parse().ok()?,
        })
    }
}

/// Percentile (nearest-rank) of an unsorted sample.
pub fn percentile(values: &mut [f64], p: f64) -> f64 {
    if values.is_empty() {
        return f64::NAN;
    }
    values.sort_by(|a, b| a.total_cmp(b));
    let rank = ((p / 100.0) * values.len() as f64).ceil().max(1.0) as usize;
    values[rank.min(values.len()) - 1]
}

#[derive(Debug, Clone, Copy)]
pub struct Summary {
    pub n: usize,
    pub p50: f64,
    pub p95: f64,
    pub max: f64,
}

pub fn summarize(mut v: Vec<f64>) -> Summary {
    let n = v.len();
    let p50 = percentile(&mut v, 50.0);
    let p95 = percentile(&mut v, 95.0);
    let max = v.last().copied().unwrap_or(f64::NAN);
    Summary { n, p50, p95, max }
}

/// Human-readable report: per-segment P50/P95/max, fps, bitrate.
pub fn report(records: &[FrameRecord]) -> String {
    let mut out = String::new();
    if records.is_empty() {
        return "no frames\n".into();
    }
    let _ = writeln!(out, "{:>8} {:>8} {:>8} {:>8}   (ms, n={})", "segment", "P50", "P95", "max", records.len());
    for (name, f) in SEGMENTS {
        let s = summarize(records.iter().map(f).collect());
        let _ = writeln!(out, "{:>8} {:>8.2} {:>8.2} {:>8.2}", name, s.p50, s.p95, s.max);
    }
    let first = records.iter().map(|r| r.present_ns).min().unwrap();
    let last = records.iter().map(|r| r.present_ns).max().unwrap();
    let secs = (last - first) as f64 / 1e9;
    if secs > 0.0 {
        let bytes: u64 = records.iter().map(|r| r.bytes as u64).sum();
        let _ = writeln!(
            out,
            "fps {:.1}   bitrate {:.1} Mb/s   keyframes {}   resolution {}x{}",
            (records.len() - 1) as f64 / secs,
            bytes as f64 * 8.0 / secs / 1e6,
            records.iter().filter(|r| r.keyframe).count(),
            records[0].width,
            records[0].height,
        );
    }
    out
}

pub fn write_csv(w: &mut impl Write, records: &[FrameRecord]) -> std::io::Result<()> {
    writeln!(w, "{CSV_HEADER}")?;
    for r in records {
        writeln!(w, "{}", r.to_csv())?;
    }
    Ok(())
}

pub fn read_csv(r: impl BufRead) -> anyhow::Result<Vec<FrameRecord>> {
    let mut out = Vec::new();
    for line in r.lines() {
        let line = line?;
        if line.starts_with("frame_no") || line.trim().is_empty() {
            continue;
        }
        out.push(FrameRecord::from_csv(&line).ok_or_else(|| anyhow::anyhow!("bad CSV line: {line}"))?);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percentiles() {
        let mut v: Vec<f64> = (1..=100).map(|x| x as f64).collect();
        assert_eq!(percentile(&mut v, 50.0), 50.0);
        assert_eq!(percentile(&mut v, 95.0), 95.0);
        assert_eq!(percentile(&mut [3.0], 95.0), 3.0);
    }

    #[test]
    fn csv_roundtrip_and_report() {
        let recs: Vec<FrameRecord> = (0..61)
            .map(|i| {
                let c = i * 16_666_667;
                FrameRecord {
                    frame_no: i,
                    keyframe: i == 0,
                    bytes: 50_000,
                    width: 2560,
                    height: 1440,
                    capture_ns: c,
                    encode_done_ns: c + 3_000_000,
                    recv_ns: c + 4_000_000,
                    decode_done_ns: c + 6_000_000,
                    present_ns: c + 7_000_000,
                }
            })
            .collect();
        let mut buf = Vec::new();
        write_csv(&mut buf, &recs).unwrap();
        assert_eq!(read_csv(buf.as_slice()).unwrap(), recs);
        let rep = report(&recs);
        assert!(rep.contains("   total     7.00"), "{rep}");
        assert!(rep.contains("fps 60.0"), "{rep}");
    }
}
