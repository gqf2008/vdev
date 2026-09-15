//! AdapterCommon：适配器对象 + 虚拟声卡（扬声器→麦克风环回）安装流程
#![allow(non_snake_case, non_camel_case_types)]
#![allow(clippy::missing_errors_doc)]

use core::mem::size_of;
use core::sync::atomic::{AtomicU32, Ordering};

use crate::com::{interlocked_decrement, interlocked_increment};
use crate::miniport::MiniportWaveRT;
use crate::ringbuffer::RingBuffer;
use crate::sys::mem::{ExAllocatePool2_np, ExFreePoolWithTag_np};
use crate::sys::portcls::*;
use crate::sys::types::*;
use crate::topology::MiniportTopology;

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
    /// 环形缓冲对象（B1：与 ring_pool 同属一块池分配，ring 指向池基址）
    pub ring: *mut RingBuffer,
    /// B1：环形缓冲的唯一池指针（RingBuffer 头 + 数据区一次分配），释放时只 free 它
    pub ring_pool: *mut c_void,
    pub mic: *mut MiniportWaveRT,
    pub speaker: *mut MiniportWaveRT,
    pub mic_port: PVOID,
    pub speaker_port: PVOID,
    /// topology 小端口（音频端点拓扑；驱动侧引用，随适配器释放）
    pub topo_mic: *mut MiniportTopology,
    pub topo_speaker: *mut MiniportTopology,
    /// topology 子设备的 port 对象（PortCls 持有；teardown 注销用）
    pub topo_mic_port: PVOID,
    pub topo_speaker_port: PVOID,
    /// 物理（wave↔topology）连接是否已注册——teardown 按此注销
    pub phys_render_connected: bool,
    pub phys_capture_connected: bool,
}

