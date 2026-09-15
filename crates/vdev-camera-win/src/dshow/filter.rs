//! `VirtualCameraFilter`：DirectShow 捕获源过滤器主体。
//!
//! 实现 `IBaseFilter`（含继承链 `IMediaFilter` / `IPersist`）与
//! `IAMFilterMiscFlags`（标识为源过滤器）。推流线程见 [`super::streaming`]。

use std::mem::ManuallyDrop;
use std::sync::atomic::AtomicU64;
use std::sync::{Arc, Mutex};

use windows::Win32::Foundation::E_POINTER;
use windows::Win32::Media::DirectShow::{
    IAMFilterMiscFlags, IAMFilterMiscFlags_Impl, IBaseFilter, IBaseFilter_Impl, IEnumPins,
    IFilterGraph, IMediaFilter, IMediaFilter_Impl, IPin, State_Paused, State_Running,
    State_Stopped, AM_FILTER_MISC_FLAGS_IS_SOURCE, FILTER_INFO, FILTER_STATE, VFW_E_NOT_FOUND,
    VFW_E_NO_CLOCK,
};
use windows::Win32::Media::IReferenceClock;
use windows::Win32::System::Com::{IPersist, IPersist_Impl};
use windows_core::{implement, Error, Interface, Ref, GUID, PCWSTR, PWSTR};

use super::pin::{OutputPin, PinInner};
use super::streaming::{self, StreamThread};
use crate::com::shm::SharedFrameChannel;

/// 过滤器 CLSID（COM 标识；注册表路径用 [`crate::com::registry::guid_string`]）。
#[allow(non_upper_case_globals)] // 沿用 Windows `CLSID_*` 命名惯例
pub const CLSID_VirtualCameraFilter: GUID = GUID::from_u128(0xE4C01F0D_A9FC_4352_8590_F0E5AD2BFFCE);
/// 过滤器显示名（摄像头列表里显示的名字）。
pub const FILTER_NAME: &str = "vdev-camera";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FilterStateInternal {
    Stopped,
    Paused,
    Running,
}

/// 过滤器共享状态（被 COM 接口方法与推流线程并发访问，内部加锁）。
///
/// SAFETY 说明：DirectShow MTA 下 COM 接口（IBaseFilter/IFilterGraph/IReferenceClock）
/// 允许跨线程访问；内部所有共享状态均受 Mutex/原子保护。
///
/// 所有权模型（修 B2 COM 自引用环，无法在 macOS 宿主单测——需 Windows COM
/// 运行时；生命周期推演如下，随 Windows 侧 selftest 验证）：
/// - 过滤器 COM 对象（F）持有 `Arc<FilterInner>`（FI）；
/// - FI 持有输出 pin COM 对象的**初始强引用**（`pin_com`）⇒ pin 的存活期 ≤ F；
/// - FI 只以**弱引用**回看自身 COM 对象（`self_base: Weak<IBaseFilter>`），
///   pin 只以弱引用回看自身（`PinInner::self_pin: Weak<IPin>`）与 owner
///   （`PinInner::filter: Weak<FilterInner>`）；
/// - 强引用链无环：F → FI → pin(强) → PI；PI → FI/pin 均为弱。外部引用
///   （图/调用方）全部释放后 F 引用计数归零 → F 析构 → 释放 FI → FI 释放
///   pin 强引用 → pin 析构 → 释放 PI，全部可回收（原实现 filter/pin 各自
///   缓存自身接口的强引用，引用计数永不归零，Stop 后不析构）；
/// - 弱引用 upgrade 失败即对象正在析构 → 返回错误/空指针，不悬垂。
pub struct FilterInner {
    name: Mutex<Vec<u16>>,
    graph: Mutex<Option<IFilterGraph>>,
    state: Mutex<FilterStateInternal>,
    clock: Mutex<Option<IReferenceClock>>,
    /// 输出 pin。
    pub pin: Arc<PinInner>,
    /// 输出 pin COM 对象的初始强引用（过滤器持有；pin 自身与 FilterInner 均
    /// 只存弱引用，见结构体文档所有权模型）。
    pin_com: Mutex<Option<IPin>>,
    /// 共享帧通道（消费者端）。
    pub channel: Arc<SharedFrameChannel>,
    stream: Mutex<Option<StreamThread>>,
    /// 自身 `IBaseFilter` 的**弱引用**（供 `QueryFilterInfo` / pin 的
    /// `QueryPinInfo` 返回；析构中 upgrade 失败 → pFilter=null，不悬垂）。
    self_base: Mutex<Option<windows_core::Weak<IBaseFilter>>>,
    /// 已交付帧计数（自测/调试用）。
    pub frames_delivered: AtomicU64,
}

