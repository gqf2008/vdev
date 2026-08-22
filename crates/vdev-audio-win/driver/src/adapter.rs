//! AdapterCommon：适配器对象 + 虚拟声卡（扬声器→麦克风环回）安装流程
#![allow(non_snake_case, non_camel_case_types)]
#![allow(clippy::missing_errors_doc)]

use core::mem::size_of;

use crate::com::{interlocked_decrement, interlocked_increment};
use crate::miniport::MiniportWaveRT;
use crate::ringbuffer::RingBuffer;
use crate::sys::mem::{ExAllocatePool2_np, ExFreePoolWithTag_np};
use crate::sys::portcls::*;
use crate::sys::types::*;

pub const TAG: u32 = u32::from_le_bytes(*b"vdev");

/// IAdapterCommon vtable
#[repr(C)]
pub struct IAdapterCommonVtbl {
    pub query_interface: crate::com::PFN_QUERYINTERFACE,
    pub add_ref: crate::com::PFN_ADDREF,
    pub release: crate::com::PFN_RELEASE,
    pub init: unsafe extern "system" fn(PVOID, PIRP, PDEVICE_OBJECT) -> NTSTATUS,
    pub get_device_object: unsafe extern "system" fn(PVOID) -> PDEVICE_OBJECT,
    pub get_physical_device_object: unsafe extern "system" fn(PVOID) -> PDEVICE_OBJECT,
    pub write_etw_event: unsafe extern "system" fn(PVOID, u32, u64, u64, u64, u64) -> NTSTATUS,
    pub set_etw_helper: unsafe extern "system" fn(PVOID, PVOID),
    pub cleanup: unsafe extern "system" fn(PVOID),
}

/// IPort vtable（小端口侧调用端口）
#[repr(C)]
pub struct IPortVtbl {
    pub query_interface: crate::com::PFN_QUERYINTERFACE,
    pub add_ref: crate::com::PFN_ADDREF,
    pub release: crate::com::PFN_RELEASE,
    pub init: unsafe extern "system" fn(
        PVOID,
        PDEVICE_OBJECT,
        PIRP,
        PVOID,
        PVOID,
        *mut c_void,
    ) -> NTSTATUS,
    pub get_device_property:
        unsafe extern "system" fn(PVOID, u32, u32, PVOID, *mut u32) -> NTSTATUS,
    pub new_registry_key: unsafe extern "system" fn(
        PVOID,
        *mut *mut c_void,
        PVOID,
        u32,
        u32,
        *mut c_void,
        u32,
        *mut u32,
    ) -> NTSTATUS,
}

/// 适配器对象
#[repr(C)]
pub struct AdapterCommon {
    pub vtable: &'static IAdapterCommonVtbl,
    pub refcount: u32,
    pub device_object: PDEVICE_OBJECT,
    pub physical_device_object: PDEVICE_OBJECT,
    pub ring: *mut RingBuffer,
    pub mic: *mut MiniportWaveRT,
    pub speaker: *mut MiniportWaveRT,
    pub mic_port: PVOID,
    pub speaker_port: PVOID,
}

// 单例：一次只允许一个适配器
static mut INSTANCES: u32 = 0;

unsafe extern "system" fn adapter_qi(
    this: PVOID,
    iid: *const GUID,
    obj: *mut *mut c_void,
) -> NTSTATUS {
    let this = this as *mut AdapterCommon;
    // SAFETY: 调用方保证 iid/obj 有效
    if is_equal_guid(iid, &IID_IUnknown) || is_equal_guid(iid, &IID_IAdapterCommon) {
        *obj = this.cast();
        interlocked_increment(core::ptr::addr_of_mut!((*this).refcount));
        STATUS_SUCCESS
    } else {
        *obj = core::ptr::null_mut();
        STATUS_INVALID_PARAMETER
    }
}

unsafe extern "system" fn adapter_addref(this: PVOID) -> u32 {
    interlocked_increment(core::ptr::addr_of_mut!(
        (*(this as *mut AdapterCommon)).refcount
    ))
}

unsafe extern "system" fn adapter_release(this: PVOID) -> u32 {
    let this = this as *mut AdapterCommon;
    // SAFETY: 引用计数保护
    let rc = unsafe { interlocked_decrement(core::ptr::addr_of_mut!((*this).refcount)) };
    if rc == 0 {
        // SAFETY: 释放环形缓冲与子对象
        if !(*this).ring.is_null() {
            ExFreePoolWithTag_np((*this).ring.cast(), TAG);
        }
        if !(*this).mic.is_null() {
            crate::miniport::miniport_release((*this).mic.cast());
        }
        if !(*this).speaker.is_null() {
            crate::miniport::miniport_release((*this).speaker.cast());
        }
        ExFreePoolWithTag_np(this.cast(), TAG);
        INSTANCES = 0;
    }
    rc
}

