//! VideoToolbox H.264 encoder and decoder (plan 6.5, macOS).
//!
//! Encoder: low-latency rate control, RealTime, no frame reordering, H.264 High AutoLevel,
//! "infinite" GOP (IDR only on request), input = IOSurface NV12 straight from SCK (zero-copy).
//! Decoder: output BGRA IOSurface, Metal compatible.

use std::ffi::c_void;
use std::ptr::NonNull;
use std::sync::Mutex;

use anyhow::{Result, anyhow, bail};
use objc2_core_foundation::{CFBoolean, CFDictionary, CFNumber, CFRetained, CFString, CFType};
use objc2_core_media::{
    CMBlockBuffer, CMFormatDescription, CMSampleBuffer, CMTime, CMVideoFormatDescriptionCreateFromH264ParameterSets,
    CMVideoFormatDescriptionGetH264ParameterSetAtIndex, kCMBlockBufferAssureMemoryNowFlag,
    kCMSampleAttachmentKey_NotSync, kCMTimeInvalid, kCMVideoCodecType_H264,
};
use objc2_core_video::{CVImageBuffer, kCVPixelFormatType_32BGRA};
use objc2_video_toolbox::{
    VTCompressionSession, VTDecodeFrameFlags, VTDecodeInfoFlags, VTDecompressionOutputCallbackRecord,
    VTDecompressionSession, VTEncodeInfoFlags, VTSessionSetProperty, kVTCompressionPropertyKey_AllowFrameReordering,
    kVTCompressionPropertyKey_AverageBitRate, kVTCompressionPropertyKey_ExpectedFrameRate,
    kVTCompressionPropertyKey_MaxKeyFrameInterval, kVTCompressionPropertyKey_MaxKeyFrameIntervalDuration,
    kVTCompressionPropertyKey_ProfileLevel, kVTCompressionPropertyKey_RealTime, kVTDecompressionPropertyKey_RealTime,
    kVTEncodeFrameOptionKey_ForceKeyFrame, kVTProfileLevel_H264_High_AutoLevel,
    kVTVideoEncoderSpecification_EnableLowLatencyRateControl,
    kVTVideoEncoderSpecification_RequireHardwareAcceleratedVideoEncoder,
};
use rz_capture::pixbuf::{PixelBuffer, iosurface_attributes};
use rz_capture::{Frame, FrameBuffer};
use rz_proto::clock::now_ns;

use crate::annexb::{self, NAL_PPS, NAL_SPS};
use crate::{DecodedFrame, Decoder, EncodedCallback, EncodedFrame, Encoder, EncoderConfig, EncoderOutput};

// Small CoreFoundation accessors on raw pointers (attachments are untyped CF containers).
#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    fn CFArrayGetCount(a: *const c_void) -> isize;
    fn CFArrayGetValueAtIndex(a: *const c_void, i: isize) -> *const c_void;
    fn CFDictionaryGetValue(d: *const c_void, k: *const c_void) -> *const c_void;
    fn CFBooleanGetValue(b: *const c_void) -> u8;
    fn CFBooleanGetTypeID() -> usize;
    fn CFGetTypeID(o: *const c_void) -> usize;
}

fn dict(pairs: &[(&CFString, &CFType)]) -> CFRetained<CFDictionary<CFString, CFType>> {
    let (k, v): (Vec<&CFString>, Vec<&CFType>) = pairs.iter().copied().unzip();
    CFDictionary::from_slices(&k, &v)
}

fn set_prop(session: &CFType, key: &CFString, value: &CFType) -> bool {
    let st = unsafe { VTSessionSetProperty(session, key, Some(value)) };
    if st != 0 {
        tracing::warn!("VTSessionSetProperty({key}) failed: {st}");
    }
    st == 0
}

fn ns_time(ns: u64) -> CMTime {
    unsafe { CMTime::new(ns as i64, 1_000_000_000) }
}

// ---------------------------------------------------------------- encoder

struct EncShared {
    cb: Mutex<EncodedCallback>,
}

struct FrameMeta {
    frame_no: u64,
    capture_ts_ns: u64,
    width: u32,
    height: u32,
}

pub struct VtEncoder {
    session: CFRetained<VTCompressionSession>,
    shared: *mut EncShared,
    cfg: EncoderConfig,
    warned_size: bool,
}

// SAFETY: VTCompressionSession is thread-safe for encode calls; `shared` is only touched via Mutex.
unsafe impl Send for VtEncoder {}