// 单例：一次只允许一个适配器（全程原子访问；修复前与普通写 `INSTANCES = 0` 混用）
static INSTANCES: AtomicU32 = AtomicU32::new(0);

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
        // minor e：COM 约定 E_NOINTERFACE（不是 STATUS_INVALID_PARAMETER）
        *obj = core::ptr::null_mut();
        E_NOINTERFACE
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
        // SAFETY: 释放环形缓冲（B1：单块池 = RingBuffer 头 + 数据区，free 唯一池指针）
        // 与子对象
        if !(*this).ring_pool.is_null() {
            ExFreePoolWithTag_np((*this).ring_pool, TAG);
        }
        if !(*this).mic.is_null() {
            crate::miniport::miniport_release((*this).mic.cast());
        }
        if !(*this).speaker.is_null() {
            crate::miniport::miniport_release((*this).speaker.cast());
        }
        if !(*this).topo_mic.is_null() {
            // SAFETY: topology 小端口头部即 vtable 指针，release_unknown 调其 Release
            crate::com::release_unknown((*this).topo_mic.cast());
        }
        if !(*this).topo_speaker.is_null() {
            // SAFETY: 同上
            crate::com::release_unknown((*this).topo_speaker.cast());
        }
        ExFreePoolWithTag_np(this.cast(), TAG);
        INSTANCES.store(0, Ordering::SeqCst);
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
    // 分配共享环形缓冲——B1：单块池分配（RingBuffer 头 + 数据区），
    // 结构体 ptr::write 到池基址。旧实现在栈上构造 RingBuffer 后存其地址，
    // init 返回即悬垂，首次使用必蓝屏。
    //
    // 容量即"渲染写入 → 采集读出"的积压上限。**注意单位**：环里搬的是**设备格式**
    // （16bit/48k/2ch = 192000 B/s），不是 32bit 浮点混音格式（384000 B/s）——
    // 按后者算会把秒数少一半（早期文档里的 "1.36 s" 就是这么错的）：
    //   1 MB   ≈ 5.46 s   ← 原值；真机实测（agent 连续写满环 + 标记音后沿）≈ 5.14 s
    //   256 KB ≈ 1.37 s   ← 现值：把环回积压压到交互可接受，同时仍远大于
    //                       一个 20 ms 引擎缓冲 + 一个 DMA 周期，留足抖动余量
    // 再小（64 KB ≈ 0.34 s）开始受调度抖动影响，故不继续压。
    const RING_SIZE: usize = 256 * 1024;
    let pool = ExAllocatePool2_np(0x40, (size_of::<RingBuffer>() + RING_SIZE) as u64, TAG);
    if pool.is_null() {
        return STATUS_INSUFFICIENT_RESOURCES;
    }
    let header = pool.cast::<RingBuffer>();
    // SAFETY: pool 有效且容量 = size_of::<RingBuffer>() + RING_SIZE
    let data = unsafe { (pool as *mut u8).add(size_of::<RingBuffer>()) };
    // SAFETY: data 指向紧跟其后的 RING_SIZE 字节有效非分页内存
    unsafe { core::ptr::write(header, RingBuffer::new(data, RING_SIZE)) };
    // RingBuffer 无 Drop；内存生命周期 = 适配器，由 adapter_release 释放 ring_pool
    // SAFETY: 单线程初始化
    unsafe {
        (*this).ring = header;
        (*this).ring_pool = pool;
    }
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
    // SAFETY: 释放驱动持有的 miniport 引用（port 由 PortCls 持有，不在此释放）。
    // wave 与 topology 对称释放（审查 M-e 修复）：原实现 cleanup 只置 NULL 不
    // Release topology 两个驱动侧引用，若 PortCls 先 Cleanup 后最终 Release，
    // 这两个引用永久泄漏（wave 路径两条路都放，行为不对称）。
    if !(*this).mic.is_null() {
        crate::miniport::miniport_release((*this).mic.cast());
    }
    if !(*this).speaker.is_null() {
        crate::miniport::miniport_release((*this).speaker.cast());
    }
    if !(*this).topo_mic.is_null() {
        // SAFETY: topology 小端口头部即 vtable 指针，release_unknown 调其 Release
        crate::com::release_unknown((*this).topo_mic.cast());
    }
    if !(*this).topo_speaker.is_null() {
        // SAFETY: 同上
        crate::com::release_unknown((*this).topo_speaker.cast());
    }
    (*this).mic = core::ptr::null_mut();
    (*this).speaker = core::ptr::null_mut();
    (*this).mic_port = core::ptr::null_mut();
    (*this).speaker_port = core::ptr::null_mut();
    (*this).topo_mic = core::ptr::null_mut();
    (*this).topo_speaker = core::ptr::null_mut();
    (*this).topo_mic_port = core::ptr::null_mut();
    (*this).topo_speaker_port = core::ptr::null_mut();
    (*this).phys_render_connected = false;
    (*this).phys_capture_connected = false;
}

/// 创建适配器（单例）
///
/// # Safety
/// device 为有效设备对象。
pub unsafe fn create(device: PDEVICE_OBJECT) -> *mut AdapterCommon {
    if INSTANCES.fetch_add(1, Ordering::SeqCst) != 0 {
        INSTANCES.fetch_sub(1, Ordering::SeqCst);
        return core::ptr::null_mut();
    }
    // SAFETY: 非分页池分配
    let ptr = ExAllocatePool2_np(0x40, size_of::<AdapterCommon>() as u64, TAG);
    if ptr.is_null() {
        INSTANCES.store(0, Ordering::SeqCst);
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
            ring_pool: core::ptr::null_mut(),
            mic: core::ptr::null_mut(),
            speaker: core::ptr::null_mut(),
            mic_port: core::ptr::null_mut(),
            speaker_port: core::ptr::null_mut(),
            topo_mic: core::ptr::null_mut(),
            topo_speaker: core::ptr::null_mut(),
            topo_mic_port: core::ptr::null_mut(),
            topo_speaker_port: core::ptr::null_mut(),
            phys_render_connected: false,
            phys_capture_connected: false,
        },
    );
    let this = ptr as *mut AdapterCommon;
    // SAFETY: 初始化适配器
    let st = adapter_init(this.cast(), core::ptr::null_mut(), device);
    if st < 0 {
        ExFreePoolWithTag_np(ptr, TAG);
        INSTANCES.store(0, Ordering::SeqCst);
        return core::ptr::null_mut();
    }
    this
}

