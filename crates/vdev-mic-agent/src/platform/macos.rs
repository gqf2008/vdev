//! CoreAudio HAL backend — the actual D3-D4 loop.
//!
//! Three pieces, all of them userland, none of them needing a new driver:
//!
//! ```text
//!   physical mic ──▶ AudioUnit(HALOutput, input)  ──▶ 480-sample frames
//!                                                      │
//!                                            RNNoise + adaptive dry/wet
//!                                                      │
//!                                            [SPSC ring, lock-free]
//!                                                      │
//!   "vdev-audio A" ──▶ AudioDeviceIOProc (output scope) ──┘
//!        │
//!        └─ HAL plugin loops output into its own input stream
//!               │
//!               └─▶ any conference app picks "vdev-audio A" as its microphone
//! ```
//!
//! Why this shape:
//!
//! * **The plugin needs no change.** `vdev-audio` is already a loopback device
//!   (output stream -> ring -> input stream, `ring_write_out` /
//!   `ring_read_in`). Playing into its output is therefore enough to feed its
//!   input, which is what `test_loopback.sh` verifies from userland. The agent
//!   is just another CoreAudio client.
//! * **`AudioUnit` for capture, `AudioDeviceIOProc` for injection.** Capture
//!   needs sample-rate/channel conversion from whatever the hardware runs at
//!   (44100, stereo, whatever), and the HAL output AU does that for free.
//!   Injection must land in the plugin's own format (8ch, 48 kHz, f32,
//!   interleaved) with no conversion and no extra buffering -- an IOProc hands
//!   us exactly that buffer.
//! * **The model runs inside the capture callback.** 0.08 ms of a 10 ms budget
//!   (D1-D2) leaves plenty of headroom, and running inline removes a whole
//!   ring buffer + thread from the latency budget.
//!
//! Real-time rules obeyed by every callback in this file: no allocation, no
//! `println!`, no blocking lock (`try_lock` on the one shared queue, and only
//! after a detection has already happened), no unwinding.

use super::{LiveConfig, ProbeMode};
use crate::frames::{DelayLine, FrameAssembler};
use crate::latency::{detect_with, marker, LatencyProbe, MARKER_LEN};
use crate::mixer::AdaptiveMixer;
use crate::proctime::cpu_seconds;
use crate::ring::SpscRing;
use crate::rnnoise::{Denoiser, Engine, FRAME};
use anyhow::{anyhow, bail, Context, Result};
use serde::Serialize;
use std::collections::VecDeque;
use std::ffi::c_void;
use std::os::raw::c_char;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

// ===========================================================================
// FFI
// ===========================================================================

/// The bindings are declared in full, as a binding layer should be: an
/// unused selector here is one call away, and trimming it to exactly today's
/// call sites would make the next change harder, not safer.
#[allow(dead_code)]
mod ffi {
    use std::ffi::c_void;
    use std::os::raw::c_char;

    pub type OSStatus = i32;
    pub type AudioObjectID = u32;
    pub type AudioDeviceID = u32;
    pub type AudioUnit = *mut c_void;
    pub type CFStringRef = *const c_void;
    pub type CFAllocatorRef = *const c_void;
    pub type AudioDeviceIOProcID = *mut c_void;

    pub const SCOPE_GLOBAL: u32 = 0x676c_6f62; // 'glob'
    pub const SCOPE_INPUT: u32 = 0x696e_7074; // 'inpt'
    pub const SCOPE_OUTPUT: u32 = 0x6f75_7470; // 'outp'
    pub const ELEM_MAIN: u32 = 0;

    pub const SYSTEM_OBJECT: AudioObjectID = 1;

    // AudioHardware / AudioObject selectors (four-char codes, same ones the
    // plugin itself uses in `props.rs`).
    pub const HW_DEVICES: u32 = 0x6465_7623; // 'dev#'
    pub const HW_DEFAULT_INPUT: u32 = 0x6449_6e20; // 'dIn '
    pub const HW_DEFAULT_OUTPUT: u32 = 0x644f_7574; // 'dOut'
    pub const HW_TRANSLATE_UID_TO_DEVICE: u32 = 0x7569_6464; // 'uidd'
    pub const OBJ_NAME: u32 = 0x6c6e_616d; // 'lnam'
    pub const OBJ_MANUFACTURER: u32 = 0x6c6d_616b; // 'lmak'
    pub const DEV_UID: u32 = 0x7569_6420; // 'uid '
    pub const DEV_NOMINAL_SR: u32 = 0x6e73_7274; // 'nsrt'
    pub const DEV_BUFFER_FRAMES: u32 = 0x6673_697a; // 'fsiz'
    pub const DEV_LATENCY: u32 = 0x6c74_6e63; // 'ltnc'
    pub const DEV_SAFETY_OFFSET: u32 = 0x7361_6674; // 'saft'
    pub const DEV_TRANSPORT: u32 = 0x7472_616e; // 'tran'
    pub const DEV_IS_ALIVE: u32 = 0x6c69_766e; // 'livn'
    pub const DEV_STREAMS: u32 = 0x7374_6d23; // 'stm#'
    pub const STREAM_VIRTUAL_FORMAT: u32 = 0x7366_6d74; // 'sfmt'

    // AudioUnit property IDs are small integers, not four-char codes.
    pub const AU_PROP_CURRENT_DEVICE: u32 = 2000;
    pub const AU_PROP_ENABLE_IO: u32 = 2003;
    pub const AU_PROP_SET_INPUT_CALLBACK: u32 = 2005;
    pub const AU_PROP_STREAM_FORMAT: u32 = 8;
    pub const AU_PROP_MAX_FRAMES_PER_SLICE: u32 = 14;
    pub const AU_PROP_SET_RENDER_CALLBACK: u32 = 23;
    // AudioUnit scopes are also small integers.
    pub const AU_SCOPE_GLOBAL: u32 = 0;
    pub const AU_SCOPE_INPUT: u32 = 1;
    pub const AU_SCOPE_OUTPUT: u32 = 2;

    pub const AU_TYPE_OUTPUT: u32 = 0x6175_6f75; // 'auou'
    pub const AU_SUBTYPE_HAL_OUTPUT: u32 = 0x6168_616c; // 'ahal'
    pub const AU_MFR_APPLE: u32 = 0x6170_706c; // 'appl'

    pub const FORMAT_LINEAR_PCM: u32 = 0x6c70_636d; // 'lpcm'
    pub const FLAG_IS_FLOAT: u32 = 1 << 0;
    pub const FLAG_IS_PACKED: u32 = 1 << 3;
    pub const FLAG_IS_NON_INTERLEAVED: u32 = 1 << 5;

    pub const CF_UTF8: u32 = 0x0800_0100;

    #[repr(C)]
    #[derive(Clone, Copy)]
    pub struct AudioObjectPropertyAddress {
        pub m_selector: u32,
        pub m_scope: u32,
        pub m_element: u32,
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    pub struct AudioStreamBasicDescription {
        pub m_sample_rate: f64,
        pub m_format_id: u32,
        pub m_format_flags: u32,
        pub m_bytes_per_packet: u32,
        pub m_frames_per_packet: u32,
        pub m_bytes_per_frame: u32,
        pub m_channels_per_frame: u32,
        pub m_bits_per_channel: u32,
        pub m_reserved: u32,
    }

    #[repr(C)]
    pub struct AudioBuffer {
        pub m_number_channels: u32,
        pub m_data_byte_size: u32,
        pub m_data: *mut c_void,
    }

    /// Variable-length in C; only `mBuffers[0]` is ever accessed here, and the
    /// device does not hand out multi-buffer lists for interleaved formats.
    #[repr(C)]
    pub struct AudioBufferList {
        pub m_number_buffers: u32,
        pub m_buffers: [AudioBuffer; 1],
    }

    #[repr(C)]
    pub struct AudioTimeStamp {
        pub m_sample_time: f64,
        pub m_host_time: u64,
        pub m_rate_scalar: f64,
        pub m_word_clock_time: u64,
        pub m_flags: u32,
        pub m_reserved: u32,
    }

    #[repr(C)]
    pub struct AudioComponentDescription {
        pub component_type: u32,
        pub component_sub_type: u32,
        pub component_manufacturer: u32,
        pub component_flags: u32,
        pub component_flags_mask: u32,
    }

    pub type AudioUnitRenderCallback = unsafe extern "C" fn(
        in_ref_con: *mut c_void,
        io_action_flags: *mut u32,
        in_time_stamp: *const AudioTimeStamp,
        in_bus_number: u32,
        in_number_frames: u32,
        io_data: *mut AudioBufferList,
    ) -> OSStatus;

    #[repr(C)]
    pub struct AURenderCallbackStruct {
        pub input_proc: Option<AudioUnitRenderCallback>,
        pub input_proc_ref_con: *mut c_void,
    }

    pub type AudioDeviceIOProc = unsafe extern "C" fn(
        in_device: AudioObjectID,
        in_now: *const AudioTimeStamp,
        in_input_data: *const AudioBufferList,
        in_input_time: *const AudioTimeStamp,
        out_output_data: *mut AudioBufferList,
        in_output_time: *const AudioTimeStamp,
        in_client_data: *mut c_void,
    ) -> OSStatus;

