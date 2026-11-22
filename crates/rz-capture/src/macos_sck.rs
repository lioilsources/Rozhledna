//! ScreenCaptureKit display capture (plan 6.6, macOS). Requires Screen Recording permission.
//!
//! Config: `showsCursor=false`, `pixelFormat=420v`, `minimumFrameInterval=1/fps`, `queueDepth=5`.
//! SCK delivers frames only when content changes; idle/blank frames are skipped.

use std::sync::Mutex;
use std::sync::mpsc;
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use block2::RcBlock;
use dispatch2::{DispatchQueue, DispatchQueueAttr, DispatchRetained};
use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2::{AnyThread, DefinedClass, define_class, msg_send};
use objc2_core_graphics::{CGDisplayCopyDisplayMode, CGDisplayMode};
use objc2_core_media::{CMSampleBuffer, CMTime};
use objc2_core_video::{CVPixelBuffer, kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange};
use objc2_foundation::{NSArray, NSDictionary, NSError, NSNumber, NSObject, NSObjectProtocol, NSString};
use objc2_screen_capture_kit::{
    SCContentFilter, SCDisplay, SCFrameStatus, SCShareableContent, SCStream, SCStreamConfiguration,
    SCStreamFrameInfoDisplayTime, SCStreamFrameInfoStatus, SCStreamOutput, SCStreamOutputType,
};
use rz_proto::clock::{mach_ticks_to_ns, now_ns};

use crate::pixbuf::PixelBuffer;
use crate::{Capture, Frame, FrameBuffer, FrameCallback};

#[derive(Debug, Clone)]
pub struct DisplayInfo {
    pub id: u32,
    /// Points.
    pub width: u32,
    pub height: u32,
    /// Native pixels.
    pub pixel_width: u32,
    pub pixel_height: u32,
    pub refresh_hz: f64,
}

struct Content(Retained<SCShareableContent>);
// SAFETY: SCShareableContent is an immutable snapshot.
unsafe impl Send for Content {}

fn shareable_content() -> Result<Retained<SCShareableContent>> {
    let (tx, rx) = mpsc::channel::<Result<Content, String>>();
    let block = RcBlock::new(move |content: *mut SCShareableContent, err: *mut NSError| {
        let r = if let Some(c) = unsafe { Retained::retain(content) } {
            Ok(Content(c))
        } else if let Some(e) = unsafe { err.as_ref() } {
            Err(e.localizedDescription().to_string())
        } else {
            Err("no content and no error".into())
        };
        let _ = tx.send(r);
    });
    unsafe { SCShareableContent::getShareableContentWithCompletionHandler(&block) };
    match rx.recv_timeout(Duration::from_secs(10)) {
        Ok(Ok(c)) => Ok(c.0),
        Ok(Err(e)) => bail!("SCShareableContent: {e} (is Screen Recording permission granted?)"),
        Err(_) => bail!("SCShareableContent timed out"),
    }
}

fn display_info(d: &SCDisplay) -> DisplayInfo {
    let id = unsafe { d.displayID() };
    let mode = CGDisplayCopyDisplayMode(id);
    let (pw, ph, hz) = match &mode {
        Some(m) => (
            CGDisplayMode::pixel_width(Some(m)) as u32,
            CGDisplayMode::pixel_height(Some(m)) as u32,
            CGDisplayMode::refresh_rate(Some(m)),
        ),
        None => (0, 0, 0.0),
    };
    let (w, h) = unsafe { (d.width() as u32, d.height() as u32) };
    DisplayInfo {
        id,
        width: w,
        height: h,
        pixel_width: if pw > 0 { pw } else { w },
        pixel_height: if ph > 0 { ph } else { h },
        refresh_hz: if hz > 0.0 { hz } else { 60.0 },
    }
}

pub fn list_displays() -> Result<Vec<DisplayInfo>> {
    let content = shareable_content()?;
    let displays = unsafe { content.displays() };
    Ok(displays.iter().map(|d| display_info(&d)).collect())
}

/// Fits `(w,h)` into `(max_w,max_h)` keeping aspect ratio, never upscaling, even dimensions.
pub fn fit(w: u32, h: u32, max_w: u32, max_h: u32) -> (u32, u32) {
    crate::fit_even(w, h, max_w, max_h)
}