impl FilterInner {
    /// 返回缓存的自身 IBaseFilter 强引用（弱引用 upgrade）。
    ///
    /// 过滤器 COM 对象正在析构（外部强引用已归零）时返回 `None`——调用方
    /// （pin 的 `QueryPinInfo`）相应写 null pFilter，DirectShow 允许调用方判空。
    pub(crate) fn self_base_owned(&self) -> Option<IBaseFilter> {
        self.self_base
            .lock()
            .unwrap()
            .as_ref()
            .and_then(windows_core::Weak::upgrade)
    }
}

// SAFETY: 见 FilterInner 文档——MTA 语义 + 内部加锁，跨线程访问安全。
unsafe impl Send for FilterInner {}
unsafe impl Sync for FilterInner {}

impl FilterInner {
    fn new(channel: Arc<SharedFrameChannel>) -> Arc<Self> {
        let inner = Arc::new(Self {
            name: Mutex::new(FILTER_NAME.encode_utf16().collect()),
            graph: Mutex::new(None),
            state: Mutex::new(FilterStateInternal::Stopped),
            clock: Mutex::new(None),
            pin: Arc::new(PinInner::unattached()),
            pin_com: Mutex::new(None),
            channel,
            stream: Mutex::new(None),
            self_base: Mutex::new(None),
            frames_delivered: AtomicU64::new(0),
        });
        // 创建输出 pin COM 对象并缓存其 IPin（EnumPins/FindPin/协商返回同一对象）。
        let pin_obj = OutputPin {
            inner: inner.pin.clone(),
        };
        let ipin: IPin = pin_obj.into();
        inner.pin.set_self_pin(&ipin);
        // pin 的初始强引用由过滤器持有（pin 自身只存弱引用，见 FilterInner
        // 所有权模型）；此后 pin 存活期 ≤ 过滤器。
        *inner.pin_com.lock().unwrap() = Some(ipin);
        // 把 pin 挂到过滤器（Weak，避免循环强引用）。
        *inner.pin.filter.lock().unwrap() = Some(Arc::downgrade(&inner));
        inner
    }
}

/// 创建过滤器 COM 对象，返回其 `IBaseFilter` 接口。
pub fn create_filter() -> windows_core::Result<IBaseFilter> {
    Ok(create_filter_with_inner()?.0)
}

/// 创建过滤器 COM 对象，同时返回内部状态（自测读帧计数用）。
pub fn create_filter_with_inner() -> windows_core::Result<(IBaseFilter, Arc<FilterInner>)> {
    let channel = Arc::new(SharedFrameChannel::open_or_create(false).map_err(|e| {
        log::error!("open frame channel failed: {e}");
        Error::from_hresult(windows::Win32::Foundation::E_FAIL)
    })?);
    let inner = FilterInner::new(channel);
    let obj = VirtualCameraFilter {
        inner: inner.clone(),
    };
    let base: IBaseFilter = obj.into();
    // 修 B2：自缓存 IBaseFilter 改存**弱引用**。原实现 clone 强引用（windows_core
    // 显式 AddRef）→ 过滤器引用计数含 1 个自引用，外部全部释放后仍停在 1，
    // 永不析构（泄漏）。弱引用不参与强计数；析构中 upgrade 失败 → 返回 None。
    // `#[implement]` 生成的对象必然支持 IWeakReferenceSource（WeakRefCount
    // 无条件处理该 IID），downgrade 实际不会失败；万一失败则 QueryPinInfo
    // 退化为 null pFilter（安全降级，不悬垂）。
    *inner.self_base.lock().unwrap() = base.downgrade().ok();
    Ok((base, inner))
}