    #[link(name = "CoreFoundation", kind = "framework")]
    extern "C" {
        pub fn CFStringCreateWithCString(
            alloc: CFAllocatorRef,
            cstr: *const c_char,
            encoding: u32,
        ) -> CFStringRef;
        pub fn CFStringGetCString(
            s: CFStringRef,
            buf: *mut c_char,
            buf_size: isize,
            encoding: u32,
        ) -> u8;
        pub fn CFRelease(cf: *const c_void);
    }

    #[link(name = "CoreAudio", kind = "framework")]
    extern "C" {
        pub fn AudioObjectGetPropertyDataSize(
            obj: AudioObjectID,
            addr: *const AudioObjectPropertyAddress,
            qualifier_size: u32,
            qualifier: *const c_void,
            out_size: *mut u32,
        ) -> OSStatus;
        pub fn AudioObjectGetPropertyData(
            obj: AudioObjectID,
            addr: *const AudioObjectPropertyAddress,
            qualifier_size: u32,
            qualifier: *const c_void,
            io_size: *mut u32,
            out_data: *mut c_void,
        ) -> OSStatus;
        pub fn AudioObjectSetPropertyData(
            obj: AudioObjectID,
            addr: *const AudioObjectPropertyAddress,
            qualifier_size: u32,
            qualifier: *const c_void,
            data_size: u32,
            data: *const c_void,
        ) -> OSStatus;

        pub fn AudioComponentFindNext(
            in_component: *mut c_void,
            in_desc: *const AudioComponentDescription,
        ) -> *mut c_void;
        pub fn AudioComponentInstanceNew(
            in_component: *mut c_void,
            out_instance: *mut AudioUnit,
        ) -> OSStatus;
        pub fn AudioComponentInstanceDispose(instance: AudioUnit) -> OSStatus;
        pub fn AudioUnitSetProperty(
            unit: AudioUnit,
            prop: u32,
            scope: u32,
            element: u32,
            data: *const c_void,
            size: u32,
        ) -> OSStatus;
        pub fn AudioUnitGetProperty(
            unit: AudioUnit,
            prop: u32,
            scope: u32,
            element: u32,
            data: *mut c_void,
            size: *mut u32,
        ) -> OSStatus;
        pub fn AudioUnitInitialize(unit: AudioUnit) -> OSStatus;
        pub fn AudioUnitUninitialize(unit: AudioUnit) -> OSStatus;
        pub fn AudioOutputUnitStart(unit: AudioUnit) -> OSStatus;
        pub fn AudioOutputUnitStop(unit: AudioUnit) -> OSStatus;

        pub fn AudioDeviceCreateIOProcID(
            dev: AudioDeviceID,
            proc_: AudioDeviceIOProc,
            client_data: *mut c_void,
            out_id: *mut AudioDeviceIOProcID,
        ) -> OSStatus;
        pub fn AudioDeviceDestroyIOProcID(dev: AudioDeviceID, id: AudioDeviceIOProcID) -> OSStatus;
        pub fn AudioDeviceStart(dev: AudioDeviceID, id: AudioDeviceIOProcID) -> OSStatus;
        pub fn AudioDeviceStop(dev: AudioDeviceID, id: AudioDeviceIOProcID) -> OSStatus;
    }
}

use ffi::*;

// ===========================================================================
// Device helpers
// ===========================================================================

fn addr(selector: u32, scope: u32) -> AudioObjectPropertyAddress {
    AudioObjectPropertyAddress {
        m_selector: selector,
        m_scope: scope,
        m_element: ELEM_MAIN,
    }
}

/// Render an OSStatus the way CoreAudio developers read it: as `'abcd'` when
/// the 32-bit value's big-endian bytes are all printable ASCII, else as the
/// plain number (`'!pnc'` says more than `561665635` does).
fn os_status(rc: OSStatus) -> String {
    let bytes = rc.to_be_bytes();
    if bytes.iter().all(|b| (0x20..0x7f).contains(b)) {
        format!("'{}'", bytes.iter().map(|&b| b as char).collect::<String>())
    } else {
        rc.to_string()
    }
}

fn cf_string(s: &str) -> CFStringRef {
    let c = std::ffi::CString::new(s).unwrap_or_default();
    // SAFETY: `c` is a valid NUL-terminated C string; a null return (OOM) is
    // handled by the callers' `is_null` checks.
    unsafe { CFStringCreateWithCString(std::ptr::null(), c.as_ptr(), CF_UTF8) }
}

fn cf_to_string(cf: CFStringRef) -> Option<String> {
    if cf.is_null() {
        return None;
    }
    let mut buf = [0 as c_char; 256];
    // SAFETY: `cf` is a live CFString and `buf` is a valid 256-byte buffer.
    let ok = unsafe { CFStringGetCString(cf, buf.as_mut_ptr(), buf.len() as isize, CF_UTF8) };
    if ok == 0 {
        return None;
    }
    // SAFETY: CFStringGetCString returned 1, so `buf` holds a NUL-terminated string.
    Some(
        unsafe { std::ffi::CStr::from_ptr(buf.as_ptr()) }
            .to_string_lossy()
            .into_owned(),
    )
}

/// Copy-out a fixed-size property (u32 / f64 / AudioStreamBasicDescription...).
fn get_prop<T: Copy>(obj: AudioObjectID, a: &AudioObjectPropertyAddress) -> Option<T> {
    let mut size = std::mem::size_of::<T>() as u32;
    let mut out = std::mem::MaybeUninit::<T>::zeroed();
    // SAFETY: `out` is a sized MaybeUninit and `size` declares exactly its
    // extent, so CoreAudio writes at most size_of::<T>() bytes.
    let rc = unsafe {
        AudioObjectGetPropertyData(
            obj,
            a,
            0,
            std::ptr::null(),
            &mut size,
            out.as_mut_ptr() as *mut c_void,
        )
    };
    if rc != 0 || size as usize != std::mem::size_of::<T>() {
        return None;
    }
    // SAFETY: `rc == 0` and the written size matches, so the value is fully
    // initialized (and zero-initialized before the call regardless).
    Some(unsafe { out.assume_init() })
}

fn get_cf_prop(obj: AudioObjectID, a: &AudioObjectPropertyAddress) -> Option<String> {
    let mut size = std::mem::size_of::<CFStringRef>() as u32;
    let mut cf: CFStringRef = std::ptr::null();
    // SAFETY: `cf` is a valid out-slot of exactly the declared `size`; on
    // success CoreAudio has filled it (or left it null) and +1'd the refcount.
    let rc = unsafe {
        AudioObjectGetPropertyData(
            obj,
            a,
            0,
            std::ptr::null(),
            &mut size,
            &mut cf as *mut CFStringRef as *mut c_void,
        )
    };
    if rc != 0 {
        return None;
    }
    let s = cf_to_string(cf);
    if !cf.is_null() {
        // SAFETY: `cf` came from a copy-out getter (+1 retain) and is non-null;
        // release it exactly once.
        unsafe { CFRelease(cf) };
    }
    s
}

fn get_array<T: Copy>(obj: AudioObjectID, a: &AudioObjectPropertyAddress) -> Vec<T> {
    let mut size = 0u32;
    // SAFETY: plain copy-out size query; the only out-parameter is `size`.
    let rc = unsafe { AudioObjectGetPropertyDataSize(obj, a, 0, std::ptr::null(), &mut size) };
    if rc != 0 || size == 0 {
        return Vec::new();
    }
    let n = size as usize / std::mem::size_of::<T>();
    if n == 0 {
        return Vec::new();
    }
    let mut v: Vec<T> = Vec::with_capacity(n);
    // SAFETY: `v` has capacity for `n` elements, the exact extent the size
    // query authorized; CoreAudio writes at most that many bytes.
    let rc = unsafe {
        AudioObjectGetPropertyData(
            obj,
            a,
            0,
            std::ptr::null(),
            &mut size,
            v.as_mut_ptr() as *mut c_void,
        )
    };
    if rc != 0 {
        return Vec::new();
    }
    // The call rewrites `size` with the bytes it actually wrote, which can be
    // smaller than the query-time value (the device changed between the two
    // calls). Only the fully covered prefix is initialized, so size the Vec
    // from that — never from the query-time `n` — and clamp against a
    // misbehaving over-report.
    let written = (size as usize / std::mem::size_of::<T>()).min(n);
    // SAFETY: `written` <= capacity, and the first `written` elements were
    // initialized by the GetPropertyData call above.
    unsafe { v.set_len(written) };
    v
}

fn all_devices() -> Vec<AudioDeviceID> {
    get_array::<AudioDeviceID>(SYSTEM_OBJECT, &addr(HW_DEVICES, SCOPE_GLOBAL))
}

fn default_input_device() -> Option<AudioDeviceID> {
    let d: AudioDeviceID = get_prop(SYSTEM_OBJECT, &addr(HW_DEFAULT_INPUT, SCOPE_GLOBAL))?;
    (d != 0).then_some(d)
}

fn default_output_device() -> Option<AudioDeviceID> {
    let d: AudioDeviceID = get_prop(SYSTEM_OBJECT, &addr(HW_DEFAULT_OUTPUT, SCOPE_GLOBAL))?;
    (d != 0).then_some(d)
}

fn translate_uid(uid: &str) -> Option<AudioDeviceID> {
    let cf = cf_string(uid);
    let a = addr(HW_TRANSLATE_UID_TO_DEVICE, SCOPE_GLOBAL);
    let mut dev: AudioDeviceID = 0;
    let mut size = std::mem::size_of::<AudioDeviceID>() as u32;
    // SAFETY: `dev` is a valid out-slot of the declared `size`, and `cf` is a
    // live CFString serving as the qualifier.
    let rc = unsafe {
        AudioObjectGetPropertyData(
            SYSTEM_OBJECT,
            &a,
            std::mem::size_of::<CFStringRef>() as u32,
            &cf as *const CFStringRef as *const c_void,
            &mut size,
            &mut dev as *mut AudioDeviceID as *mut c_void,
        )
    };
    if !cf.is_null() {
        // SAFETY: non-null CFString from `cf_string`; release the one reference.
        unsafe { CFRelease(cf) };
    }
    (rc == 0 && dev != 0).then_some(dev)
}

fn device_name(dev: AudioDeviceID) -> String {
    get_cf_prop(dev, &addr(OBJ_NAME, SCOPE_GLOBAL)).unwrap_or_else(|| format!("<device {dev}>"))
}

fn device_uid(dev: AudioDeviceID) -> String {
    get_cf_prop(dev, &addr(DEV_UID, SCOPE_GLOBAL)).unwrap_or_default()
}

fn transport_name(dev: AudioDeviceID) -> String {
    let t: u32 = match get_prop(dev, &addr(DEV_TRANSPORT, SCOPE_GLOBAL)) {
        Some(v) => v,
        None => return "?".into(),
    };
    let s: Vec<u8> = t.to_be_bytes().to_vec();
    String::from_utf8_lossy(&s).into_owned()
}

/// Look a device up by UID first (exact, unambiguous), then by name / UID
/// substring (handy on the command line).
fn find_device(needle: &str) -> Option<AudioDeviceID> {
    if let Some(d) = translate_uid(needle) {
        return Some(d);
    }
    let lower = needle.to_lowercase();
    all_devices().into_iter().find(|&d| {
        device_name(d).to_lowercase().contains(&lower)
            || device_uid(d).to_lowercase().contains(&lower)
    })
}

/// Channel count of the device's first stream in the given scope.
fn stream_channels(dev: AudioDeviceID, scope: u32) -> u32 {
    let streams: Vec<AudioObjectID> = get_array(dev, &addr(DEV_STREAMS, scope));
    for s in streams {
        if let Some(asbd) =
            get_prop::<AudioStreamBasicDescription>(s, &addr(STREAM_VIRTUAL_FORMAT, SCOPE_GLOBAL))
        {
            if asbd.m_channels_per_frame > 0 {
                return asbd.m_channels_per_frame;
            }
        }
    }
    0
}

#[derive(Debug, Serialize)]
pub struct DeviceInfo {
    pub id: AudioDeviceID,
    pub name: String,
    pub uid: String,
    pub transport: String,
    pub sample_rate: f64,
    pub buffer_frames: u32,
    pub latency_frames: u32,
    pub safety_offset_frames: u32,
    pub input_channels: u32,
    pub output_channels: u32,
    /// Sum of the device's own reported latency, in ms -- the datasheet number,
    /// useful only to compare against what the probe actually measures.
    pub reported_latency_ms: f64,
}

fn describe(dev: AudioDeviceID) -> DeviceInfo {
    let sr: f64 = get_prop(dev, &addr(DEV_NOMINAL_SR, SCOPE_GLOBAL)).unwrap_or(0.0);
    let buffer_frames: u32 = get_prop(dev, &addr(DEV_BUFFER_FRAMES, SCOPE_GLOBAL)).unwrap_or(0);
    let latency_frames: u32 = get_prop(dev, &addr(DEV_LATENCY, SCOPE_GLOBAL)).unwrap_or(0);
    let safety_offset_frames: u32 =
        get_prop(dev, &addr(DEV_SAFETY_OFFSET, SCOPE_GLOBAL)).unwrap_or(0);
    let frames_ms = |f: u32| {
        if sr > 0.0 {
            f as f64 / sr * 1000.0
        } else {
            0.0
        }
    };
    DeviceInfo {
        id: dev,
        name: device_name(dev),
        uid: device_uid(dev),
        transport: transport_name(dev),
        sample_rate: sr,
        buffer_frames,
        latency_frames,
        safety_offset_frames,
        input_channels: stream_channels(dev, SCOPE_INPUT),
        output_channels: stream_channels(dev, SCOPE_OUTPUT),
        reported_latency_ms: frames_ms(latency_frames)
            + frames_ms(safety_offset_frames)
            + frames_ms(buffer_frames),
    }
}

// ===========================================================================
// The pipeline core (runs inside the capture callback)
// ===========================================================================

/// How much audio we are willing to record for the post-run A/B files. Fixed at
/// startup because the audio thread may not allocate.
const RECORD_SECONDS: usize = 120;
const RECORD_SAMPLES: usize = 48_000 * RECORD_SECONDS;

struct Core {
    adaptive: bool,
    mix: f32,

