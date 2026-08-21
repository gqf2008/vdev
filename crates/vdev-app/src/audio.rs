//! 视频音频推流：AVAssetReader 解音轨 → CoreAudio AudioUnit 输出到 VB-Cable（虚拟声卡）。
//! 之后 QuickTime/Zoom 等把「麦克风」选成 VB-Cable 即可听到视频原声。
//! 全 Rust FFI（AudioToolbox / AudioUnit / AVFoundation）。

use anyhow::{anyhow, Result};
use objc2::msg_send;
use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::ClassType;
use objc2_av_foundation::{AVAssetReader, AVAssetReaderTrackOutput, AVURLAsset};
use objc2_foundation::{NSDictionary, NSString, NSURL};
use std::collections::VecDeque;
use std::ffi::c_void;
use std::sync::atomic::Ordering;
use std::sync::Mutex;

// ---------------- 常量（CoreAudio / AudioUnit）----------------
const K_AUDIO_OBJECT_SYSTEM: u32 = 1; // kAudioObjectSystemObject
const K_HW_PROP_DEVICES: u32 = 0x64657623; // kAudioHardwarePropertyDevices .dev#.
const K_OBJ_SCOPE_GLOBAL: u32 = 0x676c6f62; // 'glob'
const K_OBJ_ELEMENT_MAIN: u32 = 0;
const K_DEV_PROP_NAME_CFSTRING: u32 = 0x6c6e616d; // kAudioDevicePropertyDeviceNameCFString .lnam.
const K_AUDIO_UNIT_TYPE_OUTPUT: u32 = 0x61756f75; // 'auou'
const K_AUDIO_UNIT_SUBTYPE_HAL: u32 = 0x6168616c; // 'ahal'
const K_AUDIO_UNIT_MANUF_APPLE: u32 = 0x6170706c; // 'appl'
const K_OUTPUT_UNIT_PROP_CURRENT_DEVICE: u32 = 2000; // kAudioOutputUnitProperty_CurrentDevice
const K_AUDIO_UNIT_PROP_STREAM_FORMAT: u32 = 8; // kAudioUnitProperty_StreamFormat
const K_AUDIO_UNIT_PROP_SET_RENDER_CB: u32 = 23; // kAudioUnitProperty_SetRenderCallback
const K_AUDIO_UNIT_SCOPE_GLOBAL: u32 = 0;
const K_AUDIO_UNIT_SCOPE_INPUT: u32 = 1;
const SAMPLE_RATE: f64 = 48_000.0;
const CHANNELS: u32 = 2;
// 环缓冲容量：约 0.5s（按 48k*2ch*f32）
const RING_CAP: usize = SAMPLE_RATE as usize * CHANNELS as usize / 2;

type AudioObjectID = u32;