unsafe extern "C-unwind" fn enc_output(
    refcon: *mut c_void,
    src_refcon: *mut c_void,
    status: i32,
    _flags: VTEncodeInfoFlags,
    sb: *mut CMSampleBuffer,
) {
    let done = now_ns();
    let shared = unsafe { &*(refcon as *const EncShared) };
    let meta = unsafe { Box::from_raw(src_refcon as *mut FrameMeta) };
    let emit = |out: EncoderOutput| {
        if let Ok(mut cb) = shared.cb.lock() {
            cb(out);
        }
    };
    let dropped = EncoderOutput::Dropped { frame_no: meta.frame_no };
    if status != 0 {
        tracing::warn!(frame = meta.frame_no, "encode failed: {status}");
        return emit(dropped);
    }
    let Some(sb) = (unsafe { sb.as_ref() }) else {
        return emit(dropped); // dropped by the rate controller
    };
    match unsafe { sample_to_annexb(sb) } {
        Ok((data, keyframe)) => {
            let f = EncodedFrame {
                data,
                keyframe,
                frame_no: meta.frame_no,
                capture_ts_ns: meta.capture_ts_ns,
                encode_done_ts_ns: done,
                width: meta.width,
                height: meta.height,
            };
            emit(EncoderOutput::Frame(f));
        }
        Err(e) => {
            tracing::warn!("encoded sample unreadable: {e}");
            emit(dropped);
        }
    }
}

unsafe fn is_keyframe(sb: &CMSampleBuffer) -> bool {
    unsafe {
        let Some(arr) = sb.sample_attachments_array(false) else { return true };
        let arr = &*arr as *const _ as *const c_void;
        if CFArrayGetCount(arr) == 0 {
            return true;
        }
        let d = CFArrayGetValueAtIndex(arr, 0);
        let v = CFDictionaryGetValue(d, kCMSampleAttachmentKey_NotSync as *const CFString as *const c_void);
        if v.is_null() {
            return true;
        }
        !(CFGetTypeID(v) == CFBooleanGetTypeID() && CFBooleanGetValue(v) != 0)
    }
}

/// AVCC sample → Annex B, prepending SPS/PPS for keyframes.
unsafe fn sample_to_annexb(sb: &CMSampleBuffer) -> Result<(Vec<u8>, bool)> {
    unsafe {
        let keyframe = is_keyframe(sb);
        let bb = sb.data_buffer().ok_or_else(|| anyhow!("no data buffer"))?;
        let len = bb.data_length();
        let mut avcc = vec![0u8; len];
        let st = bb.copy_data_bytes(0, len, NonNull::new(avcc.as_mut_ptr().cast()).unwrap());
        if st != 0 {
            bail!("CMBlockBufferCopyDataBytes: {st}");
        }
        let mut out = Vec::with_capacity(len + 64);
        let mut nal_len_size = 4usize;
        if let Some(fd) = sb.format_description() {
            let mut count = 0usize;
            let mut hdr_len: i32 = 4;
            let st = CMVideoFormatDescriptionGetH264ParameterSetAtIndex(
                &fd,
                0,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                &mut count,
                &mut hdr_len,
            );
            if st == 0 {
                nal_len_size = hdr_len as usize;
                if keyframe {
                    for i in 0..count {
                        let mut p: *const u8 = std::ptr::null();
                        let mut n = 0usize;
                        if CMVideoFormatDescriptionGetH264ParameterSetAtIndex(
                            &fd,
                            i,
                            &mut p,
                            &mut n,
                            std::ptr::null_mut(),
                            std::ptr::null_mut(),
                        ) == 0
                        {
                            out.extend_from_slice(&annexb::START_CODE);
                            out.extend_from_slice(std::slice::from_raw_parts(p, n));
                        }
                    }
                }
            }
        }
        annexb::avcc_to_annexb(&avcc, nal_len_size, &mut out).map_err(|e| anyhow!(e))?;
        Ok((out, keyframe))
    }
}

