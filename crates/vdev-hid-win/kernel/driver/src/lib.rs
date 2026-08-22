#![no_std]
//! vdev 虚拟 HID（路线 B）：KMDF 内核 HID minidriver（移植自 Microsoft vhidmini2）
//! 暴露一个标准键盘 HID 设备（Boot Keyboard）；用户态经 WriteFile 注入 8 字节键盘报告，
//! 驱动把报告投递给 hidclass，被系统作为真实 HID 键盘消费。

mod hid;

use core::cell::UnsafeCell;
use core::mem::size_of;
use core::sync::atomic::{AtomicBool, Ordering};

use hid::{HID_DESCRIPTOR, HID_DEVICE_ATTRIBUTES, HID_XFER_PACKET, KEYBOARD_REPORT_DESCRIPTOR};
use wdk_sys::{
    DRIVER_OBJECT, NTSTATUS, PCUNICODE_STRING, PDRIVER_OBJECT, PVOID, ULONG, WDF_DRIVER_CONFIG,
    WDF_IO_QUEUE_CONFIG, WDF_NO_HANDLE, WDF_NO_OBJECT_ATTRIBUTES, WDFDEVICE, WDFDEVICE_INIT,
    WDFDRIVER, WDFMEMORY, WDFQUEUE, WDFREQUEST, call_unsafe_wdf_function_binding,
};

/// HID 设备标识（"VD"/"HI"）
const VID: u16 = 0x5644;
const PID: u16 = 0x4849;

/// NTSTATUS 常量（wdk-sys 未生成）
const STATUS_SUCCESS: NTSTATUS = 0;
const STATUS_INVALID_DEVICE_REQUEST: NTSTATUS = 0xC000_0010u32 as NTSTATUS;
const STATUS_INVALID_PARAMETER: NTSTATUS = 0xC000_000Du32 as NTSTATUS;
const STATUS_INVALID_BUFFER_SIZE: NTSTATUS = 0xC000_020Cu32 as NTSTATUS;

/// 键盘输入报告：8 字节（1 修饰键 + 1 保留 + 6 按键）
const KEYBOARD_REPORT_SIZE: usize = 8;

static HID_DESC: HID_DESCRIPTOR = HID_DESCRIPTOR {
    bLength: size_of::<HID_DESCRIPTOR>() as u8,
    bDescriptorType: 0x21,
    bcdHID: 0x0100,
    bCountryCode: 0,
    bNumDescriptors: 1,
    DescriptorList: [hid::HID_DESCRIPTOR_LIST_ENTRY {
        bDescriptorType: 0x22,
        wDescriptorLength: KEYBOARD_REPORT_DESCRIPTOR.len() as u16,
    }],
};

static HID_ATTR: HID_DEVICE_ATTRIBUTES = HID_DEVICE_ATTRIBUTES {
    Size: size_of::<HID_DEVICE_ATTRIBUTES>() as u32,
    VendorID: VID,
    ProductID: PID,
    VersionNumber: 0x0100,
};

// ---------------- 设备状态（单实例虚拟键盘；自旋锁保护） ----------------

/// 键盘设备状态：最近注入的报告 + 挂起的 READ_REPORT 队列
struct DeviceState {
    /// 最近一次注入的键盘报告
    report: [u8; KEYBOARD_REPORT_SIZE],
    /// 是否有已注入但尚未投递给 hidclass 的报告
    report_ready: bool,
    /// manual 队列（挂起等待报告的 IOCTL_HID_READ_REPORT）
    manual_queue: WDFQUEUE,
}

/// 自旋锁 + 状态单元（可在 DISPATCH_LEVEL 下访问）
struct StateCell {
    lock: AtomicBool,
    state: UnsafeCell<DeviceState>,
}

// SAFETY: state 仅在持有自旋锁时访问；WDFQUEUE 与 [u8; 8] 均可在内核线程间共享
unsafe impl Sync for StateCell {}

static STATE: StateCell = StateCell {
    lock: AtomicBool::new(false),
    state: UnsafeCell::new(DeviceState {
        report: [0; KEYBOARD_REPORT_SIZE],
        report_ready: false,
        manual_queue: core::ptr::null_mut(),
    }),
};

/// 自旋锁守卫（RAII 释放）
struct StateGuard<'a>(&'a StateCell);

impl StateCell {
    fn lock(&self) -> StateGuard<'_> {
        while self
            .lock
            .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            core::hint::spin_loop();
        }
        StateGuard(self)
    }
}

