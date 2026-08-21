//! AudioServerPlugInDriverInterface vtable —— 对照 CoreAudio.framework AudioServerPlugIn.h。
//! 布局必须与 C 完全一致（IUNKNOWN_C_GUTS + 19 个插件方法）。

#![allow(non_camel_case_types)]
use std::ffi::c_void;

pub type OSStatus = i32;
pub type Boolean = u8;
pub type HRESULT = i32;
pub type ULONG = u32;
pub type UInt64 = u64;
pub type Float64 = f64;
pub type SInt16 = i16;
pub type AudioObjectID = u32;
pub type pid_t = i32;
// REFIID = CFUUIDBytes（16 字节，arm64 上按值传 x1:x2）——不是指针！
#[repr(C)]
#[derive(Clone, Copy)]
pub struct CFUUIDBytes {
    pub m_data: [u8; 16],
}
pub type REFIID = CFUUIDBytes;
pub type LPVOID = *mut c_void;
pub type CFDictionaryRef = *const c_void;
pub type AudioServerPlugInHostRef = *mut c_void;

#[repr(C)]
pub struct AudioObjectPropertyAddress {
    pub m_selector: u32,
    pub m_scope: u32,
    pub m_element: u32,
}

#[repr(C)]
pub struct AudioServerPlugInClientInfo {
    pub m_client_id: u32,
    pub m_process_id: pid_t,
    pub m_bundle_id: *mut c_void, // CFStringRef
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct SMPTETime {
    pub m_subframes: SInt16,
    pub m_subframe_divisor: SInt16,
    pub m_counter: u32,
    pub m_type: u32,
    pub m_flags: u32,
    pub m_hours: SInt16,
    pub m_minutes: SInt16,
    pub m_seconds: SInt16,
    pub m_frames: SInt16,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct AudioTimeStamp {
    pub m_sample_time: f64,
    pub m_host_time: u64,
    pub m_rate_scalar: f64,
    pub m_word_clock_time: u64,
    pub m_smpte_time: SMPTETime,
    pub m_flags: u32,
    pub m_reserved: u32,
}

#[repr(C)]
pub struct AudioServerPlugInIOCycleInfo {
    pub m_io_cycle_counter: u64,
    pub m_nominal_io_buffer_frame_size: u32,
    pub m_current_time: AudioTimeStamp,
    pub m_input_time: AudioTimeStamp,
    pub m_output_time: AudioTimeStamp,
    pub m_main_host_ticks_per_frame: f64,
    pub m_device_host_ticks_per_frame: f64,
}

pub type AudioServerPlugInDriverRef = *mut *mut AudioServerPlugInDriverInterface;

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
#[derive(Clone, Copy)]
pub struct AudioValueRange {
    pub m_minimum: f64,
    pub m_maximum: f64,
}


#[repr(C)]
pub struct AudioServerPlugInDriverInterface {
    // IUNKNOWN_C_GUTS
    pub _reserved: *mut c_void,
    pub query_interface:
        Option<unsafe extern "C" fn(*mut c_void, REFIID, *mut LPVOID) -> HRESULT>,
    pub add_ref: Option<unsafe extern "C" fn(*mut c_void) -> ULONG>,
    pub release: Option<unsafe extern "C" fn(*mut c_void) -> ULONG>,
    // 插件方法
    pub initialize:
        Option<unsafe extern "C" fn(AudioServerPlugInDriverRef, AudioServerPlugInHostRef) -> OSStatus>,
    pub create_device: Option<
        unsafe extern "C" fn(
            AudioServerPlugInDriverRef,
            CFDictionaryRef,
            *const AudioServerPlugInClientInfo,
            *mut AudioObjectID,
        ) -> OSStatus,
    >,
    pub destroy_device:
        Option<unsafe extern "C" fn(AudioServerPlugInDriverRef, AudioObjectID) -> OSStatus>,
    pub add_device_client: Option<
        unsafe extern "C" fn(
            AudioServerPlugInDriverRef,
            AudioObjectID,
            *const AudioServerPlugInClientInfo,
        ) -> OSStatus,
    >,
    pub remove_device_client: Option<
        unsafe extern "C" fn(
            AudioServerPlugInDriverRef,
            AudioObjectID,
            *const AudioServerPlugInClientInfo,
        ) -> OSStatus,
    >,
    pub perform_device_config_change: Option<
        unsafe extern "C" fn(AudioServerPlugInDriverRef, AudioObjectID, u64, *mut c_void) -> OSStatus,
    >,
    pub abort_device_config_change: Option<
        unsafe extern "C" fn(AudioServerPlugInDriverRef, AudioObjectID, u64, *mut c_void) -> OSStatus,
    >,
    pub has_property: Option<
        unsafe extern "C" fn(
            AudioServerPlugInDriverRef,
            AudioObjectID,
            pid_t,
            *const AudioObjectPropertyAddress,
        ) -> Boolean,
    >,
    pub is_property_settable: Option<
        unsafe extern "C" fn(
            AudioServerPlugInDriverRef,
            AudioObjectID,
            pid_t,
            *const AudioObjectPropertyAddress,
            *mut Boolean,
        ) -> OSStatus,
    >,
    pub get_property_data_size: Option<
        unsafe extern "C" fn(
            AudioServerPlugInDriverRef,
            AudioObjectID,
            pid_t,
            *const AudioObjectPropertyAddress,
            u32,
            *const c_void,
            *mut u32,
        ) -> OSStatus,
    >,
    pub get_property_data: Option<
        unsafe extern "C" fn(
            AudioServerPlugInDriverRef,
            AudioObjectID,
            pid_t,
            *const AudioObjectPropertyAddress,
            u32,
            *const c_void,
            u32,
            *mut u32,
            *mut c_void,
        ) -> OSStatus,
    >,
    pub set_property_data: Option<
        unsafe extern "C" fn(
            AudioServerPlugInDriverRef,
            AudioObjectID,
            pid_t,
            *const AudioObjectPropertyAddress,
            u32,
            *const c_void,
            u32,
            *const c_void,
        ) -> OSStatus,
    >,
    pub start_io:
        Option<unsafe extern "C" fn(AudioServerPlugInDriverRef, AudioObjectID, u32) -> OSStatus>,
    pub stop_io:
        Option<unsafe extern "C" fn(AudioServerPlugInDriverRef, AudioObjectID, u32) -> OSStatus>,
    pub get_zero_time_stamp: Option<
        unsafe extern "C" fn(
            AudioServerPlugInDriverRef,
            AudioObjectID,
            u32,
            *mut Float64,
            *mut UInt64,
            *mut UInt64,
        ) -> OSStatus,
    >,
    pub will_do_io_operation: Option<
        unsafe extern "C" fn(
            AudioServerPlugInDriverRef,
            AudioObjectID,
            u32,
            u32,
            *mut Boolean,
            *mut Boolean,
        ) -> OSStatus,
    >,
    pub begin_io_operation: Option<
        unsafe extern "C" fn(
            AudioServerPlugInDriverRef,
            AudioObjectID,
            u32,
            u32,
            u32,
            *const AudioServerPlugInIOCycleInfo,
        ) -> OSStatus,
    >,
    pub do_io_operation: Option<
        unsafe extern "C" fn(
            AudioServerPlugInDriverRef,
            AudioObjectID,
            AudioObjectID,
            u32,
            u32,
            u32,
            *const AudioServerPlugInIOCycleInfo,
            *mut c_void,
            *mut c_void,
        ) -> OSStatus,
    >,
    pub end_io_operation: Option<
        unsafe extern "C" fn(
            AudioServerPlugInDriverRef,
            AudioObjectID,
            u32,
            u32,
            u32,
            *const AudioServerPlugInIOCycleInfo,
        ) -> OSStatus,
    >,
}