/// DirectShow 过滤器类（单一对象，QI 分发给列出的接口）。
#[implement(IBaseFilter, IMediaFilter, IPersist, IAMFilterMiscFlags)]
pub struct VirtualCameraFilter {
    pub inner: Arc<FilterInner>,
}

/// 把 UTF-16 名字写入 `achName` 定长数组（截断 + NUL 结尾）。
pub(crate) fn write_ach_name(dst: &mut [u16], name: &[u16]) {
    let n = name.len().min(dst.len() - 1);
    dst[..n].copy_from_slice(&name[..n]);
    dst[n] = 0;
    for c in dst.iter_mut().skip(n + 1) {
        *c = 0;
    }
}

impl IBaseFilter_Impl for VirtualCameraFilter_Impl {
    fn EnumPins(&self) -> windows_core::Result<IEnumPins> {
        let pin = self.inner.pin.self_pin()?;
        Ok(super::enum_pins::PinEnum::new(pin).into())
    }

    fn FindPin(&self, id: &PCWSTR) -> windows_core::Result<IPin> {
        let name = self.inner.pin.name.clone();
        // SAFETY: id 由调用方保证为合法 PCWSTR。
        let id_str = unsafe { id.to_string() }.unwrap_or_default();
        if id_str == String::from_utf16_lossy(&name) {
            self.inner.pin.self_pin()
        } else {
            Err(Error::from_hresult(VFW_E_NOT_FOUND))
        }
    }

    fn QueryFilterInfo(&self, pinfo: *mut FILTER_INFO) -> windows_core::Result<()> {
        if pinfo.is_null() {
            return Err(Error::from_hresult(E_POINTER));
        }
        // SAFETY: pinfo 由调用方分配。
        let info = unsafe { &mut *pinfo };
        let name = self.inner.name.lock().unwrap().clone();
        write_ach_name(&mut info.achName, &name);
        let graph = self.inner.graph.lock().unwrap().clone();
        // pGraph 所有权交给调用方（调用方负责 Release）。
        info.pGraph = ManuallyDrop::new(graph);
        Ok(())
    }

    fn JoinFilterGraph(
        &self,
        pgraph: Ref<IFilterGraph>,
        pname: &PCWSTR,
    ) -> windows_core::Result<()> {
        let graph = pgraph.cloned();
        *self.inner.graph.lock().unwrap() = graph;
        if !pname.is_null() {
            // SAFETY: pname 由调用方保证为合法 PCWSTR。
            if let Ok(s) = unsafe { pname.to_string() } {
                *self.inner.name.lock().unwrap() = s.encode_utf16().collect();
            }
        }
        if pgraph.is_null() {
            // 从图中移除：停止推流线程。
            self.stop_streaming();
        }
        Ok(())
    }

    fn QueryVendorInfo(&self) -> windows_core::Result<PWSTR> {
        // SAFETY: 返回 CoTaskMem 分配的字符串，调用方用 CoTaskMemFree 释放。
        Ok(unsafe { crate::dshow::util::alloc_pwstr("vdev (Rust)") })
    }
}

impl IMediaFilter_Impl for VirtualCameraFilter_Impl {
    fn Stop(&self) -> windows_core::Result<()> {
        log::debug!("filter Stop");
        self.stop_streaming();
        *self.inner.state.lock().unwrap() = FilterStateInternal::Stopped;
        Ok(())
    }

    fn Pause(&self) -> windows_core::Result<()> {
        log::debug!("filter Pause");
        // 推源过滤器：Pause 时启动推流线程，送出预滚帧让下游渲染器完成 Pause。
        self.start_streaming()?;
        *self.inner.state.lock().unwrap() = FilterStateInternal::Paused;
        Ok(())
    }