static ADAPTER_VTABLE: IAdapterCommonVtbl = IAdapterCommonVtbl {
    query_interface: adapter_qi,
    add_ref: adapter_addref,
    release: adapter_release,
    init: adapter_init,
    get_device_object: adapter_get_device_object,
    get_physical_device_object: adapter_get_physical_device_object,
    write_etw_event: adapter_write_etw_event,
    set_etw_helper: adapter_set_etw_helper,
    cleanup: adapter_cleanup,
};

unsafe extern "system" fn adapter_init(
    this: PVOID,
    _irp: PIRP,
    device: PDEVICE_OBJECT,
) -> NTSTATUS {
    let this = this as *mut AdapterCommon;
    // SAFETY: 单线程初始化
    (*this).device_object = device;
    let mut pdo: PDEVICE_OBJECT = core::ptr::null_mut();
    let st = PcGetPhysicalDeviceObject(device, &mut pdo);
    if st < 0 {
        return st;
    }
    (*this).physical_device_object = pdo;
    // 分配共享环形缓冲（1 MB）
    const RING_SIZE: usize = 1024 * 1024;
    let ring_mem = ExAllocatePool2_np(0x40, RING_SIZE as u64, TAG);
    if ring_mem.is_null() {
        return STATUS_INSUFFICIENT_RESOURCES;
    }
    // SAFETY: ring_mem 有效
    let ring = RingBuffer::new(ring_mem.cast(), RING_SIZE);
    // ring 无 Drop（丢弃为 no-op），内存生命周期 = 适配器，由 adapter_release 释放
    (*this).ring = &ring as *const RingBuffer as *mut RingBuffer;
    STATUS_SUCCESS
}

unsafe extern "system" fn adapter_get_device_object(this: PVOID) -> PDEVICE_OBJECT {
    (*(this as *mut AdapterCommon)).device_object
}

unsafe extern "system" fn adapter_get_physical_device_object(this: PVOID) -> PDEVICE_OBJECT {
    (*(this as *mut AdapterCommon)).physical_device_object
}

unsafe extern "system" fn adapter_write_etw_event(
    _this: PVOID,
    _t: u32,
    _a: u64,
    _b: u64,
    _c: u64,
    _d: u64,
) -> NTSTATUS {
    STATUS_SUCCESS
}

unsafe extern "system" fn adapter_set_etw_helper(_this: PVOID, _helper: PVOID) {}

unsafe extern "system" fn adapter_cleanup(this: PVOID) {
    let this = this as *mut AdapterCommon;
    // SAFETY: 释放子设备引用
    if !(*this).mic_port.is_null() {
        crate::com::release_unknown((*this).mic_port);
    }
    if !(*this).speaker_port.is_null() {
        crate::com::release_unknown((*this).speaker_port);
    }
    if !(*this).mic.is_null() {
        crate::miniport::miniport_release((*this).mic.cast());
    }
    if !(*this).speaker.is_null() {
        crate::miniport::miniport_release((*this).speaker.cast());
    }
    (*this).mic = core::ptr::null_mut();
    (*this).speaker = core::ptr::null_mut();
    (*this).mic_port = core::ptr::null_mut();
    (*this).speaker_port = core::ptr::null_mut();
}

/// 创建适配器（单例）
///
/// # Safety
/// device 为有效设备对象。
pub unsafe fn create(device: PDEVICE_OBJECT) -> *mut AdapterCommon {
    if interlocked_increment(core::ptr::addr_of_mut!(INSTANCES)) != 1 {
        interlocked_decrement(core::ptr::addr_of_mut!(INSTANCES));
        return core::ptr::null_mut();
    }
    // SAFETY: 非分页池分配
    let ptr = ExAllocatePool2_np(0x40, size_of::<AdapterCommon>() as u64, TAG);
    if ptr.is_null() {
        INSTANCES = 0;
        return core::ptr::null_mut();
    }
    // SAFETY: 刚分配的内存
    core::ptr::write(
        ptr as *mut AdapterCommon,
        AdapterCommon {
            vtable: &ADAPTER_VTABLE,
            refcount: 1,
            device_object: core::ptr::null_mut(),
            physical_device_object: core::ptr::null_mut(),
            ring: core::ptr::null_mut(),
            mic: core::ptr::null_mut(),
            speaker: core::ptr::null_mut(),
            mic_port: core::ptr::null_mut(),
            speaker_port: core::ptr::null_mut(),
        },
    );
    let this = ptr as *mut AdapterCommon;
    // SAFETY: 初始化适配器
    let st = adapter_init(this.cast(), core::ptr::null_mut(), device);
    if st < 0 {
        ExFreePoolWithTag_np(ptr, TAG);
        INSTANCES = 0;
        return core::ptr::null_mut();
    }
    this
}