struct OutputIvars {
    on_frame: Mutex<FrameCallback>,
}

define_class!(
    // SAFETY: NSObject has no subclassing requirements; we implement no Drop-sensitive state.
    #[unsafe(super(NSObject))]
    #[name = "RzScStreamOutput"]
    #[ivars = OutputIvars]
    struct StreamOutput;

    unsafe impl NSObjectProtocol for StreamOutput {}

    unsafe impl SCStreamOutput for StreamOutput {
        #[unsafe(method(stream:didOutputSampleBuffer:ofType:))]
        unsafe fn did_output(&self, _stream: &SCStream, sb: &CMSampleBuffer, ty: SCStreamOutputType) {
            if ty != SCStreamOutputType::Screen {
                return;
            }
            if let Some(frame) = unsafe { frame_from_sample(sb) }
                && let Ok(mut cb) = self.ivars().on_frame.lock()
            {
                cb(frame);
            }
        }
    }
);

impl StreamOutput {
    fn new(on_frame: FrameCallback) -> Retained<Self> {
        let this = Self::alloc().set_ivars(OutputIvars { on_frame: Mutex::new(on_frame) });
        unsafe { msg_send![super(this), init] }
    }
}

/// Returns `None` for idle/blank frames (SCK sends those when nothing changed).
unsafe fn frame_from_sample(sb: &CMSampleBuffer) -> Option<Frame> {
    let mut ts = now_ns();
    unsafe {
        let attachments = sb.sample_attachments_array(false)?;
        // Toll-free bridged: CFArray<CFDictionary> == NSArray<NSDictionary>.
        let arr: &NSArray<NSDictionary<NSString, objc2::runtime::AnyObject>> = &*(&*attachments
            as *const objc2_core_foundation::CFArray
            as *const NSArray<NSDictionary<NSString, objc2::runtime::AnyObject>>);
        let info = arr.firstObject()?;
        let status = info.objectForKey(SCStreamFrameInfoStatus)?;
        let status: &NSNumber = status.downcast_ref()?;
        if SCFrameStatus(status.integerValue()) != SCFrameStatus::Complete {
            return None;
        }
        if let Some(t) = info.objectForKey(SCStreamFrameInfoDisplayTime).and_then(|t| t.downcast::<NSNumber>().ok()) {
            ts = mach_ticks_to_ns(t.unsignedLongLongValue());
        }
        let img = sb.image_buffer()?;
        // CVImageBuffer from SCK is a CVPixelBuffer.
        let pb: objc2_core_foundation::CFRetained<CVPixelBuffer> = img;
        let pb = PixelBuffer(pb);
        let (width, height) = (pb.width(), pb.height());
        Some(Frame { capture_ts_ns: ts, width, height, buffer: FrameBuffer::PixelBuffer(pb) })
    }
}

#[derive(Debug, Clone)]
pub struct WindowInfo {
    pub id: u32,
    pub title: String,
    pub app_name: String,
    pub bundle_id: String,
    pub pid: i32,
    /// Points, global coordinates.
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
    pub layer: i64,
    pub on_screen: bool,
    pub active: bool,
}

fn window_info(w: &objc2_screen_capture_kit::SCWindow) -> WindowInfo {
    unsafe {
        let app = w.owningApplication();
        let f = w.frame();
        WindowInfo {
            id: w.windowID(),
            title: w.title().map(|t| t.to_string()).unwrap_or_default(),
            app_name: app.as_ref().map(|a| a.applicationName().to_string()).unwrap_or_default(),
            bundle_id: app.as_ref().map(|a| a.bundleIdentifier().to_string()).unwrap_or_default(),
            pid: app.as_ref().map(|a| a.processID()).unwrap_or(0),
            x: f.origin.x,
            y: f.origin.y,
            width: f.size.width,
            height: f.size.height,
            layer: w.windowLayer() as i64,
            on_screen: w.isOnScreen(),
            active: w.isActive(),
        }
    }
}

/// All windows SCK knows about (including off-screen ones).
pub fn list_windows() -> Result<Vec<WindowInfo>> {
    let content = shareable_content()?;
    let windows = unsafe { content.windows() };
    Ok(windows.iter().map(|w| window_info(&w)).collect())
}