    den: Denoiser,
    mixer: AdaptiveMixer,
    dry: DelayLine,
    out_ring: Arc<SpscRing>,

    /// One 480-sample frame, the buffer that goes to the virtual mic.
    frame: Vec<f32>,
    /// Dry frame, delayed in place before blending.
    dry_frame: Vec<f32>,

    rec_in: Vec<f32>,
    rec_out: Vec<f32>,
    timing: Vec<f64>,

    frames: u64,
    vad_sum: f64,
    wet_sum: f64,
    wet_min: f64,
    wet_max: f64,
}

impl Core {
    fn new(engine: &Engine, cfg: &LiveConfig, out_ring: Arc<SpscRing>) -> Result<Self> {
        let den = engine.denoiser()?;
        let lag = engine.frame_size * 2; // RNNoise lookahead: 2 frames = 20 ms
        let adaptive = cfg.adaptive && cfg.probe.is_none();
        let mix = if cfg.probe == Some(ProbeMode::Acoustic) {
            // The acoustic probe puts a chirp through the whole chain; the model
            // would treat it as noise and gate it away. Bypassing the dry path
            // *with the lookahead delay* keeps the measured timing identical
            // (the delay line takes the place of the model's 20 ms) while the
            // marker survives intact.
            0.0
        } else {
            cfg.mix
        };
        let mut rec_in = Vec::with_capacity(RECORD_SAMPLES);
        rec_in.resize(RECORD_SAMPLES, 0.0);
        rec_in.clear();
        let mut rec_out = Vec::with_capacity(RECORD_SAMPLES);
        rec_out.resize(RECORD_SAMPLES, 0.0);
        rec_out.clear();
        Ok(Self {
            adaptive,
            mix,
            den,
            mixer: AdaptiveMixer::new(adaptive),
            dry: DelayLine::new(lag),
            out_ring,
            frame: vec![0.0; engine.frame_size],
            dry_frame: vec![0.0; engine.frame_size],
            rec_in,
            rec_out,
            timing: Vec::with_capacity(60 * 1000), // 60 s of 10 ms frames
            frames: 0,
            vad_sum: 0.0,
            wet_sum: 0.0,
            wet_min: f64::MAX,
            wet_max: f64::MIN,
        })
    }