    fn Run(&self, tstart: i64) -> windows_core::Result<()> {
        log::debug!("filter Run(tstart={tstart})");
        // 样本时间戳用本地流时间（从 0 递增），tstart（参考时钟起点）无需
        // 下发给推流线程（审查 L7：原实现经原子量传递却从未被读取）。
        let guard = self.inner.stream.lock().unwrap();
        if guard.is_none() {
            drop(guard);
            self.start_streaming()?;
        }
        *self.inner.state.lock().unwrap() = FilterStateInternal::Running;
        Ok(())
    }

    fn GetState(&self, _dwmillisecstimeout: u32) -> windows_core::Result<FILTER_STATE> {
        let st = *self.inner.state.lock().unwrap();
        log::debug!("filter GetState -> {:?}", st);
        Ok(match st {
            FilterStateInternal::Stopped => State_Stopped,
            FilterStateInternal::Paused => State_Paused,
            FilterStateInternal::Running => State_Running,
        })
    }

    fn SetSyncSource(&self, pclock: Ref<IReferenceClock>) -> windows_core::Result<()> {
        *self.inner.clock.lock().unwrap() = pclock.cloned();
        Ok(())
    }

    fn GetSyncSource(&self) -> windows_core::Result<IReferenceClock> {
        self.inner
            .clock
            .lock()
            .unwrap()
            .clone()
            .ok_or_else(|| Error::from_hresult(VFW_E_NO_CLOCK))
    }
}

impl IPersist_Impl for VirtualCameraFilter_Impl {
    fn GetClassID(&self) -> windows_core::Result<GUID> {
        Ok(CLSID_VirtualCameraFilter)
    }
}

impl IAMFilterMiscFlags_Impl for VirtualCameraFilter_Impl {
    fn GetMiscFlags(&self) -> u32 {
        AM_FILTER_MISC_FLAGS_IS_SOURCE.0 as u32
    }
}

impl VirtualCameraFilter {
    fn start_streaming(&self) -> windows_core::Result<()> {
        let mut guard = self.inner.stream.lock().unwrap();
        if guard.is_some() {
            return Ok(());
        }
        // 未连接时暂不启动（图会在连接后才 Run/Pause）。
        if !self.inner.pin.is_connected() {
            return Ok(());
        }
        // 提交分配器：内存分配器必须 Commit 后 GetBuffer 才能取到缓冲区
        // （DirectShow 协议，CBaseOutputPin::Active 做同样的事）。
        if let Some(conn) = self.inner.pin.connected() {
            // SAFETY: COM 方法调用。
            unsafe { conn.allocator.Commit() }?;
        }
        *guard = Some(streaming::start_stream(self.inner.clone()));
        log::debug!("stream thread started");
        Ok(())
    }

    fn stop_streaming(&self) {
        let mut guard = self.inner.stream.lock().unwrap();
        if let Some(mut t) = guard.take() {
            // 修 M1（顺序不变式，推演）：必须「先停线程（信号 + join），后
            // Decommit」。`t.stop()` 返回时推流线程已死，其最后一轮
            // GetBuffer / GetPointer / deliver_sample / Receive 均已结束——
            // 因此下一步 Decommit（与 start_streaming 的 Commit 配对，对应
            // CBaseOutputPin::Active/Inactive）时绝无线程再访问已 decommit 的
            // 分配器内存。反序（先 Decommit）会让线程在 decommit 后仍可能进入
            // 下一轮循环访问分配器状态，不取。
            // 残余风险（有意接受）：线程可能阻塞在无超时的 GetBuffer 等下游释还
            // 样本；DirectShow 全图 Stop 有顺序性（渲染器先停、样本释还），join
            // 因此有界。
            t.stop();
            // 停止推流后释放分配器缓冲区（与 Commit 配对）。
            if let Some(conn) = self.inner.pin.connected() {
                // SAFETY: COM 方法调用。
                let _ = unsafe { conn.allocator.Decommit() };
            }
        }
    }
}