/// Extra knobs, mainly for probes (T0.7).
#[derive(Debug, Clone, Copy)]
pub struct ScOptions {
    /// `420v` for the encoder (default) or `BGRA` for inspection.
    pub pixel_format: u32,
    /// `SCStreamConfiguration.includeChildWindows` (menus, sheets, popovers); `None` = system default.
    pub include_child_windows: Option<bool>,
    pub shows_cursor: bool,
    /// `minimumFrameInterval` = 1/N s. `None` = 1/fps, `Some(0)` = no limit (kCMTimeZero).
    pub min_interval_hz: Option<u32>,
}

impl Default for ScOptions {
    fn default() -> Self {
        Self {
            pixel_format: kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange,
            include_child_windows: None,
            shows_cursor: false,
            min_interval_hz: None,
        }
    }
}

/// Captures one display or one window.
pub struct ScCapture {
    filter: Retained<SCContentFilter>,
    label: String,
    width: u32,
    height: u32,
    fps: u32,
    opts: ScOptions,
    stream: Option<Retained<SCStream>>,
    output: Option<Retained<StreamOutput>>,
    queue: DispatchRetained<DispatchQueue>,
    display: Option<DisplayInfo>,
}

/// Display capture (kept as a name for call sites from Phase 0).
pub type ScDisplayCapture = ScCapture;

// SAFETY: SCStream/SCContentFilter are used only via thread-safe methods (start/stop/update).
unsafe impl Send for ScCapture {}

impl ScCapture {
    /// `display_id = None` → main display. Output is scaled to fit `max_w × max_h`.
    pub fn new(display_id: Option<u32>, max_w: u32, max_h: u32, fps: u32) -> Result<Self> {
        Self::display(display_id, max_w, max_h, fps, ScOptions::default())
    }

    pub fn display(display_id: Option<u32>, max_w: u32, max_h: u32, fps: u32, opts: ScOptions) -> Result<Self> {
        let content = shareable_content()?;
        let displays = unsafe { content.displays() };
        let display = match display_id {
            Some(id) => displays.iter().find(|d| unsafe { d.displayID() } == id),
            None => {
                let main = objc2_core_graphics::CGMainDisplayID();
                displays.iter().find(|d| unsafe { d.displayID() } == main).or_else(|| displays.firstObject())
            }
        }
        .context("display not found")?;
        let info = display_info(&display);
        let (width, height) = fit(info.pixel_width, info.pixel_height, max_w, max_h);
        let filter = unsafe {
            SCContentFilter::initWithDisplay_excludingWindows(SCContentFilter::alloc(), &display, &NSArray::new())
        };
        let label = format!("display {} ({}x{}@{:.0})", info.id, info.pixel_width, info.pixel_height, info.refresh_hz);
        Ok(Self::with_filter(filter, label, width, height, fps, opts, Some(info)))
    }

    /// `SCContentFilter(desktopIndependentWindow:)`. `scale` = backing scale (2.0 on Retina).
    pub fn window(window_id: u32, scale: f64, max_w: u32, max_h: u32, fps: u32, opts: ScOptions) -> Result<Self> {
        let content = shareable_content()?;
        let windows = unsafe { content.windows() };
        let w = windows.iter().find(|w| unsafe { w.windowID() } == window_id).context("window not found")?;
        let info = window_info(&w);
        let (width, height) = fit((info.width * scale) as u32 & !1, (info.height * scale) as u32 & !1, max_w, max_h);
        let filter = unsafe { SCContentFilter::initWithDesktopIndependentWindow(SCContentFilter::alloc(), &w) };
        let label = format!("window {} '{}' ({})", info.id, info.title, info.app_name);
        Ok(Self::with_filter(filter, label, width.max(2), height.max(2), fps, opts, None))
    }

    fn with_filter(
        filter: Retained<SCContentFilter>,
        label: String,
        width: u32,
        height: u32,
        fps: u32,
        opts: ScOptions,
        display: Option<DisplayInfo>,
    ) -> Self {
        let queue = DispatchQueue::new("rozhledna.sck", DispatchQueueAttr::SERIAL);
        Self { filter, label, width, height, fps, opts, stream: None, output: None, queue, display }
    }