impl VtEncoder {
    pub fn new(cfg: EncoderConfig, on_output: EncodedCallback) -> Result<Self> {
        let shared = Box::into_raw(Box::new(EncShared { cb: Mutex::new(on_output) }));
        let t = CFBoolean::new(true);
        let f = CFBoolean::new(false);
        // Low-latency rate control is essential: without it 4K encode latency is ~125 ms instead of ~20 ms
        // (DECISIONS.md, 2026-11-22).
        let spec = unsafe {
            dict(&[
                (kVTVideoEncoderSpecification_EnableLowLatencyRateControl, t.as_ref()),
                (kVTVideoEncoderSpecification_RequireHardwareAcceleratedVideoEncoder, t.as_ref()),
            ])
        };
        let src_attrs = iosurface_attributes(None);
        let mut out: *mut VTCompressionSession = std::ptr::null_mut();
        let st = unsafe {
            VTCompressionSession::create(
                None,
                cfg.width as i32,
                cfg.height as i32,
                kCMVideoCodecType_H264,
                Some(spec.as_opaque()),
                Some(src_attrs.as_opaque()),
                None,
                Some(enc_output),
                shared.cast(),
                NonNull::from(&mut out),
            )
        };
        if st != 0 || out.is_null() {
            drop(unsafe { Box::from_raw(shared) });
            bail!("VTCompressionSessionCreate failed: {st}");
        }
        let session = unsafe { CFRetained::from_raw(NonNull::new_unchecked(out)) };
        let s: &CFType = &session;
        unsafe {
            set_prop(s, kVTCompressionPropertyKey_RealTime, t.as_ref());
            set_prop(s, kVTCompressionPropertyKey_AllowFrameReordering, f.as_ref());
            set_prop(s, kVTCompressionPropertyKey_ProfileLevel, kVTProfileLevel_H264_High_AutoLevel.as_ref());
            set_prop(s, kVTCompressionPropertyKey_MaxKeyFrameInterval, CFNumber::new_i32(i32::MAX).as_ref());
            set_prop(s, kVTCompressionPropertyKey_MaxKeyFrameIntervalDuration, CFNumber::new_f64(0.0).as_ref());
            set_prop(s, kVTCompressionPropertyKey_ExpectedFrameRate, CFNumber::new_i32(cfg.fps as i32).as_ref());
            set_prop(
                s,
                kVTCompressionPropertyKey_AverageBitRate,
                CFNumber::new_i64(cfg.effective_bitrate_kbps() as i64 * 1000).as_ref(),
            );
            let st = session.prepare_to_encode_frames();
            if st != 0 {
                tracing::warn!("PrepareToEncodeFrames: {st}");
            }
        }
        tracing::info!(w = cfg.width, h = cfg.height, fps = cfg.fps, kbps = cfg.effective_bitrate_kbps(), "VT encoder");
        Ok(Self { session, shared, cfg, warned_size: false })
    }

    pub fn config(&self) -> EncoderConfig {
        self.cfg
    }
}

impl Encoder for VtEncoder {
    fn encode(&mut self, frame: &Frame, frame_no: u64, force_keyframe: bool) -> Result<()> {
        let owned;
        let pb: &PixelBuffer = match &frame.buffer {
            FrameBuffer::PixelBuffer(pb) => pb,
            FrameBuffer::Nv12(img) => {
                owned = PixelBuffer::from_nv12(img)?;
                &owned
            }
        };
        if (frame.width, frame.height) != (self.cfg.width, self.cfg.height) && !self.warned_size {
            tracing::warn!("frame {}x{} != encoder {}x{}", frame.width, frame.height, self.cfg.width, self.cfg.height);
            self.warned_size = true;
        }
        let meta = Box::into_raw(Box::new(FrameMeta {
            frame_no,
            capture_ts_ns: frame.capture_ts_ns,
            width: self.cfg.width,
            height: self.cfg.height,
        }));
        let t = CFBoolean::new(true);
        let props = force_keyframe.then(|| unsafe { dict(&[(kVTEncodeFrameOptionKey_ForceKeyFrame, t.as_ref())]) });
        let img: &CVImageBuffer = pb.cv();
        let st = unsafe {
            self.session.encode_frame(
                img,
                ns_time(frame.capture_ts_ns),
                CMTime::new(1, self.cfg.fps as i32),
                props.as_ref().map(|d| d.as_opaque()),
                meta.cast(),
                std::ptr::null_mut(),
            )
        };
        if st != 0 {
            // The callback is not invoked on immediate failure.
            drop(unsafe { Box::from_raw(meta) });
            bail!("VTCompressionSessionEncodeFrame: {st}");
        }
        Ok(())
    }

    fn set_bitrate_kbps(&mut self, kbps: u32) -> Result<()> {
        self.cfg.bitrate_kbps = kbps;
        let ok = unsafe {
            set_prop(
                &self.session,
                kVTCompressionPropertyKey_AverageBitRate,
                CFNumber::new_i64(kbps as i64 * 1000).as_ref(),
            )
        };
        if !ok {
            bail!("set bitrate failed");
        }
        Ok(())
    }

    fn flush(&mut self) -> Result<()> {
        let st = unsafe { self.session.complete_frames(kCMTimeInvalid) };
        if st != 0 {
            bail!("CompleteFrames: {st}");
        }
        Ok(())
    }
}

