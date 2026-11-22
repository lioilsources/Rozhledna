//! Source registry (T1.2: displays only). The source id of a display is its `CGDirectDisplayID`.

use anyhow::{Result, bail};
use rz_capture::Capture;
use rz_capture::testpattern::TestPattern;
use rz_proto::pb;

/// Where frames come from.
#[derive(Clone, Debug)]
pub enum Backend {
    /// ScreenCaptureKit (needs Screen Recording).
    Sck {
        /// SCK `minimumFrameInterval` = 1/N s; `None` = 2×fps (LATENCY.md, experiment interval).
        interval_hz: Option<u32>,
    },
    /// `count` synthetic displays of `w×h` side by side (tests, development without permissions).
    Testpattern { w: u32, h: u32, count: u32 },
}

pub struct Registry {
    backend: Backend,
}

/// What the capture should produce.
#[derive(Clone, Copy, Debug)]
pub struct CaptureParams {
    pub max_w: u32,
    pub max_h: u32,
    pub fps: u32,
}

impl Registry {
    pub fn new(backend: Backend) -> Self {
        Self { backend }
    }

    pub fn backend(&self) -> &Backend {
        &self.backend
    }

    pub fn list(&self) -> Result<Vec<pb::Source>> {
        match &self.backend {
            Backend::Testpattern { w, h, count } => Ok((0..*count)
                .map(|i| pb::Source {
                    id: i as u64 + 1,
                    kind: pb::SourceKind::Display as i32,
                    title: format!("Testpattern {}", i + 1),
                    rect: Some(pb::Rect { x: (i * w) as f64, y: 0.0, w: *w as f64, h: *h as f64 }),
                    scale: 1.0,
                    native_w: *w,
                    native_h: *h,
                    refresh_hz: 60.0,
                    on_screen: true,
                    ..Default::default()
                })
                .collect()),
            #[cfg(target_os = "macos")]
            Backend::Sck { .. } => {
                let mut v = Vec::new();
                for d in rz_capture::macos_sck::list_displays()? {
                    v.push(pb::Source {
                        id: d.id as u64,
                        kind: pb::SourceKind::Display as i32,
                        title: format!("Display {} ({}×{})", d.id, d.pixel_width, d.pixel_height),
                        display_id: d.id as u64,
                        rect: Some(crate::status::display_bounds(d.id)),
                        scale: if d.width > 0 { d.pixel_width as f64 / d.width as f64 } else { 1.0 },
                        native_w: d.pixel_width,
                        native_h: d.pixel_height,
                        refresh_hz: d.refresh_hz,
                        on_screen: true,
                        ..Default::default()
                    });
                }
                Ok(v)
            }
            #[cfg(not(target_os = "macos"))]
            Backend::Sck { .. } => bail!("ScreenCaptureKit is macOS only"),
        }
    }

    /// Opens a capture of a display. The capture is not started.
    pub fn open(&self, source_id: u64, p: CaptureParams) -> Result<Box<dyn Capture>> {
        match &self.backend {
            Backend::Testpattern { w, h, count } => {
                if source_id == 0 || source_id > *count as u64 {
                    bail!("unknown source {source_id}");
                }
                let (w, h) = rz_capture::fit_even(*w, *h, p.max_w, p.max_h);
                if w < rz_capture::testpattern::MIN_WIDTH {
                    bail!("testpattern needs width >= {} (got {w})", rz_capture::testpattern::MIN_WIDTH);
                }
                Ok(Box::new(TestPattern::new(w, h, p.fps)))
            }
            #[cfg(target_os = "macos")]
            Backend::Sck { interval_hz } => {
                use rz_capture::macos_sck::{ScCapture, ScOptions};
                let opts = ScOptions { min_interval_hz: Some(interval_hz.unwrap_or(2 * p.fps)), ..Default::default() };
                let id = u32::try_from(source_id).map_err(|_| anyhow::anyhow!("unknown source {source_id}"))?;
                Ok(Box::new(ScCapture::display(Some(id), p.max_w, p.max_h, p.fps, opts)?))
            }
            #[cfg(not(target_os = "macos"))]
            Backend::Sck { .. } => bail!("ScreenCaptureKit is macOS only"),
        }
    }
}