impl core::ops::Deref for StateGuard<'_> {
    type Target = DeviceState;
    fn deref(&self) -> &DeviceState {
        // SAFETY: 已持有自旋锁，临界区内唯一访问者
        unsafe { &*self.0.state.get() }
    }
}

impl core::ops::DerefMut for StateGuard<'_> {
    fn deref_mut(&mut self) -> &mut DeviceState {
        // SAFETY: 已持有自旋锁，临界区内唯一访问者
        unsafe { &mut *self.0.state.get() }
    }
}

impl Drop for StateGuard<'_> {
    fn drop(&mut self) {
        self.0.lock.store(false, Ordering::Release);
    }
}

// ---------------- 设备添加 ----------------

/// 设备添加回调：创建设备 + 默认并行队列（处理 hidclass 的 IOCTL_HID_*）+ manual 队列
///
/// # Safety
/// 由 WDF 调用，`driver`/`init` 为有效句柄。
unsafe extern "C" fn evt_device_add(_driver: WDFDRIVER, mut init: *mut WDFDEVICE_INIT) -> NTSTATUS {
    // 标记为过滤器（hidclass 之上；放弃电源策略所有权，与 vhidmini 一致）
    // SAFETY: init 有效
    unsafe {
        call_unsafe_wdf_function_binding!(WdfFdoInitSetFilter, init);
    }

    let mut attributes = wdk_sys::WDF_OBJECT_ATTRIBUTES {
        Size: size_of::<wdk_sys::WDF_OBJECT_ATTRIBUTES>() as ULONG,
        ..wdk_sys::WDF_OBJECT_ATTRIBUTES::default()
    };
    let mut device: WDFDEVICE = core::ptr::null_mut();
    // SAFETY: init/attributes/device 有效
    let status = unsafe {
        call_unsafe_wdf_function_binding!(WdfDeviceCreate, &mut init, &mut attributes, &mut device)
    };
    if status < 0 {
        return status;
    }

    // 默认并行队列：接收 hidclass 的 IOCTL_HID_*（READ_REPORT 会转发到 manual 队列）
    let mut queue_config = WDF_IO_QUEUE_CONFIG {
        Size: size_of::<WDF_IO_QUEUE_CONFIG>() as ULONG,
        DispatchType: wdk_sys::_WDF_IO_QUEUE_DISPATCH_TYPE::WdfIoQueueDispatchParallel,
        PowerManaged: 1, // WdfTrue
        AllowZeroLengthRequests: 1,
        DefaultQueue: 1,
        EvtIoInternalDeviceControl: Some(evt_io_internal_device_control),
        ..WDF_IO_QUEUE_CONFIG::default()
    };
    let mut queue_attr = wdk_sys::WDF_OBJECT_ATTRIBUTES {
        Size: size_of::<wdk_sys::WDF_OBJECT_ATTRIBUTES>() as ULONG,
        ..wdk_sys::WDF_OBJECT_ATTRIBUTES::default()
    };
    let mut queue: WDFQUEUE = core::ptr::null_mut();
    let status = unsafe {
        call_unsafe_wdf_function_binding!(
            WdfIoQueueCreate,
            device,
            &mut queue_config,
            &mut queue_attr,
            &mut queue
        )
    };
    if status < 0 {
        return status;
    }

    // manual 队列：挂起 READ_REPORT，等注入后由注入路径取回并完成
    let mut manual_config = WDF_IO_QUEUE_CONFIG {
        Size: size_of::<WDF_IO_QUEUE_CONFIG>() as ULONG,
        DispatchType: wdk_sys::_WDF_IO_QUEUE_DISPATCH_TYPE::WdfIoQueueDispatchManual,
        PowerManaged: 0, // WdfFalse：manual 队列不做电源管理
        ..WDF_IO_QUEUE_CONFIG::default()
    };
    let mut manual_attr = wdk_sys::WDF_OBJECT_ATTRIBUTES {
        Size: size_of::<wdk_sys::WDF_OBJECT_ATTRIBUTES>() as ULONG,
        ..wdk_sys::WDF_OBJECT_ATTRIBUTES::default()
    };
    let mut manual_queue: WDFQUEUE = core::ptr::null_mut();
    let status = unsafe {
        call_unsafe_wdf_function_binding!(
            WdfIoQueueCreate,
            device,
            &mut manual_config,
            &mut manual_attr,
            &mut manual_queue
        )
    };
    if status < 0 {
        return status;
    }

    STATE.lock().manual_queue = manual_queue;
    STATUS_SUCCESS
}