impl Drop for VtEncoder {
    fn drop(&mut self) {
        unsafe {
            self.session.complete_frames(kCMTimeInvalid);
            self.session.invalidate();
            drop(Box::from_raw(self.shared));
        }
    }
}

// ---------------------------------------------------------------- decoder

type DecodeSlot = Mutex<Option<std::result::Result<PixelBuffer, i32>>>;

unsafe extern "C-unwind" fn dec_output(
    refcon: *mut c_void,
    _src: *mut c_void,
    status: i32,
    _flags: VTDecodeInfoFlags,
    image: *mut CVImageBuffer,
    _pts: CMTime,
    _dur: CMTime,
) {
    let slot = unsafe { &*(refcon as *const DecodeSlot) };
    let r = match NonNull::new(image) {
        Some(p) if status == 0 => Ok(PixelBuffer(unsafe { CFRetained::retain(p) })),
        _ => Err(status),
    };
    if let Ok(mut s) = slot.lock() {
        *s = Some(r);
    }
}

pub struct VtDecoder {
    session: Option<CFRetained<VTDecompressionSession>>,
    format: Option<CFRetained<CMFormatDescription>>,
    sps: Vec<u8>,
    pps: Vec<u8>,
    slot: Box<DecodeSlot>,
}

// SAFETY: used from one thread at a time (`&mut self`); VT sessions are thread-safe.
unsafe impl Send for VtDecoder {}

impl Default for VtDecoder {
    fn default() -> Self {
        Self::new()
    }
}

impl VtDecoder {
    pub fn new() -> Self {
        Self { session: None, format: None, sps: Vec::new(), pps: Vec::new(), slot: Box::new(Mutex::new(None)) }
    }

    fn rebuild(&mut self) -> Result<()> {
        if let Some(s) = self.session.take() {
            unsafe {
                s.wait_for_asynchronous_frames();
                s.invalidate();
            }
        }
        let ptrs = [NonNull::new(self.sps.as_mut_ptr()).unwrap(), NonNull::new(self.pps.as_mut_ptr()).unwrap()];
        let sizes = [self.sps.len(), self.pps.len()];
        let mut fd: *const CMFormatDescription = std::ptr::null();
        let st = unsafe {
            CMVideoFormatDescriptionCreateFromH264ParameterSets(
                None,
                2,
                NonNull::from(&ptrs[0]),
                NonNull::from(&sizes[0]),
                4,
                NonNull::from(&mut fd),
            )
        };
        if st != 0 || fd.is_null() {
            bail!("CMVideoFormatDescriptionCreateFromH264ParameterSets: {st}");
        }
        let fd = unsafe { CFRetained::from_raw(NonNull::new_unchecked(fd as *mut CMFormatDescription)) };
        let attrs = iosurface_attributes(Some(kCVPixelFormatType_32BGRA));
        let rec = VTDecompressionOutputCallbackRecord {
            decompressionOutputCallback: Some(dec_output),
            decompressionOutputRefCon: (&*self.slot as *const DecodeSlot).cast_mut().cast(),
        };
        let mut out: *mut VTDecompressionSession = std::ptr::null_mut();
        let st = unsafe {
            VTDecompressionSession::create(None, &fd, None, Some(attrs.as_opaque()), &rec, NonNull::from(&mut out))
        };
        if st != 0 || out.is_null() {
            bail!("VTDecompressionSessionCreate: {st}");
        }
        let session = unsafe { CFRetained::from_raw(NonNull::new_unchecked(out)) };
        unsafe { set_prop(&session, kVTDecompressionPropertyKey_RealTime, CFBoolean::new(true).as_ref()) };
        self.session = Some(session);
        self.format = Some(fd);
        Ok(())
    }
}