#[repr(C)]
#[derive(Clone, Copy)]
struct AudioObjectPropertyAddress {
    m_selector: u32,
    m_scope: u32,
    m_element: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct AudioStreamBasicDescription {
    m_sample_rate: f64,
    m_format_id: u32,
    m_format_flags: u32,
    m_bytes_per_packet: u32,
    m_frames_per_packet: u32,
    m_bytes_per_frame: u32,
    m_channels_per_frame: u32,
    m_bits_per_channel: u32,
    m_reserved: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct AudioBuffer {
    m_number_channels: u32,
    m_data_byte_size: u32,
    m_data: *mut c_void,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct AudioBufferList {
    m_number_buffers: u32,
    m_buffers: [AudioBuffer; 1],
}

#[repr(C)]
struct AudioTimeStamp {
    m_sample_time: f64,
    m_host_time: u64,
    m_rate_scalar: f64,
    m_word_clock_time: u64,
    m_smpte_time: u64,
    m_flags: u32,
    m_reserved: u32,
}

#[repr(C)]
struct AudioUnitRenderingActionFlags(u32);

#[repr(C)]
struct AudioUnitElement(u32);

#[repr(C)]
struct AudioUnit(u32);

#[repr(C)]
struct OpaqueAudioComponent(*mut c_void);
#[repr(C)]
struct OpaqueAudioComponentInstance(*mut c_void);

#[link(name = "CoreAudio", kind = "framework")]
extern "C" {
    fn AudioObjectGetPropertyDataSize(
        object: u32,
        address: *const AudioObjectPropertyAddress,
        qualifier_data_size: u32,
        qualifier_data: *const c_void,
        data_size: *mut u32,
    ) -> i32;
    fn AudioObjectGetPropertyData(
        object: u32,
        address: *const AudioObjectPropertyAddress,
        qualifier_data_size: u32,
        qualifier_data: *const c_void,
        data_size: *mut u32,
        data: *mut c_void,
    ) -> i32;
}

#[link(name = "CoreMedia", kind = "framework")]
extern "C" {
    fn CMSampleBufferGetAudioBufferListWithRetainedBlockBuffer(
        sb: *mut c_void,
        buffer_list_size_needed: *mut usize,
        buffer_list: *mut AudioBufferList,
        buffer_list_size: usize,
        block_buffer_allocator: *const c_void,
        buffer_allocator: *const c_void,
        flags: u32,
        block_buffer_out: *mut *mut c_void,
    ) -> i32;
    fn CFRelease(obj: *const c_void);
}

#[link(name = "AudioToolbox", kind = "framework")]
extern "C" {
    fn AudioComponentFindNext(
        component: *mut OpaqueAudioComponent,
        desc: *const AudioComponentDescription,
    ) -> *mut OpaqueAudioComponent;
    fn AudioComponentInstanceNew(
        component: *mut OpaqueAudioComponent,
        out: *mut *mut OpaqueAudioComponentInstance,
    ) -> i32;
    fn AudioUnitInitialize(unit: *mut OpaqueAudioComponentInstance) -> i32;
    fn AudioUnitUninitialize(unit: *mut OpaqueAudioComponentInstance) -> i32;
    fn AudioComponentInstanceDispose(unit: *mut OpaqueAudioComponentInstance) -> i32;
    fn AudioUnitSetProperty(
        unit: *mut OpaqueAudioComponentInstance,
        id: u32,
        scope: u32,
        element: u32,
        data: *const c_void,
        data_size: u32,
    ) -> i32;
    fn AudioOutputUnitStart(unit: *mut OpaqueAudioComponentInstance) -> i32;
    fn AudioOutputUnitStop(unit: *mut OpaqueAudioComponentInstance) -> i32;
}

#[repr(C)]
struct AudioComponentDescription {
    component_type: u32,
    component_sub_type: u32,
    component_manufacturer: u32,
    component_flags: u32,
    component_flags_mask: u32,
}

// ---------------- 环缓冲（渲染回调与解码线程共享）----------------
struct Ring {
    buf: VecDeque<f32>,
    cap: usize,
}

impl Ring {
    fn new(cap: usize) -> Self {
        Self { buf: VecDeque::with_capacity(cap), cap }
    }
    fn push(&mut self, data: &[f32]) {
        for &v in data {
            self.buf.push_back(v);
        }
    }
    fn pop_n(&mut self, n: usize) -> Vec<f32> {
        let take = n.min(self.buf.len());
        let mut out: Vec<f32> = self.buf.drain(0..take).collect();
        out.resize(n, 0.0);
        out
    }
}

static RING: Mutex<Option<Ring>> = Mutex::new(None);
static RENDER_CALLS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static RENDER_SAMPLES: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

// 渲染回调（实时线程）：拉 n 帧 x 2ch 交错 f32，环空补静音
extern "C" fn render_cb(
    _in_ref_con: *mut c_void,
    _io_action_flags: *mut AudioUnitRenderingActionFlags,
    _in_time_stamp: *const AudioTimeStamp,
    _in_bus_number: u32,
    in_number_frames: u32,
    io_data: *mut AudioBufferList,
) -> i32 {
    let frames = in_number_frames as usize;
    let samples = frames * CHANNELS as usize;
    let mut out = vec![0.0f32; samples];
    let mut popped = 0usize;
    if let Ok(mut g) = RING.lock() {
        if let Some(r) = g.as_mut() {
            let before = r.buf.len();
            out = r.pop_n(samples);
            popped = before - r.buf.len();
        }
    }
    RENDER_CALLS.fetch_add(1, Ordering::SeqCst);
    RENDER_SAMPLES.fetch_add(popped as u64, Ordering::SeqCst);
    if RENDER_CALLS.load(Ordering::SeqCst) % 1000 == 1 {
    }
    if !io_data.is_null() {
        unsafe {
            let abl = &*io_data;
            if abl.m_number_buffers > 0 {
                let buf = &abl.m_buffers[0];
                if !buf.m_data.is_null() {
                    let dst = std::slice::from_raw_parts_mut(buf.m_data as *mut f32, samples);
                    dst.copy_from_slice(&out);
                }
            }
        }
    }
    0
}

/// 找名字包含 keyword 的音频设备 ID（VB-Cable 等）。
fn find_device_id(keyword: &str) -> Option<u32> {
    unsafe {
        let addr = AudioObjectPropertyAddress {
            m_selector: K_HW_PROP_DEVICES,
            m_scope: K_OBJ_SCOPE_GLOBAL,
            m_element: K_OBJ_ELEMENT_MAIN,
        };
        let mut size: u32 = 0;
        let rc = AudioObjectGetPropertyDataSize(
            K_AUDIO_OBJECT_SYSTEM,
            &addr,
            0,
            std::ptr::null(),
            &mut size,
        );
        if rc != 0 {
            return None;
        }
        let count = size as usize / std::mem::size_of::<u32>();
        let mut ids = vec![0u32; count];
        if AudioObjectGetPropertyData(
            K_AUDIO_OBJECT_SYSTEM,
            &addr,
            0,
            std::ptr::null(),
            &mut size,
            ids.as_mut_ptr() as *mut c_void,
        ) != 0
        {
            return None;
        }
        let name_addr = AudioObjectPropertyAddress {
            m_selector: K_DEV_PROP_NAME_CFSTRING,
            m_scope: K_OBJ_SCOPE_GLOBAL,
            m_element: K_OBJ_ELEMENT_MAIN,
        };
        for id in ids {
            // 返回 CFStringRef（与 NSString toll-free 桥接），需要释放
            let mut name_ref: *mut c_void = std::ptr::null_mut();
            let mut nsize = std::mem::size_of::<*mut c_void>() as u32;
            if AudioObjectGetPropertyData(
                id,
                &name_addr,
                0,
                std::ptr::null(),
                &mut nsize,
                &mut name_ref as *mut *mut c_void as *mut c_void,
            ) == 0 && !name_ref.is_null()
            {
                let ns = name_ref as *const NSString;
                let name = unsafe { Retained::retain(ns as *mut NSString) }
                    .map(|s| s.to_string())
                    .unwrap_or_default();
                // 释放 CFString（API 返回 +1）
                unsafe {
                    let _: () = msg_send![&*ns, release];
                }
                if name.to_lowercase().contains(&keyword.to_lowercase()) {
                    return Some(id);
                }
            } else {
            }
        }
        None
    }
}

fn set_stream_format(unit: *mut OpaqueAudioComponentInstance) -> Result<()> {
    let desc = AudioStreamBasicDescription {
        m_sample_rate: SAMPLE_RATE,
        m_format_id: 0x6c70636d, // kAudioFormatLinearPCM 'lpcm'
        m_format_flags: 0x0c,    // kAudioFormatFlagIsFloat | kAudioFormatFlagIsPacked
        m_bytes_per_packet: (CHANNELS * 4) as u32,
        m_frames_per_packet: 1,
        m_bytes_per_frame: (CHANNELS * 4) as u32,
        m_channels_per_frame: CHANNELS,
        m_bits_per_channel: 32,
        m_reserved: 0,
    };
    let rc = unsafe {
        AudioUnitSetProperty(
            unit,
            K_AUDIO_UNIT_PROP_STREAM_FORMAT,
            K_AUDIO_UNIT_SCOPE_INPUT,
            0,
            &desc as *const AudioStreamBasicDescription as *const c_void,
            std::mem::size_of::<AudioStreamBasicDescription>() as u32,
        )
    };
    if rc != 0 {
        return Err(anyhow!("AudioUnitSetProperty(StreamFormat) 失败 rc={}", rc));
    }
    Ok(())
}

/// 启动视频音频推流：解音轨 → VB-Cable（后台线程，随 crate::VIDEO_STOP 停止）。
/// 返回是否成功启动（无音轨/无 VB-Cable 则 false）。
pub fn start_audio_push(path: &str) -> bool {
    let path = path.to_string();
    let Some(dev_id) = find_device_id("VB-Cable") else {
        eprintln!("音频推流: 未找到 VB-Cable，跳过（可在系统设置安装/打开虚拟声卡）");
        return false;
    };
    eprintln!("音频推流: 使用设备 {} 播放视频音轨", dev_id);
    std::thread::spawn(move || {
        let _ = run_audio(&path, dev_id);
    });
    true
}

fn run_audio(path: &str, dev_id: u32) -> bool {
    // 1) 打开 asset + 音频轨
    let asset = unsafe {
        let url = NSURL::fileURLWithPath_isDirectory_relativeToURL(&NSString::from_str(path), false, None);
        AVURLAsset::URLAssetWithURL_options(&url, None)
    };
    let tracks = unsafe { asset.tracksWithMediaType(objc2_av_foundation::AVMediaTypeAudio.unwrap()) };
    let Some(track) = tracks.firstObject() else {
        eprintln!("音频推流: 视频没有音轨，跳过");
        return false;
    };
    let reader = unsafe { AVAssetReader::assetReaderWithAsset_error(&asset) }
        .expect("AVAssetReader 创建失败");

    // 简化为用 kAudioFormatLinearPCM 相关键（AVFormatIDKey）避免复杂：
    // 输出设置：48k 立体声 32bit float（交错）
    let keys: Retained<objc2_foundation::NSArray<AnyObject>> =
        objc2_foundation::NSArray::from_retained_slice(&[
            NSString::from_str("AVFormatIDKey").into(),
            NSString::from_str("AVSampleRateKey").into(),
            NSString::from_str("AVNumberOfChannelsKey").into(),
            NSString::from_str("AVLinearPCMBitDepthKey").into(),
            NSString::from_str("AVLinearPCMIsFloatKey").into(),
            NSString::from_str("AVLinearPCMIsNonInterleaved").into(),
            NSString::from_str("AVLinearPCMIsBigEndianKey").into(),
        ]);
    let vals: Retained<objc2_foundation::NSArray<AnyObject>> =
        objc2_foundation::NSArray::from_retained_slice(&[
            objc2_foundation::NSNumber::numberWithUnsignedInt(0x6c70636d).into(), // kAudioFormatLinearPCM
            objc2_foundation::NSNumber::numberWithInt(48_000).into(),
            objc2_foundation::NSNumber::numberWithInt(2).into(),
            objc2_foundation::NSNumber::numberWithInt(32).into(),
            objc2_foundation::NSNumber::numberWithInt(1).into(),
            objc2_foundation::NSNumber::numberWithInt(0).into(),
            objc2_foundation::NSNumber::numberWithInt(0).into(),
        ]);
    let dict: Retained<NSDictionary<NSString>> = unsafe {
        msg_send![
            <NSDictionary<NSString>>::class(),
            dictionaryWithObjects: &*vals,
            forKeys: &*keys
        ]
    };
    let desc: Retained<NSString> = unsafe { msg_send![&*dict, description] };
    let output = unsafe {
        AVAssetReaderTrackOutput::assetReaderTrackOutputWithTrack_outputSettings(&*track, Some(&*dict))
    };
    let odesc: Retained<NSString> = unsafe { msg_send![&*output, description] };
    unsafe { reader.addOutput(&output); }
    if !unsafe { reader.startReading() } {
        eprintln!("音频推流: startReading 失败");
        return false;
    }

    // 2) AudioUnit HALOutput → VB-Cable
    let desc = AudioComponentDescription {
        component_type: K_AUDIO_UNIT_TYPE_OUTPUT,
        component_sub_type: K_AUDIO_UNIT_SUBTYPE_HAL,
        component_manufacturer: K_AUDIO_UNIT_MANUF_APPLE,
        component_flags: 0,
        component_flags_mask: 0,
    };
    let comp = unsafe { AudioComponentFindNext(std::ptr::null_mut(), &desc) };
    if comp.is_null() {
        eprintln!("音频推流: 找不到 HALOutput AudioUnit");
        return false;
    }
    let mut unit: *mut OpaqueAudioComponentInstance = std::ptr::null_mut();
    if unsafe { AudioComponentInstanceNew(comp, &mut unit) } != 0 || unit.is_null() {
        eprintln!("音频推流: AudioComponentInstanceNew 失败");
        return false;
    }
    if unsafe { AudioUnitInitialize(unit) } != 0 {
        eprintln!("音频推流: AudioUnitInitialize 失败");
        return false;
    }
    // 指定输出设备 = VB-Cable
    if unsafe {
        AudioUnitSetProperty(
            unit,
            K_OUTPUT_UNIT_PROP_CURRENT_DEVICE,
            K_AUDIO_UNIT_SCOPE_GLOBAL,
            0,
            &dev_id as *const u32 as *const c_void,
            4,
        )
    } != 0
    {
        eprintln!("音频推流: 设置 CurrentDevice 失败");
        return false;
    }
    if set_stream_format(unit).is_err() {
        return false;
    }
    // 渲染回调（AURenderCallbackStruct）
    #[repr(C)]
    struct AURenderCallbackStruct {
        input_proc: *const c_void,
        input_proc_ref_con: *mut c_void,
    }
    let cb = AURenderCallbackStruct {
        input_proc: render_cb as *const c_void,
        input_proc_ref_con: std::ptr::null_mut(),
    };
    let rc = unsafe {
        AudioUnitSetProperty(
            unit,
            K_AUDIO_UNIT_PROP_SET_RENDER_CB,
            K_AUDIO_UNIT_SCOPE_INPUT,
            0,
            &cb as *const AURenderCallbackStruct as *const c_void,
            std::mem::size_of::<AURenderCallbackStruct>() as u32,
        )
    };
    if rc != 0 {
        eprintln!("音频推流: SetRenderCallback 失败 rc={}", rc);
        return false;
    }
    *RING.lock().unwrap_or_else(|e| e.into_inner()) = Some(Ring::new(RING_CAP));
    if unsafe { AudioOutputUnitStart(unit) } != 0 {
        eprintln!("音频推流: AudioOutputUnitStart 失败");
        return false;
    }

    // 3) 解码循环：推入环缓冲（环满则等 = 自然按实时节奏）
    eprintln!("音频推流: 开始");
    let mut pushed = 0usize;
    let mut first = true;
    loop {
        if first {
            first = false;
        }
        if crate::VIDEO_STOP.load(Ordering::SeqCst) {
            break;
        }
        let sample: *mut AnyObject = unsafe { msg_send![&*output, copyNextSampleBuffer] };
        if sample.is_null() {
            let status: i64 = unsafe { msg_send![&*reader, status] };
            break;
        }
        // CF 对象不能发 ObjC 消息，用 C API 取 AudioBufferList（blockBufferOut 必须非空）
        let mut size_needed: usize = 0;
        let rc1 = unsafe {
            CMSampleBufferGetAudioBufferListWithRetainedBlockBuffer(
                sample as *mut c_void,
                &mut size_needed,
                std::ptr::null_mut(),
                0,
                std::ptr::null(),
                std::ptr::null(),
                0,
                std::ptr::null_mut(),
            )
        };
        let mut storage = vec![
            0u8;
            size_needed.max(std::mem::size_of::<AudioBufferList>())
        ];
        let abl = storage.as_mut_ptr() as *mut AudioBufferList;
        let mut block_buf: *mut c_void = std::ptr::null_mut();
        let rc2 = unsafe {
            CMSampleBufferGetAudioBufferListWithRetainedBlockBuffer(
                sample as *mut c_void,
                &mut size_needed,
                abl,
                size_needed,
                std::ptr::null(),
                std::ptr::null(),
                0,
                &mut block_buf,
            )
        };
        if !block_buf.is_null() {
            unsafe { CFRelease(block_buf as *const c_void); }
        }
        if rc2 == 0 && unsafe { (*abl).m_number_buffers } > 0
            && unsafe { (*abl).m_buffers[0].m_data_byte_size } > 0
            && !unsafe { (*abl).m_buffers[0].m_data }.is_null()
        {
            let n = unsafe { ((*abl).m_buffers[0].m_data_byte_size as usize) / 4 };
            let src = unsafe {
                std::slice::from_raw_parts((*abl).m_buffers[0].m_data as *const f32, n)
            };
            if let Ok(mut g) = RING.lock() {
                if let Some(r) = g.as_mut() {
                    r.push(src);
                }
            }
            pushed += n;
        }
        unsafe {
            CFRelease(sample as *const c_void);
        }
        // 环满背压：等待渲染回调消费，按实时节奏播放（不丢开头）
        loop {
            let len = RING
                .lock()
                .map(|g| g.as_ref().map(|r| r.buf.len()).unwrap_or(0))
                .unwrap_or(0);
            if len < RING_CAP {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }
    unsafe {
        AudioOutputUnitStop(unit);
        AudioUnitUninitialize(unit);
        AudioComponentInstanceDispose(unit);
    }
    eprintln!("音频推流: 结束，共推 {} samples", pushed);
    true
}