    /// One 480-sample frame: model, blend, ship. Mirrors `agent::process`'s
    /// inner loop exactly, so the offline numbers stay valid.
    #[inline]
    fn on_frame(&mut self, input: &[f32]) {
        // 1) raw capture, for the A/B file
        if self.rec_in.len() + FRAME <= RECORD_SAMPLES {
            self.rec_in.extend_from_slice(input);
        }

        let t0 = Instant::now();
        let (vad, wet_frame) = self.den.process(input);
        let dt_ms = t0.elapsed().as_secs_f64() * 1000.0;
        if self.timing.len() < self.timing.capacity() {
            self.timing.push(dt_ms);
        }

        // 2) dry path, delayed to the model's lookahead (see frames::DelayLine)
        self.dry_frame.copy_from_slice(input);
        self.dry.process(&mut self.dry_frame);

        // 3) blend
        let w = if self.adaptive {
            self.mixer.update(input, vad)
        } else {
            self.mix as f64
        };
        if w >= 1.0 {
            self.frame.copy_from_slice(wet_frame);
        } else if w <= 0.0 {
            self.frame.copy_from_slice(&self.dry_frame);
        } else {
            // frame/dry_frame are exactly `FRAME` long (`Engine::load` bails
            // unless frame_size == FRAME) and `wet_frame` likewise, so this
            // zips over the same samples the old `0..FRAME` loop indexed.
            for ((out, &wet), &dry) in self.frame.iter_mut().zip(wet_frame).zip(&self.dry_frame) {
                *out = (w * wet as f64 + (1.0 - w) * dry as f64) as f32;
            }
        }

        if self.rec_out.len() + FRAME <= RECORD_SAMPLES {
            self.rec_out.extend_from_slice(&self.frame);
        }

        // 4) ship it. Dropping on a full ring is the right failure mode: the
        // render side is starving, and stale audio would be worse than a gap.
        self.out_ring.push_or_drop(&self.frame);

        self.frames += 1;
        self.vad_sum += vad as f64;
        self.wet_sum += w;
        self.wet_min = self.wet_min.min(w);
        self.wet_max = self.wet_max.max(w);
    }
}

// ===========================================================================
// Capture callback
// ===========================================================================

/// OSStatus handed back to CoreAudio when a callback catches a panic: `'!pnc'`.
/// Only contract is "not `noErr`"; the four-char form renders as itself via
/// [`os_status`].
const CB_PANIC: OSStatus = 0x2170_6e63u32 as i32; // '!pnc'

/// Report a panic caught in an audio callback exactly once. These callbacks
/// run at hundreds of Hz, so a persistently panicking one would otherwise
/// flood stderr; one line is all a log reader needs.
fn note_cb_panic(cb: &'static str) {
    static LOGGED: AtomicBool = AtomicBool::new(false);
    if !LOGGED.swap(true, Ordering::Relaxed) {
        eprintln!("vdev-mic-agent: panic caught in {cb} callback; handing error to CoreAudio (further reports suppressed)");
    }
}

/// AU input callback (the physical mic's HAL thread → `Core::on_frame`).
///
/// The body runs under [`catch_unwind`]: a panic must not unwind across the C
/// boundary (the HAL would abort the whole process), so we report `'!pnc'`
/// instead. `AssertUnwindSafe` is honest here — the closure only touches the
/// raw pointers CoreAudio handed us for this one call, plus state (`Core`,
/// `ASM`) that is deliberately process-lifetime and single-thread.
unsafe extern "C" fn capture_input_cb(
    in_ref_con: *mut c_void,
    _io_action_flags: *mut u32,
    _in_time_stamp: *const AudioTimeStamp,
    _in_bus_number: u32,
    in_number_frames: u32,
    io_data: *mut AudioBufferList,
) -> OSStatus {
    let caught = catch_unwind(AssertUnwindSafe(|| -> OSStatus {
        if in_ref_con.is_null() || io_data.is_null() {
            return 0;
        }
        // SAFETY: non-null `in_ref_con` is the `core_ptr` we registered with
        // the AU (Box::into_raw, process lifetime); the HAL runs exactly one
        // callback at a time on its own thread.
        let ctx = unsafe { &mut *(in_ref_con as *mut Core) };
        // SAFETY: `io_data` is the HAL-owned buffer list, valid for this call.
        let abl = unsafe { &*io_data };
        if abl.m_number_buffers == 0 {
            return 0;
        }
        let buf = &abl.m_buffers[0];
        if buf.m_data.is_null() || in_number_frames == 0 {
            return 0;
        }
        // Client format was declared mono, non-interleaved, f32 (see
        // `build_capture_unit`), so this buffer is exactly nframes f32 values.
        let n = in_number_frames as usize;
        // SAFETY: `buf.m_data` is non-null and holds `n` f32 samples per the
        // declared client format above.
        let mono = unsafe { std::slice::from_raw_parts(buf.m_data as *const f32, n) };

        // (--record-in capture happens in `Core::on_frame`, on clean 480-sample
        // frame boundaries; writing here too would double the WAV.)

        // SAFETY: `ASM` is touched by exactly this one audio thread, and the
        // call is synchronous (see `AsmHolder`).
        let asm = unsafe { &mut *ASM.get() };
        asm.push(mono, |frame| ctx.on_frame(frame));
        0
    }));
    match caught {
        Ok(rc) => rc,
        Err(_) => {
            note_cb_panic("capture_input");
            CB_PANIC
        }
    }
}

/// The assembler is touched by exactly one audio thread. `static mut` behind a
/// pointer is the honest expression of that; it is never shared.
struct AsmHolder(std::cell::UnsafeCell<FrameAssembler>);
// SAFETY: `AsmHolder` is only ever reached from the single HAL audio thread
// (see the `ASM.get()` call in `capture_input_cb`); no other thread touches it.
unsafe impl Sync for AsmHolder {}

impl AsmHolder {
    /// Only the (single) audio thread ever calls this; that is the contract the
    /// `unsafe impl Sync` above encodes.
    fn get(&self) -> *mut FrameAssembler {
        self.0.get()
    }
}

static ASM: AsmHolder = AsmHolder(std::cell::UnsafeCell::new(FrameAssembler::new()));

// ===========================================================================
// Injection callback (on the virtual device)
// ===========================================================================

struct InjectCtx {
    out_ring: Arc<SpscRing>,
    /// mono staging for the render side (preallocated; the callback never grows it)
    mono: Vec<f32>,
    /// mono staging for the probe's input scan
    scan: Vec<f32>,
    blocks: u64,
    starved_before: usize,

    probe: bool,
    /// Digital probe only: the marker source. `None` means the render side is
    /// fed from the ring (live denoise / acoustic probe).
    emit: Option<MarkerEmitter>,
    marker: Vec<f32>,
    /// rolling correlation window (preallocated)
    search: Vec<f32>,
    /// NCC prefix-sum scratch for `detect_with` (preallocated: `clear()`ed and
    /// reused on the hit path, never grown on this RT thread)
    prefix_buf: Vec<f64>,
    pending: Arc<Mutex<VecDeque<Instant>>>,
    latency: LatencyProbe,
    /// Correlation threshold: a digital loopback is bit-exact, an acoustic one
    /// is not.
    threshold: f32,
}

const RENDER_SCRATCH: usize = 8192;
const SEARCH_CAP: usize = 48_000; // 1 s: covers a 300 ms marker cadence with room to spare

/// IOProc on the vdev virtual device: render (ring → device) and, in probe
/// mode, marker detection in the same call.
///
/// Wrapped in [`catch_unwind`] like [`capture_input_cb`]: a panic must not
/// unwind into CoreAudio, so we report `'!pnc'` instead. `AssertUnwindSafe`
/// covers only the pointers CoreAudio hands us plus the process-lifetime
/// `InjectCtx` — see the leak note at its creation in `run`.
unsafe extern "C" fn inject_ioproc(
    _in_device: AudioObjectID,
    _in_now: *const AudioTimeStamp,
    in_input_data: *const AudioBufferList,
    _in_input_time: *const AudioTimeStamp,
    out_output_data: *mut AudioBufferList,
    _in_output_time: *const AudioTimeStamp,
    in_client_data: *mut c_void,
) -> OSStatus {
    let caught = catch_unwind(AssertUnwindSafe(|| -> OSStatus {
        if in_client_data.is_null() {
            return 0;
        }
        // SAFETY: non-null `in_client_data` is the (leaked, process-lifetime)
        // `InjectCtx` we registered via `AudioDeviceCreateIOProcID`; the HAL
        // serializes IOProc calls per device.
        let ctx = unsafe { &mut *(in_client_data as *mut InjectCtx) };
        ctx.blocks += 1;

        // ---- 1) render: fill the device's output buffer from the ring ----------
        if !out_output_data.is_null() {
            // SAFETY: `out_output_data` is the HAL-owned buffer list to fill,
            // valid for this call.
            let abl = unsafe { &mut *out_output_data };
            if abl.m_number_buffers > 0 {
                let buf = &mut abl.m_buffers[0];
                let nch = buf.m_number_channels.max(1) as usize;
                let frames = (buf.m_data_byte_size as usize) / (4 * nch);
                if !buf.m_data.is_null() && frames > 0 {
                    let frames = frames.min(RENDER_SCRATCH);
                    // SAFETY: `m_data` is non-null with room for
                    // `m_data_byte_size / (4*nch)` f32 values (≥ `frames`).
                    let dst = unsafe {
                        std::slice::from_raw_parts_mut(buf.m_data as *mut f32, frames * nch)
                    };
                    // The virtual device is 8-channel and its input stream is also
                    // 8-channel: duplicate the mono signal across all of them so
                    // every consumer channel map hears it.
                    if let Some(emit) = &mut ctx.emit {
                        // Digital probe: we *are* the source. The instant the
                        // marker leaves is the instant the round trip starts, so
                        // timestamp here rather than on a feeder thread.
                        for f in 0..frames {
                            let (v, started) = emit.next();
                            if started {
                                if let Ok(mut q) = ctx.pending.try_lock() {
                                    q.push_back(Instant::now());
                                }
                            }
                            let base = f * nch;
                            for c in 0..nch {
                                dst[base + c] = v;
                            }
                        }
                    } else {
                        let n = ctx.out_ring.pop_or_silence(&mut ctx.mono[..frames]);
                        ctx.starved_before += frames - n;
                        for f in 0..frames {
                            let v = ctx.mono[f];
                            let base = f * nch;
                            for c in 0..nch {
                                dst[base + c] = v;
                            }
                        }
                    }
                }
            }
        }

        // ---- 2) probe: look for the marker coming back ------------------------
        if ctx.probe && !in_input_data.is_null() {
            // SAFETY: `in_input_data` is the HAL-owned input buffer list,
            // valid for this call.
            let abl = unsafe { &*in_input_data };
            if abl.m_number_buffers > 0 {
                let buf = &abl.m_buffers[0];
                let nch = buf.m_number_channels.max(1) as usize;
                let frames = (buf.m_data_byte_size as usize) / (4 * nch);
                if !buf.m_data.is_null() && frames > 0 {
                    let frames = frames.min(ctx.scan.len());
                    // SAFETY: `m_data` is non-null and holds
                    // `m_data_byte_size / (4*nch)` f32 values (≥ `frames`).
                    let src = unsafe {
                        std::slice::from_raw_parts(buf.m_data as *const f32, frames * nch)
                    };
                    for f in 0..frames {
                        ctx.scan[f] = src[f * nch]; // channel 0 carries the mono signal
                    }
                    if ctx.search.len() + frames > SEARCH_CAP {
                        let drop = ctx.search.len() + frames - SEARCH_CAP;
                        ctx.search.drain(..drop);
                    }
                    ctx.search.extend_from_slice(&ctx.scan[..frames]);

                    if ctx.search.len() >= MARKER_LEN {
                        // Preallocated scratch: clear() keeps the capacity, so
                        // detect_with's reserve() never has to allocate here.
                        ctx.prefix_buf.clear();
                        if let Some(d) = detect_with(
                            &ctx.search,
                            &ctx.marker,
                            ctx.threshold,
                            &mut ctx.prefix_buf,
                        ) {
                            // match the earliest un-matched injection
                            let sent = ctx.pending.try_lock().ok().and_then(|mut q| q.pop_front());
                            match sent {
                                Some(sent) => ctx.latency.record(sent, Instant::now()),
                                None => ctx.latency.record_rejected(),
                            }
                            // do not re-report the same occurrence
                            let consumed = (d.start + MARKER_LEN).min(ctx.search.len());
                            ctx.search.drain(..consumed);
                        }
                    }
                }
            }
        }
        0
    }));
    match caught {
        Ok(rc) => rc,
        Err(_) => {
            note_cb_panic("inject_ioproc");
            CB_PANIC
        }
    }
}

// ===========================================================================
// Marker emitter (digital probe: into the ring; acoustic probe: out the speaker)
// ===========================================================================

/// The digital probe's marker source: a sample-by-sample state machine the
/// render callback drives, so no thread, no ring and no scheduling hop sits
/// between "marker emitted" and "marker measured".
///
/// The HAL plugin loops this stream straight back into its own input stream,
/// which is the same IOProc's `in_input_data` -- so injection and detection
/// happen in one callback and the delta is pure device + plugin buffering.
struct MarkerEmitter {
    marker: Vec<f32>,
    /// Position inside the marker; `>= marker.len()` means "idle".
    pos: usize,
    /// Frames left before the next marker starts.
    until_next: u64,
    /// Marker-to-marker spacing; never shorter than the marker itself.
    period_frames: u64,
}

impl MarkerEmitter {
    fn new(period_frames: u64) -> Self {
        let m = marker();
        let n = m.len();
        Self {
            marker: m,
            pos: n,
            until_next: 0,
            period_frames: period_frames.max(MARKER_LEN as u64),
        }
    }