impl Decoder for VtDecoder {
    fn decode(&mut self, data: &[u8]) -> Result<Option<DecodedFrame>> {
        let nals = annexb::split_nals(data);
        let mut changed = false;
        let mut vcl: Vec<&[u8]> = Vec::with_capacity(nals.len());
        for n in &nals {
            match annexb::nal_type(n) {
                NAL_SPS if *n != self.sps.as_slice() => {
                    self.sps = n.to_vec();
                    changed = true;
                }
                NAL_PPS if *n != self.pps.as_slice() => {
                    self.pps = n.to_vec();
                    changed = true;
                }
                NAL_SPS | NAL_PPS | annexb::NAL_AUD => {}
                _ => vcl.push(n),
            }
        }
        if changed && !self.sps.is_empty() && !self.pps.is_empty() {
            self.rebuild()?;
        }
        let (Some(session), Some(fd)) = (&self.session, &self.format) else { return Ok(None) };
        if vcl.is_empty() {
            return Ok(None);
        }
        let avcc = annexb::nals_to_avcc(vcl);
        let len = avcc.len();
        unsafe {
            let mut bb: *mut CMBlockBuffer = std::ptr::null_mut();
            let st = CMBlockBuffer::create_with_memory_block(
                None,
                std::ptr::null_mut(),
                len,
                None,
                std::ptr::null(),
                0,
                len,
                kCMBlockBufferAssureMemoryNowFlag,
                NonNull::from(&mut bb),
            );
            if st != 0 || bb.is_null() {
                bail!("CMBlockBufferCreateWithMemoryBlock: {st}");
            }
            let bb = CFRetained::from_raw(NonNull::new_unchecked(bb));
            let st =
                CMBlockBuffer::replace_data_bytes(NonNull::new(avcc.as_ptr() as *mut c_void).unwrap(), &bb, 0, len);
            if st != 0 {
                bail!("CMBlockBufferReplaceDataBytes: {st}");
            }
            let mut sb: *mut CMSampleBuffer = std::ptr::null_mut();
            let sizes = [len];
            let st = CMSampleBuffer::create_ready(
                None,
                Some(&bb),
                Some(fd),
                1,
                0,
                std::ptr::null(),
                1,
                sizes.as_ptr(),
                NonNull::from(&mut sb),
            );
            if st != 0 || sb.is_null() {
                bail!("CMSampleBufferCreateReady: {st}");
            }
            let sb = CFRetained::from_raw(NonNull::new_unchecked(sb));
            *self.slot.lock().unwrap() = None;
            let st = session.decode_frame(&sb, VTDecodeFrameFlags(0), std::ptr::null_mut(), std::ptr::null_mut());
            if st != 0 {
                bail!("VTDecompressionSessionDecodeFrame: {st}");
            }
            if self.slot.lock().unwrap().is_none() {
                session.wait_for_asynchronous_frames();
            }
        }
        match self.slot.lock().unwrap().take() {
            Some(Ok(pb)) => {
                let (width, height) = (pb.width(), pb.height());
                Ok(Some(DecodedFrame { pixel_buffer: pb, width, height }))
            }
            Some(Err(st)) => bail!("decode callback status {st}"),
            None => Ok(None),
        }
    }
}

impl Drop for VtDecoder {
    fn drop(&mut self) {
        if let Some(s) = self.session.take() {
            unsafe {
                s.wait_for_asynchronous_frames();
                s.invalidate();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rz_capture::testpattern::{TestPattern, decode_frame_no};
    use std::sync::{Arc, mpsc};

    /// T0.2 roundtrip: testpattern → VT encode → Annex B → VT decode → burned-in frame number matches.
    #[test]
    fn roundtrip_testpattern() {
        let (w, h) = (1280, 720);
        let (tx, rx) = mpsc::channel();
        let mut enc = VtEncoder::new(
            EncoderConfig { width: w, height: h, fps: 60, bitrate_kbps: 20_000 },
            Box::new(move |f| tx.send(f).unwrap()),
        )
        .unwrap();
        let tp = TestPattern::new(w, h, 60);
        for n in 0..30u64 {
            enc.encode(&tp.frame(n), n, n == 0).unwrap();
        }
        enc.flush().unwrap();
        drop(enc);
        let frames: Vec<EncodedFrame> = rx.iter().filter_map(EncoderOutput::frame).collect();
        assert_eq!(frames.len(), 30, "every frame encoded");
        assert!(frames[0].keyframe, "first frame is IDR");
        assert!(frames[1..].iter().all(|f| !f.keyframe), "infinite GOP: no further IDR");
        assert_eq!(annexb::nal_type(annexb::split_nals(&frames[0].data)[0]), NAL_SPS, "SPS in-band");

        let mut dec = VtDecoder::new();
        let ok = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        for f in &frames {
            let d = dec.decode(&f.data).unwrap().expect("decoded picture");
            assert_eq!((d.width, d.height), (w, h));
            let n = d.pixel_buffer.with_luma(|l| decode_frame_no(l));
            assert_eq!(n, Some(f.frame_no as u32), "burned-in frame number survives the codec");
            ok.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
        assert_eq!(ok.load(std::sync::atomic::Ordering::Relaxed), 30);
    }

    #[test]
    fn decoder_waits_for_keyframe() {
        let mut dec = VtDecoder::new();
        assert!(dec.decode(&[0, 0, 0, 1, 0x41, 0x9a, 0x00]).unwrap().is_none());
    }
}