    /// Display info (display captures only).
    pub fn display_info(&self) -> Option<&DisplayInfo> {
        self.display.as_ref()
    }

    fn config(&self) -> Retained<SCStreamConfiguration> {
        unsafe {
            let c = SCStreamConfiguration::new();
            c.setWidth(self.width as usize);
            c.setHeight(self.height as usize);
            c.setPixelFormat(self.opts.pixel_format);
            let interval = match self.opts.min_interval_hz.unwrap_or(self.fps) {
                0 => CMTime::new(0, 1),
                hz => CMTime::new(1, hz as i32),
            };
            c.setMinimumFrameInterval(interval);
            c.setQueueDepth(5);
            c.setShowsCursor(self.opts.shows_cursor);
            if let Some(v) = self.opts.include_child_windows {
                c.setIncludeChildWindows(v);
            }
            c
        }
    }
}

fn wait_completion(what: &str, f: impl FnOnce(&block2::DynBlock<dyn Fn(*mut NSError)>)) -> Result<()> {
    let (tx, rx) = mpsc::channel::<Option<String>>();
    let block = RcBlock::new(move |err: *mut NSError| {
        let _ = tx.send(unsafe { err.as_ref() }.map(|e| e.localizedDescription().to_string()));
    });
    f(&block);
    match rx.recv_timeout(Duration::from_secs(10)) {
        Ok(None) => Ok(()),
        Ok(Some(e)) => Err(anyhow!("{what}: {e}")),
        Err(_) => Err(anyhow!("{what}: timed out")),
    }
}

impl Capture for ScCapture {
    fn start(&mut self, on_frame: FrameCallback) -> Result<()> {
        anyhow::ensure!(self.stream.is_none(), "already started");
        let config = self.config();
        let stream =
            unsafe { SCStream::initWithFilter_configuration_delegate(SCStream::alloc(), &self.filter, &config, None) };
        let output = StreamOutput::new(on_frame);
        unsafe {
            stream
                .addStreamOutput_type_sampleHandlerQueue_error(
                    ProtocolObject::from_ref(&*output),
                    SCStreamOutputType::Screen,
                    Some(&self.queue),
                )
                .map_err(|e| anyhow!("addStreamOutput: {}", e.localizedDescription()))?;
        }
        wait_completion("startCapture", |b| unsafe { stream.startCaptureWithCompletionHandler(Some(b)) })?;
        tracing::info!(source = self.label, w = self.width, h = self.height, fps = self.fps, "SCK capture started");
        self.stream = Some(stream);
        self.output = Some(output);
        Ok(())
    }

    fn stop(&mut self) {
        if let Some(s) = self.stream.take() {
            let _ = wait_completion("stopCapture", |b| unsafe { s.stopCaptureWithCompletionHandler(Some(b)) });
        }
        self.output = None;
    }

    fn size(&self) -> (u32, u32) {
        (self.width, self.height)
    }
}

impl Drop for ScCapture {
    fn drop(&mut self) {
        self.stop();
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn fit_keeps_aspect_and_never_upscales() {
        assert_eq!(super::fit(5120, 2880, 2560, 1440), (2560, 1440));
        assert_eq!(super::fit(1920, 1080, 3840, 2160), (1920, 1080));
        assert_eq!(super::fit(3024, 1964, 1920, 1080), (1662, 1080));
    }
}

#[link(name = "CoreGraphics", kind = "framework")]
unsafe extern "C" {
    fn CGPreflightScreenCaptureAccess() -> bool;
}
#[link(name = "ApplicationServices", kind = "framework")]
unsafe extern "C" {
    fn AXIsProcessTrusted() -> u8;
}

/// TCC state without triggering any prompt: (Screen Recording, Accessibility).
///
/// Note: when started from a terminal, macOS attributes permissions to the terminal app
/// (the "responsible process"), not to this binary. Start the signed .app bundle with `open` to get
/// the bundle's own identity.
pub fn permissions() -> (bool, bool) {
    unsafe { (CGPreflightScreenCaptureAccess(), AXIsProcessTrusted() != 0) }
}