    /// The next sample to hand the device, and whether a fresh marker starts
    /// here (in which case *this* frame is the round trip's zero).
    #[inline]
    fn next(&mut self) -> (f32, bool) {
        if self.pos >= self.marker.len() {
            if self.until_next == 0 {
                self.pos = 0;
            } else {
                self.until_next -= 1;
                return (0.0, false);
            }
        }
        let v = self.marker[self.pos];
        let started = self.pos == 0;
        self.pos += 1;
        if self.pos == self.marker.len() {
            self.until_next = self.period_frames;
        }
        (v, started)
    }
}

// ===========================================================================
// Speaker output (acoustic probe)
// ===========================================================================

struct SpeakerCtx {
    marker: Vec<f32>,
    pos: usize,
    until_next: u64,
    period_frames: u64,
    channels: usize,
    pending: Arc<Mutex<VecDeque<Instant>>>,
}

impl SpeakerCtx {
    /// The next sample to put on the speaker, and — when a fresh marker starts
    /// on this sample — the instant that sample leaves: the acoustic round
    /// trip's zero. Same machine as [`MarkerEmitter::next`]: play the marker
    /// out once, then keep `period_frames` of silence before the next one.
    /// The timestamp is taken on the marker's *first* sample, before the
    /// position advances.
    #[inline]
    fn next(&mut self) -> (f32, Option<Instant>) {
        if self.pos >= self.marker.len() {
            if self.until_next == 0 {
                self.pos = 0;
            } else {
                self.until_next -= 1;
                return (0.0, None);
            }
        }
        let started = self.pos == 0;
        let ts = started.then(Instant::now);
        let v = self.marker[self.pos];
        self.pos += 1;
        if self.pos == self.marker.len() {
            self.until_next = self.period_frames;
        }
        (v, ts)
    }
}

/// AU render callback for the physical speaker (acoustic probe marker source).
///
/// Wrapped in [`catch_unwind`] like [`capture_input_cb`]: a panic must not
/// unwind into CoreAudio, so we report `'!pnc'` instead. `AssertUnwindSafe`
/// covers only the pointers CoreAudio hands us plus the process-lifetime
/// `SpeakerCtx` (leaked at creation in `run`).
unsafe extern "C" fn speaker_render_cb(
    in_ref_con: *mut c_void,
    _io_action_flags: *mut u32,
    _in_time_stamp: *const AudioTimeStamp,
    _in_bus_number: u32,
    in_number_frames: u32,
    io_data: *mut AudioBufferList,
) -> OSStatus {
    let caught = catch_unwind(AssertUnwindSafe(|| -> OSStatus {
        if in_ref_con.is_null() || io_data.is_null() {
            return 0;
        }
        // SAFETY: non-null `in_ref_con` is the `ctx_ptr` we registered with the
        // AU (Box::into_raw, process lifetime); the AU serializes render calls.
        let ctx = unsafe { &mut *(in_ref_con as *mut SpeakerCtx) };
        // SAFETY: `io_data` is the AU-owned buffer list, valid for this call.
        let abl = unsafe { &mut *io_data };
        if abl.m_number_buffers == 0 || in_number_frames == 0 {
            return 0;
        }
        let frames = in_number_frames as usize;
        let nch = ctx.channels.max(1);

        // Silence first: the marker is the only thing we ever send.
        for b in 0..abl.m_number_buffers as usize {
            // SAFETY: `m_buffers` really holds `m_number_buffers` entries for
            // interleaved buffer lists (see the `AudioBufferList` doc).
            let buf = unsafe { &mut *abl.m_buffers.as_mut_ptr().add(b) };
            if !buf.m_data.is_null() {
                // SAFETY: HAL-owned `m_data` is valid for `m_data_byte_size`.
                unsafe {
                    std::ptr::write_bytes(buf.m_data as *mut u8, 0, buf.m_data_byte_size as usize)
                };
            }
        }

        for f in 0..frames {
            let (v, ts) = ctx.next();
            if let Some(zero) = ts {
                // first sample of a fresh marker: this is the round trip's zero
                if let Ok(mut q) = ctx.pending.try_lock() {
                    q.push_back(zero);
                }
            }
            for b in 0..abl.m_number_buffers as usize {
                // SAFETY: `m_buffers` really holds `m_number_buffers` entries
                // (as above).
                let buf = unsafe { &mut *abl.m_buffers.as_mut_ptr().add(b) };
                if buf.m_data.is_null() {
                    continue;
                }
                let bch = if buf.m_number_channels == 0 {
                    nch
                } else {
                    buf.m_number_channels as usize
                };
                // SAFETY: `m_data` is non-null and holds `frames * bch` f32
                // samples (interleaved, per the registered client format).
                let dst =
                    unsafe { std::slice::from_raw_parts_mut(buf.m_data as *mut f32, frames * bch) };
                let base = f * bch;
                for c in 0..bch {
                    dst[base + c] = v;
                }
            }
        }
        0
    }));
    match caught {
        Ok(rc) => rc,
        Err(_) => {
            note_cb_panic("speaker_render");
            CB_PANIC
        }
    }
}

// ===========================================================================
// AudioUnit construction
// ===========================================================================

fn asbd(rate: f64, channels: u32, non_interleaved: bool) -> AudioStreamBasicDescription {
    let bytes_per_frame = 4 * if non_interleaved { 1 } else { channels };
    AudioStreamBasicDescription {
        m_sample_rate: rate,
        m_format_id: FORMAT_LINEAR_PCM,
        m_format_flags: FLAG_IS_FLOAT
            | FLAG_IS_PACKED
            | if non_interleaved {
                FLAG_IS_NON_INTERLEAVED
            } else {
                0
            },
        m_bytes_per_packet: bytes_per_frame,
        m_frames_per_packet: 1,
        m_bytes_per_frame: bytes_per_frame,
        m_channels_per_frame: channels,
        m_bits_per_channel: 32,
        m_reserved: 0,
    }
}

fn new_hal_unit() -> Result<AudioUnit> {
    let desc = AudioComponentDescription {
        component_type: AU_TYPE_OUTPUT,
        component_sub_type: AU_SUBTYPE_HAL_OUTPUT,
        component_manufacturer: AU_MFR_APPLE,
        component_flags: 0,
        component_flags_mask: 0,
    };
    // SAFETY: `desc` is a fully initialized AudioComponentDescription; a null
    // return just means "no such component" and is handled below.
    let comp = unsafe { AudioComponentFindNext(std::ptr::null_mut(), &desc) };
    if comp.is_null() {
        bail!("no HAL output AudioUnit (kAudioUnitSubType_HALOutput) available");
    }
    let mut unit: AudioUnit = std::ptr::null_mut();
    // SAFETY: `desc` is a valid initialized description and `unit` a valid
    // out-slot; `comp` came from FindNext, so it is a live component.
    let rc = unsafe { AudioComponentInstanceNew(comp, &mut unit) };
    if rc != 0 || unit.is_null() {
        bail!(
            "AudioComponentInstanceNew failed (OSStatus {})",
            os_status(rc)
        );
    }
    Ok(unit)
}

fn au_set<T>(
    unit: AudioUnit,
    prop: u32,
    scope: u32,
    element: u32,
    v: &T,
    what: &str,
) -> Result<()> {
    // SAFETY: `v` is a valid `T` and the size passed is exactly its extent,
    // which is the contract of every scalar AudioUnit property setter.
    let rc = unsafe {
        AudioUnitSetProperty(
            unit,
            prop,
            scope,
            element,
            v as *const T as *const c_void,
            std::mem::size_of::<T>() as u32,
        )
    };
    if rc != 0 {
        bail!(
            "AudioUnitSetProperty({what}) failed (OSStatus {})",
            os_status(rc)
        );
    }
    Ok(())
}

/// Capture AU: hardware input of `device` -> our callback, mono f32 @ 48 kHz.
fn build_capture_unit(device: AudioDeviceID, core: *mut Core) -> Result<AudioUnit> {
    let unit = new_hal_unit()?;
    let one: u32 = 1;
    let zero: u32 = 0;

    // input element (bus 1) on, output element (bus 0) off
    au_set(
        unit,
        AU_PROP_ENABLE_IO,
        AU_SCOPE_INPUT,
        1,
        &one,
        "EnableIO(input)",
    )?;
    au_set(
        unit,
        AU_PROP_ENABLE_IO,
        AU_SCOPE_OUTPUT,
        0,
        &zero,
        "EnableIO(output)",
    )?;
    au_set(
        unit,
        AU_PROP_CURRENT_DEVICE,
        AU_SCOPE_GLOBAL,
        0,
        &device,
        "CurrentDevice",
    )?;

    // Client side of the input element: mono, f32, non-interleaved. The HAL
    // converts from whatever the hardware actually runs at, which is why we use
    // an AudioUnit here and a raw IOProc on the injection side.
    let fmt = asbd(48_000.0, 1, true);
    au_set(
        unit,
        AU_PROP_STREAM_FORMAT,
        AU_SCOPE_OUTPUT,
        1,
        &fmt,
        "StreamFormat(input client)",
    )?;

    // Buffer the biggest slice we are prepared to handle; the device can go
    // down to 32 frames and we must not care.
    let max_frames: u32 = 4096;
    au_set(
        unit,
        AU_PROP_MAX_FRAMES_PER_SLICE,
        AU_SCOPE_GLOBAL,
        0,
        &max_frames,
        "MaximumFramesPerSlice",
    )?;

    let cb = AURenderCallbackStruct {
        input_proc: Some(capture_input_cb),
        input_proc_ref_con: core as *mut c_void,
    };
    au_set(
        unit,
        AU_PROP_SET_INPUT_CALLBACK,
        AU_SCOPE_GLOBAL,
        0,
        &cb,
        "SetInputCallback",
    )?;

    // SAFETY: `unit` is a live HAL-output AudioUnit from `new_hal_unit`, and
    // all its properties have been set; Initialize is its one-time start.
    let rc = unsafe { AudioUnitInitialize(unit) };
    if rc != 0 {
        bail!(
            "AudioUnitInitialize(capture) failed (OSStatus {})",
            os_status(rc)
        );
    }
    Ok(unit)
}

/// Speaker AU (acoustic probe only): play the marker on the *physical* output.
fn build_speaker_unit(
    device: AudioDeviceID,
    channels: u32,
    ctx: *mut SpeakerCtx,
) -> Result<AudioUnit> {
    let unit = new_hal_unit()?;
    let one: u32 = 1;
    let zero: u32 = 0;
    au_set(
        unit,
        AU_PROP_ENABLE_IO,
        AU_SCOPE_OUTPUT,
        0,
        &one,
        "EnableIO(output)",
    )?;
    au_set(
        unit,
        AU_PROP_ENABLE_IO,
        AU_SCOPE_INPUT,
        1,
        &zero,
        "EnableIO(input)",
    )?;
    au_set(
        unit,
        AU_PROP_CURRENT_DEVICE,
        AU_SCOPE_GLOBAL,
        0,
        &device,
        "CurrentDevice",
    )?;

    // Interleaved f32 at 48 kHz on the client side; the AU converts to the
    // hardware format (this is the element/scopes pair that means "what the app
    // gives the unit").
    let fmt = asbd(48_000.0, channels, false);
    au_set(
        unit,
        AU_PROP_STREAM_FORMAT,
        AU_SCOPE_INPUT,
        0,
        &fmt,
        "StreamFormat(output client)",
    )?;

    let cb = AURenderCallbackStruct {
        input_proc: Some(speaker_render_cb),
        input_proc_ref_con: ctx as *mut c_void,
    };
    au_set(
        unit,
        AU_PROP_SET_RENDER_CALLBACK,
        AU_SCOPE_GLOBAL,
        0,
        &cb,
        "SetRenderCallback",
    )?;

    // SAFETY: `unit` is a live HAL-output AudioUnit from `new_hal_unit`, and
    // all its properties have been set; Initialize is its one-time start.
    let rc = unsafe { AudioUnitInitialize(unit) };
    if rc != 0 {
        bail!(
            "AudioUnitInitialize(speaker) failed (OSStatus {})",
            os_status(rc)
        );
    }
    Ok(unit)
}

// ===========================================================================
// Report
// ===========================================================================

#[derive(Debug, Serialize)]
struct RunReport {
    mode: String,
    seconds_requested: f64,
    sample_rate: u32,
    frame_samples: usize,
    frame_ms: f64,
    engine: Option<String>,
    engine_state_bytes: Option<usize>,
    adaptive: bool,
    mix: f32,
    vdev: DeviceInfo,
    capture: Option<DeviceInfo>,
    speaker: Option<DeviceInfo>,
    /// Frames the model actually processed.
    frames: u64,
    /// Audio seconds the model produced (frames * 10 ms).
    audio_seconds: f64,
    /// Wall seconds the graph was running (fills the gap between the two).
    wall_seconds: f64,
    vad_mean: f64,
    wet_ratio_mean: f64,
    wet_ratio_min: f64,
    wet_ratio_max: f64,
    /// Per-frame model time, p50/p95/p99/max, in ms.
    frame_ms_p50: f64,
    frame_ms_p95: f64,
    frame_ms_p99: f64,
    frame_ms_max: f64,
    /// Samples the ring dropped (model faster than the render side) ...
    ring_dropped_samples: usize,
    /// ... and samples the render side had to pad with silence (model late).
    ring_starved_samples: usize,
    cpu_seconds: f64,
    cpu_percent_of_one_core: f64,
    latency: Option<LatencyStatsOut>,
}

#[derive(Debug, Serialize)]
struct LatencyStatsOut {
    count: usize,
    injected: u64,
    detected: u64,
    rejected: u64,
    min_ms: f64,
    mean_ms: f64,
    p50_ms: f64,
    p95_ms: f64,
    max_ms: f64,
    stdev_ms: f64,
    /// `digital`: add the model lookahead (20 ms) to get the model total.
    /// `acoustic`: already end to end, including the model's delay line.
    interpretation: String,
}

fn percentile(sorted: &[f64], q: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    sorted[((sorted.len() as f64 - 1.0) * q).round() as usize]
}

// ===========================================================================
// Entry point
// ===========================================================================

pub fn run(cfg: LiveConfig) -> Result<()> {
    let vdev = find_device(&cfg.vdev).ok_or_else(|| {
        anyhow!(
            "virtual device {:?} not found. Is vdev-audio.driver installed?\n\
             Build and install it from the vdev checkout:\n    make -C crates/vdev-audio install\n\
             Then check `vdev-audio-ctl` prints the device, and that this crate is not\n\
             running inside a sandbox that hides /Library/Audio/Plug-Ins/HAL.",
            cfg.vdev
        )
    })?;
    let vdev_info = describe(vdev);

    let capture_dev = match &cfg.input {
        Some(n) => Some(find_device(n).ok_or_else(|| anyhow!("capture device {n:?} not found"))?),
        None => default_input_device(),
    };
    let speaker_dev = if cfg.probe == Some(ProbeMode::Acoustic) {
        default_output_device()
    } else {
        None
    };

    println!(
        "vdev device : #{} \"{}\"  uid={}",
        vdev_info.id, vdev_info.name, vdev_info.uid
    );
    println!(
        "              {:.0} Hz, buffer {} frames, {} ch out / {} ch in, reported latency {:.2} ms",
        vdev_info.sample_rate,
        vdev_info.buffer_frames,
        vdev_info.output_channels,
        vdev_info.input_channels,
        vdev_info.reported_latency_ms
    );
    if let Some(d) = capture_dev {
        let i = describe(d);
        println!("capture     : #{} \"{}\"  {:.0} Hz, buffer {} frames, {} ch in, reported latency {:.2} ms",
            i.id, i.name, i.sample_rate, i.buffer_frames, i.input_channels, i.reported_latency_ms);
    } else {
        println!("capture     : (none -- digital probe only)");
    }
    if let Some(d) = speaker_dev {
        let i = describe(d);
        println!(
            "speaker     : #{} \"{}\"  {:.0} Hz, {} ch out",
            i.id, i.name, i.sample_rate, i.output_channels
        );
    }

    // ---- engine (not needed for the digital probe) ------------------------
    let engine = match cfg.probe {
        Some(ProbeMode::Digital) => None,
        _ => Some(Engine::load(cfg.dll.as_deref())?),
    };
    if let Some(e) = &engine {
        println!(
            "engine      : {}  ({} B state/stream)",
            e.path.display(),
            e.state_bytes
        );
    }

    let frame_ms = if let Some(e) = &engine {
        e.frame_size as f64 / 48.0
    } else {
        10.0
    };
    let out_ring = Arc::new(SpscRing::new(48_000)); // ~1.4 s of slack (rounded up to 64 Ki samples)
    let pending: Arc<Mutex<VecDeque<Instant>>> = Arc::new(Mutex::new(VecDeque::new()));
    let probe_mode = cfg.probe;
    let is_probe = probe_mode.is_some();

    let probe_period_frames = (cfg.probe_interval_ms.max(50) as f64 * 48.0) as u64;

    // ---- injection side ---------------------------------------------------
    // LEAK BY DESIGN (do not "fix"): the IOProc's client pointer must outlive
    // not just this scope but CoreAudio's last callback. A panic past
    // `AudioDeviceStart` (e.g. the stdout pipe going away in the loop below)
    // would otherwise unwind-drop the Box while the IOProc may still be
    // invoked — a use-after-free inside the HAL. The process exits right
    // after either way, so leaking one InjectCtx is strictly better. This
    // Box is never freed; same trade as `core_ptr`/`ctx_ptr` below.
    let inject: &'static mut InjectCtx = Box::leak(Box::new(InjectCtx {
        out_ring: Arc::clone(&out_ring),
        mono: vec![0.0; RENDER_SCRATCH],
        scan: vec![0.0; RENDER_SCRATCH],
        blocks: 0,
        starved_before: 0,
        probe: is_probe,
        emit: (probe_mode == Some(ProbeMode::Digital))
            .then(|| MarkerEmitter::new(probe_period_frames)),
        marker: marker(),
        search: Vec::with_capacity(SEARCH_CAP + RENDER_SCRATCH),
        prefix_buf: Vec::with_capacity(SEARCH_CAP + RENDER_SCRATCH),
        pending: Arc::clone(&pending),
        latency: LatencyProbe::new(),
        threshold: if probe_mode == Some(ProbeMode::Acoustic) {
            0.35
        } else {
            0.60
        },
    }));
    let mut ioproc_id: AudioDeviceIOProcID = std::ptr::null_mut();
    // SAFETY: `vdev` is a live AudioDeviceID; `inject_ioproc` has the required
    // ABI; `inject` is process-lifetime (leak note above) so the client
    // pointer stays valid; `ioproc_id` is a valid out-slot.
    let rc = unsafe {
        AudioDeviceCreateIOProcID(
            vdev,
            inject_ioproc,
            inject as *mut InjectCtx as *mut c_void,
            &mut ioproc_id,
        )
    };
    if rc != 0 {
        bail!(
            "AudioDeviceCreateIOProcID(vdev) failed (OSStatus {})",
            os_status(rc)
        );
    }

    // ---- capture side (live denoise + acoustic probe) ---------------------
    let capture_unit = match (&engine, capture_dev) {
        (Some(e), Some(d)) if probe_mode != Some(ProbeMode::Digital) => {
            let core = Box::new(Core::new(e, &cfg, Arc::clone(&out_ring))?);
            let core_ptr = Box::into_raw(core); // kept alive for the process lifetime
            let unit = build_capture_unit(d, core_ptr)?;
            Some((unit, core_ptr))
        }
        _ => None,
    };

    // ---- speaker side (acoustic probe only) -------------------------------
    // The acoustic probe needs a real source at the *far* end: the physical
    // speaker plays the marker, the room and the physical microphone are the
    // channel, and the virtual microphone is where we look for it again.
    let speaker_unit = match (probe_mode, speaker_dev) {
        (Some(ProbeMode::Acoustic), Some(d)) => {
            let info = describe(d);
            let channels = info.output_channels.max(1);
            let ctx = Box::new(SpeakerCtx {
                marker: marker(),
                pos: 0,
                until_next: 0,
                // 与 MarkerEmitter::new 同款钳制：静音期不得短于标记本身
                period_frames: probe_period_frames.max(MARKER_LEN as u64),
                channels: channels as usize,
                pending: Arc::clone(&pending),
            });
            let ctx_ptr = Box::into_raw(ctx); // kept alive for the process lifetime
            let unit = build_speaker_unit(d, channels, ctx_ptr)?;
            Some((unit, ctx_ptr))
        }
        _ => None,
    };

    // ---- start everything -------------------------------------------------
    if let Some((unit, _)) = &capture_unit {
        // SAFETY: `unit` is an initialized AudioUnit (see build_capture_unit).
        let rc = unsafe { AudioOutputUnitStart(*unit) };
        if rc != 0 {
            bail!(
                "AudioOutputUnitStart(capture) failed (OSStatus {})",
                os_status(rc)
            );
        }
    }
    if let Some((unit, _)) = &speaker_unit {
        // SAFETY: `unit` is an initialized AudioUnit (see build_speaker_unit).
        let rc = unsafe { AudioOutputUnitStart(*unit) };
        if rc != 0 {
            bail!(
                "AudioOutputUnitStart(speaker) failed (OSStatus {})",
                os_status(rc)
            );
        }
    }
    // SAFETY: `vdev`/`ioproc_id` are the live pair created above.
    let rc = unsafe { AudioDeviceStart(vdev, ioproc_id) };
    if rc != 0 {
        bail!("AudioDeviceStart(vdev) failed (OSStatus {})", os_status(rc));
    }

    println!("\nrunning for {:.0} s ...", cfg.seconds);
    let wall0 = Instant::now();
    let cpu0 = cpu_seconds();
    let target = Duration::from_secs_f64(cfg.seconds.max(1.0));
    while wall0.elapsed() < target {
        std::thread::sleep(Duration::from_millis(500));
        if is_probe {
            let n = inject.latency.stats().count;
            println!(
                "  {:.0}s  markers detected: {}",
                wall0.elapsed().as_secs_f64(),
                n
            );
        } else {
            // SAFETY: `p` is the leaked `core_ptr` (process lifetime); the
            // audio thread may be writing it concurrently, but we only read a
            // u64 counter, which is torn-free on this platform.
            let core = capture_unit.as_ref().map(|(_, p)| unsafe { &**p });
            if let Some(c) = core {
                println!(
                    "  {:.0}s  frames: {}",
                    wall0.elapsed().as_secs_f64(),
                    c.frames
                );
            }
        }
    }
    let wall = wall0.elapsed().as_secs_f64();
    let cpu = (cpu_seconds() - cpu0).max(0.0);

    // ---- stop -------------------------------------------------------------
    if let Some((unit, _)) = &capture_unit {
        // SAFETY: `unit` is our live capture AU; Stop/Uninitialize/Dispose is
        // its documented teardown order. Errors are ignored on this exit path.
        unsafe {
            AudioOutputUnitStop(*unit);
            AudioUnitUninitialize(*unit);
            AudioComponentInstanceDispose(*unit);
        }
    }
    // SAFETY: `vdev`/`ioproc_id` are the live pair created above; Stop first
    // quiesces callbacks, then Destroy removes the IOProc. Errors ignored.
    unsafe {
        AudioDeviceStop(vdev, ioproc_id);
        AudioDeviceDestroyIOProcID(vdev, ioproc_id);
    }
    if let Some(speaker) = speaker_unit {
        // SAFETY: `speaker.0` is our live speaker AU; documented teardown order.
        unsafe {
            AudioOutputUnitStop(speaker.0);
            AudioUnitUninitialize(speaker.0);
            AudioComponentInstanceDispose(speaker.0);
        }
    }

    // ---- report -----------------------------------------------------------
    // SAFETY: `p` is the leaked `core_ptr` (process lifetime); the capture AU
    // was stopped above, so no thread is writing it anymore.
    let core = capture_unit.as_ref().map(|(_, p)| unsafe { &**p });
    let mut timing = core.map(|c| c.timing.clone()).unwrap_or_default();
    timing.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));

    let audio_seconds = core
        .map(|c| c.frames as f64 * frame_ms / 1000.0)
        .unwrap_or(0.0);
    let report = RunReport {
        mode: match probe_mode {
            None => "live denoise".into(),
            Some(ProbeMode::Digital) => "probe: digital".into(),
            Some(ProbeMode::Acoustic) => "probe: acoustic".into(),
        },
        seconds_requested: cfg.seconds,
        sample_rate: 48_000,
        frame_samples: engine.as_ref().map(|e| e.frame_size).unwrap_or(FRAME),
        frame_ms,
        engine: engine.as_ref().map(|e| e.path.display().to_string()),
        engine_state_bytes: engine.as_ref().map(|e| e.state_bytes),
        adaptive: core.map(|c| c.adaptive).unwrap_or(false),
        mix: core.map(|c| c.mix).unwrap_or(cfg.mix),
        vdev: describe(vdev),
        capture: capture_dev.map(describe),
        speaker: speaker_dev.map(describe),
        frames: core.map(|c| c.frames).unwrap_or(0),
        audio_seconds,
        wall_seconds: wall,
        vad_mean: core
            .map(|c| c.vad_sum / c.frames.max(1) as f64)
            .unwrap_or(0.0),
        wet_ratio_mean: core
            .map(|c| c.wet_sum / c.frames.max(1) as f64)
            .unwrap_or(0.0),
        wet_ratio_min: core
            .and_then(|c| (c.wet_min != f64::MAX).then_some(c.wet_min))
            .unwrap_or(0.0),
        wet_ratio_max: core
            .and_then(|c| (c.wet_max != f64::MIN).then_some(c.wet_max))
            .unwrap_or(0.0),
        frame_ms_p50: percentile(&timing, 0.50),
        frame_ms_p95: percentile(&timing, 0.95),
        frame_ms_p99: percentile(&timing, 0.99),
        frame_ms_max: timing.last().copied().unwrap_or(0.0),
        ring_dropped_samples: out_ring.dropped_samples(),
        ring_starved_samples: out_ring.starved_samples(),
        cpu_seconds: cpu,
        cpu_percent_of_one_core: if audio_seconds > 0.0 {
            cpu / audio_seconds * 100.0
        } else {
            0.0
        },
        latency: if is_probe {
            let s = inject.latency.stats();
            Some(LatencyStatsOut {
                count: s.count,
                injected: s.injected,
                detected: s.detected,
                rejected: s.rejected,
                min_ms: s.min_ms,
                mean_ms: s.mean_ms,
                p50_ms: s.p50_ms,
                p95_ms: s.p95_ms,
                max_ms: s.max_ms,
                stdev_ms: s.stdev_ms,
                interpretation: match probe_mode {
                    Some(ProbeMode::Digital) => format!(
                        "inject -> plugin ring -> virtual-mic capture. Add the model lookahead ({:.0} ms) for the model's total.",
                        frame_ms * 2.0
                    ),
                    _ => "physical speaker -> room -> physical mic -> denoise -> virtual mic. End to end; already includes the model's lookahead delay line.".into(),
                },
            })
        } else {
            None
        },
    };

    print_report(&report);

    // ---- A/B material from the live run -----------------------------------
    if let Some(c) = core {
        if let Some(p) = &cfg.record_in {
            crate::wavio::write(p, &c.rec_in)
                .with_context(|| format!("writing {}", p.display()))?;
            println!(
                "captured    : {} ({:.1} s)",
                p.display(),
                c.rec_in.len() as f64 / 48_000.0
            );
        }
        if let Some(p) = &cfg.record_out {
            crate::wavio::write(p, &c.rec_out)
                .with_context(|| format!("writing {}", p.display()))?;
            println!(
                "injected    : {} ({:.1} s)",
                p.display(),
                c.rec_out.len() as f64 / 48_000.0
            );
        }
    }
    if let Some(p) = &cfg.report {
        std::fs::write(p, serde_json::to_string_pretty(&report)?)?;
        println!("report      : {}", p.display());
    }
    Ok(())
}