// ---------------- IOCTL 分发 ----------------

/// 分发结果：Complete(status) 由调用方完成请求；Pending 表示请求已挂起（注入路径完成）
enum DispatchResult {
    Complete(NTSTATUS),
    Pending,
}

/// 内部设备控制：hidclass 的 IOCTL_HID_* 契约
///
/// # Safety
/// 由 WDF 队列分发，参数为有效句柄。
unsafe extern "C" fn evt_io_internal_device_control(
    _queue: WDFQUEUE,
    request: WDFREQUEST,
    output_len: usize,
    input_len: usize,
    ioctl_code: u32,
) {
    // SAFETY: request 有效
    let result = unsafe { dispatch_ioctl(request, output_len, input_len, ioctl_code) };
    if let DispatchResult::Complete(status) = result {
        // SAFETY: 完成请求
        unsafe {
            call_unsafe_wdf_function_binding!(
                WdfRequestCompleteWithInformation,
                request,
                status,
                0
            );
        }
    }
}

/// 分发 HID IOCTL
///
/// # Safety
/// request 必须为有效 WDFREQUEST。
unsafe fn dispatch_ioctl(
    request: WDFREQUEST,
    output_len: usize,
    _input_len: usize,
    code: u32,
) -> DispatchResult {
    match code {
        hid::IOCTL_HID_GET_DEVICE_DESCRIPTOR => {
            // SAFETY: 输出缓冲区由 hidclass 提供
            DispatchResult::Complete(unsafe { copy_to_output(request, &HID_DESC) })
        }
        hid::IOCTL_HID_GET_DEVICE_ATTRIBUTES => {
            // SAFETY: 输出缓冲区由 hidclass 提供
            DispatchResult::Complete(unsafe { copy_to_output(request, &HID_ATTR) })
        }
        hid::IOCTL_HID_GET_REPORT_DESCRIPTOR => {
            // SAFETY: 输出缓冲区由 hidclass 提供
            DispatchResult::Complete(unsafe {
                copy_to_output(request, &KEYBOARD_REPORT_DESCRIPTOR)
            })
        }
        hid::IOCTL_HID_READ_REPORT => {
            // 输入报告：已有注入则立即投递，否则挂 manual 队列等注入
            unsafe { handle_read_report(request, output_len) }
        }
        hid::IOCTL_HID_WRITE_REPORT | hid::IOCTL_HID_SET_OUTPUT_REPORT => {
            // 注入入口：接收 8 字节键盘报告并投递
            DispatchResult::Complete(unsafe { handle_inject(request) })
        }
        hid::IOCTL_HID_GET_INPUT_REPORT => {
            // HidD_GetInputReport：返回当前状态
            DispatchResult::Complete(unsafe { handle_get_input_report(request) })
        }
        hid::IOCTL_HID_GET_FEATURE | hid::IOCTL_HID_SET_FEATURE => {
            // 无 feature 报告；空操作
            DispatchResult::Complete(STATUS_SUCCESS)
        }
        hid::IOCTL_HID_GET_STRING => {
            // 设备名由 INF 提供；返回空串即可
            DispatchResult::Complete(STATUS_SUCCESS)
        }
        _ => DispatchResult::Complete(STATUS_INVALID_DEVICE_REQUEST),
    }
}

/// IOCTL_HID_READ_REPORT：投递一个键盘输入报告
///
/// # Safety
/// request 为有效 WDFREQUEST。
unsafe fn handle_read_report(request: WDFREQUEST, _output_len: usize) -> DispatchResult {
    // 若已有注入报告，直接投递
    {
        let mut g = STATE.lock();
        if g.report_ready {
            g.report_ready = false;
            let report = g.report;
            // SAFETY: 输出缓冲区由 hidclass 提供
            return DispatchResult::Complete(unsafe { copy_to_output(request, &report) });
        }
    }
    // 否则挂到 manual 队列等待注入
    let queue = STATE.lock().manual_queue;
    let status =
        unsafe { call_unsafe_wdf_function_binding!(WdfRequestForwardToIoQueue, request, queue) };
    if status < 0 {
        return DispatchResult::Complete(status);
    }
    // 竞态收尾：转发期间恰好有注入到达时，manual 队列至多一个挂起请求，
    // 取回（就是本请求）并立即投递，避免永久挂起。
    let mut req: WDFREQUEST = core::ptr::null_mut();
    let mut report = [0u8; KEYBOARD_REPORT_SIZE];
    let mut ready = false;
    {
        let mut g = STATE.lock();
        if g.report_ready {
            let status = unsafe {
                call_unsafe_wdf_function_binding!(
                    WdfIoQueueRetrieveNextRequest,
                    g.manual_queue,
                    &mut req
                )
            };
            if status >= 0 && !req.is_null() {
                g.report_ready = false;
                report = g.report;
                ready = true;
            }
        }
    }
    if ready {
        // 取回的就是本请求（hidclass 同一时刻至多一个挂起读）；自行完成，不再交还调用方
        // SAFETY: req 有效，输出缓冲区由 hidclass 提供
        let status = unsafe { copy_to_output(req, &report) };
        unsafe {
            call_unsafe_wdf_function_binding!(WdfRequestCompleteWithInformation, req, status, 0);
        }
        return DispatchResult::Pending;
    }
    DispatchResult::Pending
}