/// 安装一个端点（port + miniport + PcRegisterSubdevice）
///
/// # Safety
/// this 必须有效。
unsafe fn install_endpoint(this: *mut AdapterCommon, capture: bool) -> NTSTATUS {
    let adapter = &mut *this;
    // 1) PcNewPort 创建 PortWaveRT
    let mut port: PVOID = core::ptr::null_mut();
    let st = PcNewPort(&mut port, &CLSID_PortWaveRT);
    if st < 0 {
        return st;
    }
    // 2) 创建 MiniportWaveRT
    let miniport = MiniportWaveRT::create(
        adapter as *mut AdapterCommon as PVOID,
        adapter.device_object,
        capture,
    );
    if miniport.is_null() {
        // SAFETY: 释放 port
        crate::com::release_unknown(port);
        return STATUS_INSUFFICIENT_RESOURCES;
    }
    // 3) port->Init(DeviceObject, Irp, miniport, adapter, ResourceList)
    let init_fn = port_iport_init(port);
    let st = init_fn(
        port,
        adapter.device_object,
        core::ptr::null_mut(),
        miniport.cast(),
        adapter as *mut AdapterCommon as PVOID,
        core::ptr::null_mut(),
    );
    if st < 0 {
        crate::com::release_unknown(port);
        crate::miniport::miniport_release(miniport.cast());
        return st;
    }
    // 4) 共享环形缓冲
    let ring = adapter.ring;
    (*miniport).set_ring(ring);
    // 5) PcRegisterSubdevice(DeviceObject, name, port)
    let name_wide: &[u16] = if capture {
        &[
            0x57, 0x61, 0x76, 0x65, 0x43, 0x61, 0x70, 0x74, 0x75, 0x72, 0x65, 0x2d, 0x30, 0x00,
        ] // "WaveCapture-0"
    } else {
        &[
            0x57, 0x61, 0x76, 0x65, 0x52, 0x65, 0x6e, 0x64, 0x65, 0x72, 0x2d, 0x30, 0x00,
        ] // "WaveRender-0"
    };
    let st = PcRegisterSubdevice(adapter.device_object, name_wide.as_ptr(), port);
    if st < 0 {
        crate::com::release_unknown(port);
        crate::miniport::miniport_release(miniport.cast());
        return st;
    }
    // 注册 KSCATEGORY_AUDIO 设备接口（控制面板音频端点可见）
    // SAFETY: pdo 有效；reference 为局部宽字符串，符号链接输出可丢弃
    let pdo = adapter.physical_device_object;
    let mut symbolic = UNICODE_STRING {
        Length: 0,
        MaximumLength: 0,
        Buffer: core::ptr::null_mut(),
    };
    let mut ref_us = UNICODE_STRING {
        Length: (name_wide.len().saturating_sub(1) * 2) as u16,
        MaximumLength: (name_wide.len() * 2) as u16,
        Buffer: name_wide.as_ptr() as PWSTR,
    };
    IoRegisterDeviceInterface(pdo, &KSCATEGORY_AUDIO, &mut ref_us, &mut symbolic);
    if capture {
        adapter.mic = miniport;
        adapter.mic_port = port;
    } else {
        adapter.speaker = miniport;
        adapter.speaker_port = port;
    }
    STATUS_SUCCESS
}

/// 取 IPort 的 Init 方法指针
///
/// # Safety
/// port 必须为有效 IPort。
pub unsafe fn port_iport_init(
    port: PVOID,
) -> unsafe extern "system" fn(PVOID, PDEVICE_OBJECT, PIRP, PVOID, PVOID, *mut c_void) -> NTSTATUS {
    // SAFETY: IPort vtable 布局：QI/AddRef/Release/Init/GetDeviceProperty/NewRegistryKey
    let vtbl = *(port as *const *const IPortVtbl);
    (*vtbl).init
}

/// 安装虚拟声卡（扬声器 + 麦克风 + 配对环回）
///
/// # Safety
/// this 必须有效。
pub unsafe fn install_virtual_cable(this: *mut AdapterCommon) -> NTSTATUS {
    let st_mic = install_endpoint(this, true);
    if st_mic < 0 {
        return st_mic;
    }
    let st_spk = install_endpoint(this, false);
    if st_spk < 0 {
        return st_spk;
    }
    // 配对：麦克风小端口持有扬声器小端口（环回写入）
    // SAFETY: 两者已创建
    let mic = (*this).mic;
    let spk = (*this).speaker;
    (*mic).set_paired(spk);
    (*spk).set_paired(mic);
    STATUS_SUCCESS
}