// 子设备注册名与物理连接 pin 常量在 endpoint_names 模块（无条件编译，
// 宿主 #[test] 可校验其与 INF 模板/pin 布局的一致性）
use crate::endpoint_names::{
    TOPO_CAPTURE_TO_WAVE_PIN, TOPO_RENDER_FROM_WAVE_PIN, TOPOLOGY_CAPTURE_NAME,
    TOPOLOGY_RENDER_NAME, WAVE_CAPTURE_BRIDGE_PIN, WAVE_CAPTURE_NAME, WAVE_RENDER_BRIDGE_PIN,
    WAVE_RENDER_NAME,
};

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
    // 2) 创建 MiniportWaveRT（refcount=1，PcNewPort 的返回值所有权归驱动）
    let miniport = MiniportWaveRT::create(
        adapter as *mut AdapterCommon as PVOID,
        adapter.device_object,
        capture,
    );
    if miniport.is_null() {
        // SAFETY: 释放驱动持有的 port 引用
        crate::com::release_unknown(port);
        return STATUS_INSUFFICIENT_RESOURCES;
    }
    // 所有权（M4）：MiniportWaveRT::create 返回 refcount=1 的引用，归适配器持有；
    // port->Init 成功时 PortCls 的 port 对象另持一份（内部 AddRef→2）。此处不再
    // 额外 AddRef——多加的那份谁也不释放，adapter_release 永远减不到 0（永久泄漏）。
    // 驱动侧引用由 adapter_cleanup 释放；PortCls 侧随 port 对象销毁释放。
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
        // SAFETY: 释放驱动持有的 port 与 miniport 引用
        crate::com::release_unknown(port);
        crate::miniport::miniport_release(miniport.cast());
        return st;
    }
    // 4) 共享环形缓冲
    let ring = adapter.ring;
    (*miniport).set_ring(ring);
    // 5) PcRegisterSubdevice(DeviceObject, name, port)
    let name_wide: &[u16] = if capture {
        WAVE_CAPTURE_NAME
    } else {
        WAVE_RENDER_NAME
    };
    let st = PcRegisterSubdevice(adapter.device_object, name_wide.as_ptr(), port);
    if st < 0 {
        // SAFETY: PcRegisterSubdevice 失败则 port 未接管；释放驱动持有的两份引用
        crate::com::release_unknown(port);
        crate::miniport::miniport_release(miniport.cast());
        return st;
    }
    // PcRegisterSubdevice 成功后 PortCls 持有 port 引用；释放驱动那份 PcNewPort 引用
    // SAFETY: port 仍有效（PortCls 持有）
    crate::com::release_unknown(port);
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