/// 注入入口：解析 HID_XFER_PACKET，把 8 字节键盘报告写入状态并投递
///
/// # Safety
/// request 为有效 WDFREQUEST。
unsafe fn handle_inject(request: WDFREQUEST) -> NTSTATUS {
    let mut packet = HID_XFER_PACKET::default();
    let status = unsafe { retrieve_packet(request, true, &mut packet) };
    if status < 0 {
        return status;
    }
    if packet.reportBuffer.is_null() {
        return STATUS_INVALID_PARAMETER;
    }
    if (packet.reportBufferLen as usize) < KEYBOARD_REPORT_SIZE {
        return STATUS_INVALID_BUFFER_SIZE;
    }
    let mut report = [0u8; KEYBOARD_REPORT_SIZE];
    // SAFETY: reportBuffer 指向 hidclass 提供的已锁定内核缓冲区，长度 >= 8
    unsafe {
        core::ptr::copy_nonoverlapping(
            packet.reportBuffer.cast::<u8>(),
            report.as_mut_ptr(),
            KEYBOARD_REPORT_SIZE,
        );
    }
    inject_report(report);
    // SAFETY: 记录已消费字节数
    unsafe {
        call_unsafe_wdf_function_binding!(
            WdfRequestSetInformation,
            request,
            KEYBOARD_REPORT_SIZE as u64
        );
    }
    STATUS_SUCCESS
}

/// HidD_GetInputReport：把当前键盘报告拷贝到 packet.reportBuffer
///
/// # Safety
/// request 为有效 WDFREQUEST。
unsafe fn handle_get_input_report(request: WDFREQUEST) -> NTSTATUS {
    let mut packet = HID_XFER_PACKET::default();
    let status = unsafe { retrieve_packet(request, false, &mut packet) };
    if status < 0 {
        return status;
    }
    if packet.reportBuffer.is_null() {
        return STATUS_INVALID_PARAMETER;
    }
    if (packet.reportBufferLen as usize) < KEYBOARD_REPORT_SIZE {
        return STATUS_INVALID_BUFFER_SIZE;
    }
    let report = STATE.lock().report;
    // SAFETY: reportBuffer 指向 hidclass 提供的已锁定输出缓冲区，长度 >= 8
    unsafe {
        core::ptr::copy_nonoverlapping(
            report.as_ptr(),
            packet.reportBuffer.cast::<u8>(),
            KEYBOARD_REPORT_SIZE,
        );
    }
    // SAFETY: 记录写入字节数
    unsafe {
        call_unsafe_wdf_function_binding!(
            WdfRequestSetInformation,
            request,
            KEYBOARD_REPORT_SIZE as u64
        );
    }
    STATUS_SUCCESS
}

/// 把注入报告写入状态，并投递给挂起的 READ_REPORT（若有）
fn inject_report(report: [u8; KEYBOARD_REPORT_SIZE]) {
    let mut req: WDFREQUEST = core::ptr::null_mut();
    {
        let mut g = STATE.lock();
        g.report = report;
        g.report_ready = true;
        let status = unsafe {
            call_unsafe_wdf_function_binding!(
                WdfIoQueueRetrieveNextRequest,
                g.manual_queue,
                &mut req
            )
        };
        if status >= 0 && !req.is_null() {
            g.report_ready = false;
        }
    }
    if !req.is_null() {
        // 完成挂起的 READ_REPORT；hidclass 同一时刻至多一个挂起读
        // SAFETY: req 有效
        let status = unsafe { copy_to_output(req, &report) };
        unsafe {
            call_unsafe_wdf_function_binding!(WdfRequestCompleteWithInformation, req, status, 0);
        }
    }
}

// ---------------- 缓冲区辅助 ----------------