fn print_report(r: &RunReport) {
    println!("\n=== {} ===", r.mode);
    if let Some(l) = &r.latency {
        println!(
            "round trips : {} detected of {} injected ({} rejected)",
            l.count, l.injected, l.rejected
        );
        println!(
            "latency     : min {:.2}  mean {:.2}  p50 {:.2}  p95 {:.2}  max {:.2}  sd {:.2} ms",
            l.min_ms, l.mean_ms, l.p50_ms, l.p95_ms, l.max_ms, l.stdev_ms
        );
        println!("              {}", l.interpretation);
    }
    if r.frames > 0 {
        println!(
            "frames      : {}  ({:.2} s of audio in {:.2} s wall)",
            r.frames, r.audio_seconds, r.wall_seconds
        );
        println!(
            "model time  : p50 {:.4}  p95 {:.4}  p99 {:.4}  max {:.4} ms  (budget {:.1} ms)",
            r.frame_ms_p50, r.frame_ms_p95, r.frame_ms_p99, r.frame_ms_max, r.frame_ms
        );
        println!(
            "cpu         : {:.2} s -> {:.2} % of one core",
            r.cpu_seconds, r.cpu_percent_of_one_core
        );
        println!(
            "wet ratio   : mean {:.3}  min {:.3}  max {:.3}",
            r.wet_ratio_mean, r.wet_ratio_min, r.wet_ratio_max
        );
    }
    println!(
        "ring        : dropped {} samples, starved {} samples",
        r.ring_dropped_samples, r.ring_starved_samples
    );
    if r.ring_starved_samples > 0 {
        println!("              (any starvation is an audible gap -- treat as a defect)");
    }
}