/// 安装一个 topology 端点（PortTopology + MiniportTopology + PcRegisterSubdevice）
///
/// 顺序对照 sysvad common.cpp InstallEndpointFilters：PcNewPort → 创建小端口 →
/// port->Init(DeviceObject, Irp, miniport, adapter, ResourceList) →
/// PcRegisterSubdevice。IPortTopology 与 IPortWaveRT 共享同一 IPort::Init
/// 槽位（vtable 第 4 项），故复用 port_iport_init。
///
/// # Safety
/// this 必须有效。
unsafe fn install_topology_endpoint(this: *mut AdapterCommon, capture: bool) -> NTSTATUS {
    let adapter = &mut *this;
    // 1) PcNewPort 创建 PortTopology
    let mut port: PVOID = core::ptr::null_mut();
    let st = PcNewPort(&mut port, &CLSID_PortTopology);
    if st < 0 {
        return st;
    }
    // 2) 创建 MiniportTopology（refcount=1，引用归驱动持有）
    let miniport = MiniportTopology::create(capture);
    if miniport.is_null() {
        // SAFETY: 释放驱动持有的 port 引用
        crate::com::release_unknown(port);
        return STATUS_INSUFFICIENT_RESOURCES;
    }
    // 3) port->Init 成功时 PortCls 的 port 对象另持一份 miniport 引用（同 wave 路径）
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
        // SAFETY: Init 失败则 port 未接管 miniport；释放驱动持有的两份引用
        crate::com::release_unknown(port);
        crate::com::release_unknown(miniport.cast());
        return st;
    }
    // 4) PcRegisterSubdevice(DeviceObject, name, port)
    let name_wide: &[u16] = if capture {
        TOPOLOGY_CAPTURE_NAME
    } else {
        TOPOLOGY_RENDER_NAME
    };
    let st = PcRegisterSubdevice(adapter.device_object, name_wide.as_ptr(), port);
    if st < 0 {
        // SAFETY: PcRegisterSubdevice 失败则 port 未接管；释放两份驱动引用
        crate::com::release_unknown(port);
        crate::com::release_unknown(miniport.cast());
        return st;
    }
    // PcRegisterSubdevice 成功后 PortCls 持有 port 引用；释放驱动那份 PcNewPort 引用
    // SAFETY: port 仍有效（PortCls 持有）
    crate::com::release_unknown(port);
    if capture {
        adapter.topo_mic = miniport;
        adapter.topo_mic_port = port;
    } else {
        adapter.topo_speaker = miniport;
        adapter.topo_speaker_port = port;
    }
    STATUS_SUCCESS
}

/// M8：安装失败路径的端点注销——先经 IUnregisterPhysicalConnection 解除
/// wave↔topology 物理连接（sysvad DisconnectTopologies 先于子设备注销，且
/// 参数须与注册时完全一致），再经 IUnregisterSubdevice 解除 PortCls 持有的
/// 子设备引用（port 对象随之销毁，其对 miniport 的引用一并解除），随后
/// start_device 再 release 适配器；否则子设备链里的 port/miniport 反向引用
/// 已释放的适配器内存，后续访问即 UAF。
///
/// # Safety
/// this 必须有效；仅应在 install_virtual_cable 失败路径调用（成功路径的
/// 生命周期由 PortCls/适配器析构顺序管理）。
unsafe fn teardown_endpoints(this: *mut AdapterCommon) {
    // SAFETY: 调用方保证 this 有效
    let adapter = &mut *this;
    // 1) 注销物理连接（best-effort，失败继续）
    if adapter.phys_render_connected {
        // SAFETY: 两端为已注册子设备的有效 port 对象
        unsafe {
            pc_unregister_physical_connection(
                adapter.device_object,
                adapter.speaker_port,
                WAVE_RENDER_BRIDGE_PIN,
                adapter.topo_speaker_port,
                TOPO_RENDER_FROM_WAVE_PIN,
            )
        };
        adapter.phys_render_connected = false;
    }
    if adapter.phys_capture_connected {
        // SAFETY: 同上
        unsafe {
            pc_unregister_physical_connection(
                adapter.device_object,
                adapter.topo_mic_port,
                TOPO_CAPTURE_TO_WAVE_PIN,
                adapter.mic_port,
                WAVE_CAPTURE_BRIDGE_PIN,
            )
        };
        adapter.phys_capture_connected = false;
    }
    // 2) 注销子设备
    if !adapter.topo_mic_port.is_null() {
        // SAFETY: port 为尚未注销的有效子设备 COM 对象
        unsafe { pc_unregister_subdevice(adapter.device_object, adapter.topo_mic_port) };
        adapter.topo_mic_port = core::ptr::null_mut();
    }
    if !adapter.topo_speaker_port.is_null() {
        // SAFETY: 同上
        unsafe { pc_unregister_subdevice(adapter.device_object, adapter.topo_speaker_port) };
        adapter.topo_speaker_port = core::ptr::null_mut();
    }
    if !adapter.mic_port.is_null() {
        // SAFETY: port 为尚未注销的有效子设备 COM 对象
        unsafe { pc_unregister_subdevice(adapter.device_object, adapter.mic_port) };
        adapter.mic_port = core::ptr::null_mut();
    }
    if !adapter.speaker_port.is_null() {
        // SAFETY: 同上
        unsafe { pc_unregister_subdevice(adapter.device_object, adapter.speaker_port) };
        adapter.speaker_port = core::ptr::null_mut();
    }
}