/// 从请求输入/输出缓冲区取 HID_XFER_PACKET
///
/// # Safety
/// request 为有效 WDFREQUEST。
unsafe fn retrieve_packet(
    request: WDFREQUEST,
    from_input: bool,
    packet: &mut HID_XFER_PACKET,
) -> NTSTATUS {
    let mut memory: WDFMEMORY = core::ptr::null_mut();
    let status = if from_input {
        // SAFETY: 取输入缓冲区
        unsafe {
            call_unsafe_wdf_function_binding!(WdfRequestRetrieveInputMemory, request, &mut memory)
        }
    } else {
        // SAFETY: 取输出缓冲区
        unsafe {
            call_unsafe_wdf_function_binding!(WdfRequestRetrieveOutputMemory, request, &mut memory)
        }
    };
    if status < 0 {
        return status;
    }
    let mut len: usize = 0;
    // SAFETY: memory 有效
    let buf: PVOID =
        unsafe { call_unsafe_wdf_function_binding!(WdfMemoryGetBuffer, memory, &mut len) };
    if len < size_of::<HID_XFER_PACKET>() {
        return STATUS_INVALID_BUFFER_SIZE;
    }
    // SAFETY: buf 指向 hidclass 传入的 HID_XFER_PACKET，长度 >= sizeof
    unsafe {
        core::ptr::copy_nonoverlapping(
            buf.cast::<u8>(),
            (packet as *mut HID_XFER_PACKET).cast::<u8>(),
            size_of::<HID_XFER_PACKET>(),
        );
    }
    STATUS_SUCCESS
}

/// 把固定数据拷到请求输出缓冲区（结构体或字节切片均可），并记录信息字节数
///
/// # Safety
/// request 有效；data 指向有效内存。
unsafe fn copy_to_output<T: ?Sized>(request: WDFREQUEST, data: &T) -> NTSTATUS {
    // SAFETY: data 有效；按字节视图读出，不修改
    let bytes = unsafe {
        core::slice::from_raw_parts(
            (data as *const T).cast::<u8>(),
            core::mem::size_of_val(data),
        )
    };
    let mut memory: WDFMEMORY = core::ptr::null_mut();
    // SAFETY: 取输出内存
    let status = unsafe {
        call_unsafe_wdf_function_binding!(WdfRequestRetrieveOutputMemory, request, &mut memory)
    };
    if status < 0 {
        return status;
    }
    let mut out_len: usize = 0;
    // SAFETY: memory 有效
    let buf: PVOID =
        unsafe { call_unsafe_wdf_function_binding!(WdfMemoryGetBuffer, memory, &mut out_len) };
    if out_len < bytes.len() {
        return STATUS_INVALID_BUFFER_SIZE;
    }
    // SAFETY: buf 指向输出缓冲区且长度 >= bytes.len()
    unsafe {
        core::ptr::copy_nonoverlapping(bytes.as_ptr(), buf.cast::<u8>(), bytes.len());
    }
    // SAFETY: 记录已写字节数
    unsafe {
        call_unsafe_wdf_function_binding!(WdfRequestSetInformation, request, bytes.len() as u64);
    }
    STATUS_SUCCESS
}

// ---------------- 驱动入口 ----------------

/// Windows 驱动入口点
///
/// # Safety
/// 由加载器调用，参数为内核传入的有效指针。
#[unsafe(export_name = "DriverEntry")]
pub unsafe extern "system" fn driver_entry(
    driver: *mut DRIVER_OBJECT,
    registry_path: PCUNICODE_STRING,
) -> NTSTATUS {
    let mut config = WDF_DRIVER_CONFIG {
        Size: size_of::<WDF_DRIVER_CONFIG>() as ULONG,
        EvtDriverDeviceAdd: Some(evt_device_add),
        ..WDF_DRIVER_CONFIG::default()
    };
    // SAFETY: driver/registry_path 由 DriverEntry 提供且有效；config 有效；handle 输出可空
    unsafe {
        call_unsafe_wdf_function_binding!(
            WdfDriverCreate,
            driver as PDRIVER_OBJECT,
            registry_path,
            WDF_NO_OBJECT_ATTRIBUTES,
            &mut config,
            WDF_NO_HANDLE.cast::<WDFDRIVER>(),
        )
    }
}

/// panic 处理器：内核不允许 unwind，卡死等待调试器
#[cfg(not(test))]
#[panic_handler]
fn panic(_info: &core::panic::PanicInfo) -> ! {
    loop {}
}