#[cfg(test)]
mod speaker_schedule_tests {
    use super::*;

    /// Drive `SpeakerCtx::next` sample-by-sample the way `speaker_render_cb`
    /// does, and pin the schedule: exactly one timestamp per marker playback,
    /// taken on the marker's first sample; exactly `period_frames` of silence
    /// between consecutive markers; no back-to-back replay after a marker
    /// ends. Regression guard for the old inline state machine, whose
    /// timestamp branch was unreachable and whose `period_frames` never took
    /// effect (markers replayed immediately).
    #[test]
    fn speaker_schedule_times_each_marker_once_and_honours_period() {
        let period: u64 = 2400; // 50 ms at 48 kHz
        let marker_len = MARKER_LEN as u64;
        let spacing = marker_len + period; // start-to-start distance
        let mut ctx = SpeakerCtx {
            marker: marker(),
            pos: 0,
            until_next: 0,
            period_frames: period,
            channels: 2,
            pending: Arc::new(Mutex::new(VecDeque::new())),
        };

        // 3 complete markers plus a partial trailing window: a 4th marker
        // starts on schedule at 3*spacing and is truncated by the window end
        // (on the device it just continues into the next callback).
        let total = (3 * spacing + 123) as usize;
        let mut out: Vec<f32> = Vec::with_capacity(total);
        let mut starts: Vec<usize> = Vec::new();
        for i in 0..total {
            let (v, ts) = ctx.next();
            if ts.is_some() {
                starts.push(i);
            }
            out.push(v);
        }

        // Every timestamped start emits the marker: complete windows match it
        // bit for bit, the final truncated one matches its prefix exactly.
        for (k, &s) in starts.iter().enumerate() {
            let avail = (out.len() - s).min(MARKER_LEN);
            assert_eq!(
                &out[s..s + avail],
                &ctx.marker[..avail],
                "playback {} must be the marker from its first sample",
                k
            );
        }

        // 1) one timestamp per playback, on the marker's first sample: starts
        //    sit exactly on the schedule (k * spacing), and every complete
        //    marker window in the output is one of them.
        let expected: Vec<usize> = (0..4u64).map(|k| (k * spacing) as usize).collect();
        assert_eq!(
            starts, expected,
            "timestamps must sit exactly on marker first-samples"
        );
        let complete_plays: Vec<usize> = (0..=out.len() - MARKER_LEN)
            .filter(|&s| out[s..s + MARKER_LEN] == *ctx.marker)
            .collect();
        assert_eq!(
            complete_plays,
            starts[..3],
            "timestamp count must equal marker playback count"
        );

        // 2) the silence between consecutive markers is exactly period_frames.
        for k in 0..starts.len() - 1 {
            let gap = &out[starts[k] + MARKER_LEN..starts[k + 1]];
            assert_eq!(
                gap.len() as u64,
                period,
                "gap {} must be exactly period_frames",
                k
            );
            assert!(gap.iter().all(|&v| v == 0.0), "gap {} must be silent", k);
        }

        // 3) no back-to-back replay: after the third marker ends, the speaker
        //    stays silent for the full period before the 4th marker starts.
        let tail = &out[starts[2] + MARKER_LEN..starts[3]];
        assert_eq!(tail.len() as u64, period);
        assert!(
            tail.iter().all(|&v| v == 0.0),
            "tail must stay silent (no immediate replay)"
        );
    }
}