/// 安装虚拟声卡（扬声器 + 麦克风 + 配对环回 + 端点拓扑 + wave↔topology 连接）
///
/// 顺序对照 sysvad CAdapterCommon::InstallDevice：先注册全部子设备
///（wave capture/render，topology capture/render），再 ConnectTopologies 注册
/// 两条物理连接；任一步失败沿注册逆序 teardown。
///
/// # Safety
/// this 必须有效。
pub unsafe fn install_virtual_cable(this: *mut AdapterCommon) -> NTSTATUS {
    // SAFETY: 调用方保证 this 有效
    let st_mic = unsafe { install_endpoint(this, true) };
    if st_mic < 0 {
        return st_mic;
    }
    // SAFETY: 调用方保证 this 有效
    let st_spk = unsafe { install_endpoint(this, false) };
    if st_spk < 0 {
        // M8：第二端点安装失败——先注销已注册的麦克风子设备再返回
        unsafe { teardown_endpoints(this) };
        return st_spk;
    }
    // topology 端点（播放/录音设备的音频端点拓扑）
    // SAFETY: 调用方保证 this 有效
    let st_topo_cap = unsafe { install_topology_endpoint(this, true) };
    if st_topo_cap < 0 {
        unsafe { teardown_endpoints(this) };
        return st_topo_cap;
    }
    // SAFETY: 调用方保证 this 有效
    let st_topo_ren = unsafe { install_topology_endpoint(this, false) };
    if st_topo_ren < 0 {
        unsafe { teardown_endpoints(this) };
        return st_topo_ren;
    }
    // 配对：麦克风小端口持有扬声器小端口（环回写入）
    // SAFETY: 两者已创建
    let mic = (*this).mic;
    let spk = (*this).speaker;
    (*mic).set_paired(spk);
    (*spk).set_paired(mic);
    // wave↔topology 物理（filter 间）连接——对照 sysvad ConnectTopologies
    //（common.cpp:2023-2052）：CONNECTIONTYPE_WAVE_OUTPUT 为 render 路径
    //（wave → topology），CONNECTIONTYPE_TOPOLOGY_OUTPUT 为 capture 路径
    //（topology → wave）；PcRegisterPhysicalConnection 参数顺序为
    //(DeviceObject, From, FromPin, To, ToPin)（portcls.h:3888）。
    // Windows 运行时行为（PortCls filter 图构建、端点枚举）无法宿主单测，
    // 仅交叉编译 + 真机验证。
    // SAFETY: 调用方保证 this 有效，四个 port 均已注册
    let st = PcRegisterPhysicalConnection(
        (*this).device_object,
        (*this).speaker_port,
        WAVE_RENDER_BRIDGE_PIN,
        (*this).topo_speaker_port,
        TOPO_RENDER_FROM_WAVE_PIN,
    );
    if st < 0 {
        unsafe { teardown_endpoints(this) };
        return st;
    }
    (*this).phys_render_connected = true;
    // SAFETY: 同上
    let st = PcRegisterPhysicalConnection(
        (*this).device_object,
        (*this).topo_mic_port,
        TOPO_CAPTURE_TO_WAVE_PIN,
        (*this).mic_port,
        WAVE_CAPTURE_BRIDGE_PIN,
    );
    if st < 0 {
        unsafe { teardown_endpoints(this) };
        return st;
    }
    (*this).phys_capture_connected = true;
    STATUS_SUCCESS
}
