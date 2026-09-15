//! WASAPI 注入 / 采集（Windows 宿主）。
//!
//! 语义与驱动侧链路一致：
//! - `inject`   = 宿主向「vdev 扬声器」（render 端点）推流 → 驱动环形缓冲 →
//! - `capture`  = 从「vdev 麦克风」（capture 端点）拉流（即虚拟麦克风听得到扬声器所播内容）
//!
//! 只走共享模式 + 端点混音格式（引擎负责与设备格式之间的转换），与
//! `wasapi-loop` 验收脚本用的是同一条路径。

use std::time::Duration;

use anyhow::{Context as _, Result, bail};
use windows::Win32::Media::Audio::{
    AUDCLNT_BUFFERFLAGS_SILENT, AUDCLNT_SHAREMODE_SHARED, DEVICE_STATE_ACTIVE, EDataFlow,
    IAudioCaptureClient, IAudioClient, IAudioRenderClient, IMMDevice, IMMDeviceEnumerator,
    WAVEFORMATEX, eCapture, eRender,
};
use windows::Win32::System::Com::{
    CLSCTX_ALL, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx, STGM_READ,
};
use windows::Win32::UI::Shell::PropertiesSystem::{IPropertyStore, PROPERTYKEY};
use windows::core::{GUID, PROPVARIANT};

use crate::wav::{Wav, to_dbfs};

/// CLSID_MMDeviceEnumerator（mmdeviceapi.h）= {BCDE0395-E52F-467C-8E3D-C4579291692E}
const CLSID_MMDEVICEENUMERATOR: GUID = GUID::from_u128(0xbcde_0395_e52f_467c_8e3d_c457_9291_692e);
/// PKEY_Device_FriendlyName = {a45c254e-df1c-4efd-8020-67d146a850e0}, 14
const PKEY_DEVICE_FRIENDLY_NAME: PROPERTYKEY = PROPERTYKEY {
    fmtid: GUID::from_u128(0xa45c_254e_df1c_4efd_8020_67d1_46a8_50e0),
    pid: 14,
};

/// 端点采样格式（本 CLI 支持的两类）
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum SampleFmt {
    /// 32bit IEEE float（共享模式混音格式）
    F32,
    /// 16bit PCM（本驱动设备格式）
    I16,
}

struct Endpoint {
    name: String,
    id: String,
    device: IMMDevice,
}

impl Endpoint {
    /// 打开共享模式客户端 + 混音格式
    fn open(&self) -> Result<(IAudioClient, MixFormatPtr, SampleFmt, u16, u32)> {
        // SAFETY: 调用方持有有效 IMMDevice；返回的接口指针由 windows crate 管理
        unsafe {
            let client: IAudioClient = self
                .device
                .Activate(CLSCTX_ALL, None)
                .with_context(|| format!("Activate(IAudioClient) 失败：{}", self.name))?;
            let fmt = MixFormatPtr(client.GetMixFormat().context("GetMixFormat 失败")?);
            let kind = detect_format(fmt.0)?;
            // 只取前 18 字节里需要的两个字段（WAVEFORMATEX 本身是 packed，按值拷贝安全）
            let wf = *fmt.0;
            client
                .Initialize(
                    AUDCLNT_SHAREMODE_SHARED,
                    0,
                    10_000_000, // 1s 缓冲（hns）
                    0,
                    fmt.0,
                    None,
                )
                .context("IAudioClient::Initialize(shared) 失败（端点是否可用？）")?;
            Ok((client, fmt, kind, wf.nChannels, wf.nSamplesPerSec))
        }
    }
}

/// `GetMixFormat` 的 CoTaskMem 分配守卫（审查 L4 修复）：原实现只在 inject/capture
/// 的成功路径 `CoTaskMemFree`，中途任何 `?` 早退都泄漏这份分配；RAII 保证所有出口释放。
struct MixFormatPtr(*mut WAVEFORMATEX);

impl Drop for MixFormatPtr {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: 指针来自 GetMixFormat（CoTaskMemAlloc），本守卫是唯一释放点
            unsafe {
                windows::Win32::System::Com::CoTaskMemFree(Some(self.0.cast_const().cast()));
            }
        }
    }
}

/// 从 WAVEFORMATEX 判定可处理的采样格式
fn detect_format(p: *const WAVEFORMATEX) -> Result<SampleFmt> {
    const WAVE_FORMAT_PCM: u16 = 1;
    const WAVE_FORMAT_IEEE_FLOAT: u16 = 3;
    const WAVE_FORMAT_EXTENSIBLE: u16 = 0xFFFE;
    // KSDATAFORMAT_SUBTYPE_IEEE_FLOAT 的前 4 字节为 03 00 00 00
    const IEEE_FLOAT_HEAD: [u8; 4] = [0x03, 0x00, 0x00, 0x00];
    // windows crate 的 WAVEFORMATEX/WAVEFORMATEXTENSIBLE 都是 1 字节对齐（packed）：
    // 既不能取字段引用（E0793），也不能把 18 字节的 WAVEFORMATEX 拷到栈上再去读
    // 偏移 24 的 SubFormat（越界 = 栈垃圾，实测会把 float32 混音格式误判成"位深 32 不支持"）。
    // 统一按原始缓冲的字节偏移读取。
    let mut head = [0u8; 18];
    // SAFETY: 调用方保证 p 指向完整 WAVEFORMATEX（至少 18 字节）
    unsafe { core::ptr::copy_nonoverlapping(p.cast::<u8>(), head.as_mut_ptr(), head.len()) };
    let tag = u16::from_le_bytes([head[0], head[1]]);
    let bits = u16::from_le_bytes([head[14], head[15]]);
    let cb_size = u16::from_le_bytes([head[16], head[17]]);
    match tag {
        WAVE_FORMAT_IEEE_FLOAT => Ok(SampleFmt::F32),
        WAVE_FORMAT_PCM if bits == 16 => Ok(SampleFmt::I16),
        WAVE_FORMAT_EXTENSIBLE => {
            if cb_size < 22 {
                bail!("WAVEFORMATEXTENSIBLE.cbSize={cb_size} 太小");
            }
            // SAFETY: cbSize >= 22 已校验 → 缓冲至少 40 字节；SubFormat 位于偏移 24..40
            let mut sub = [0u8; 16];
            unsafe {
                core::ptr::copy_nonoverlapping(p.cast::<u8>().add(24), sub.as_mut_ptr(), sub.len());
            }
            if sub[0..4] == IEEE_FLOAT_HEAD {
                Ok(SampleFmt::F32)
            } else if bits == 16 {
                Ok(SampleFmt::I16)
            } else {
                bail!("不支持的 WAVEFORMATEXTENSIBLE 位深 {bits}（支持 16bit PCM / 32bit float）")
            }
        }
        tag => bail!("不支持的 wFormatTag={tag}（支持 PCM 16bit / IEEE float 32bit）"),
    }
}

/// 初始化 COM 并枚举活动端点，按名字子串（不区分大小写）挑选 vdev 端点
fn find_endpoint(flow: EDataFlow, name_filter: &str) -> Result<Endpoint> {
    // SAFETY: CoInitializeEx 可重复调用；RPC_E_CHANGED_MODE 表示已按别的套间初始化，可继续
    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        let enumerator: IMMDeviceEnumerator =
            CoCreateInstance(&CLSID_MMDEVICEENUMERATOR, None, CLSCTX_ALL)
                .context("创建 MMDeviceEnumerator 失败")?;
        let collection = enumerator
            .EnumAudioEndpoints(flow, DEVICE_STATE_ACTIVE)
            .context("EnumAudioEndpoints 失败")?;
        let count = collection.GetCount().context("GetCount 失败")?;
        let mut candidates: Vec<String> = Vec::new();
        let mut all: Vec<String> = Vec::new();
        for i in 0..count {
            let dev = collection.Item(i).context("Item(i) 失败")?;
            let name = friendly_name(&dev).unwrap_or_else(|_| "(未知)".to_string());
            let id = device_id(&dev).unwrap_or_default();
            all.push(name.clone());
            if name.to_lowercase().contains(&name_filter.to_lowercase()) {
                candidates.push(format!("{name} [{id}]"));
                return Ok(Endpoint {
                    name,
                    id,
                    device: dev,
                });
            }
        }
        if candidates.is_empty() {
            bail!(
                "未找到名字含「{name_filter}」的活动端点。当前可用：{}",
                all.join(" / ")
            );
        }
        unreachable!()
    }
}

unsafe fn friendly_name(dev: &IMMDevice) -> Result<String> {
    // SAFETY: dev 有效；PROPVARIANT 由 windows crate 的 Drop 释放
    unsafe {
        let store: IPropertyStore = dev
            .OpenPropertyStore(STGM_READ)
            .context("OpenPropertyStore 失败")?;
        let pv: PROPVARIANT = store
            .GetValue(&PKEY_DEVICE_FRIENDLY_NAME)
            .context("GetValue(PKEY_Device_FriendlyName) 失败")?;
        Ok(pv.to_string())
    }
}

unsafe fn device_id(dev: &IMMDevice) -> Result<String> {
    // SAFETY: GetId 返回 CoTaskMem 分配的 PWSTR，转 String 后释放
    unsafe {
        let p = dev.GetId().context("GetId 失败")?;
        let s = p.to_string().context("端点 ID 解码失败")?;
        windows::Win32::System::Com::CoTaskMemFree(Some(p.0.cast()));
        Ok(s)
    }
}

/// 把 f32 样本按端点采样格式写入缓冲
unsafe fn store_sample(ptr: *mut u8, idx: usize, kind: SampleFmt, v: f32) {
    // SAFETY: 调用方保证 ptr 至少可容纳 idx 个样本
    unsafe {
        match kind {
            SampleFmt::F32 => core::ptr::write_unaligned(ptr.add(idx * 4).cast::<f32>(), v),
            SampleFmt::I16 => core::ptr::write_unaligned(
                ptr.add(idx * 2).cast::<i16>(),
                (v.clamp(-1.0, 1.0) * 32767.0).round() as i16,
            ),
        }
    }
}

/// 从缓冲读出 f32 样本
unsafe fn load_sample(ptr: *const u8, idx: usize, kind: SampleFmt) -> f32 {
    // SAFETY: 调用方保证 ptr 至少可容纳 idx 个样本
    unsafe {
        match kind {
            SampleFmt::F32 => core::ptr::read_unaligned(ptr.add(idx * 4).cast::<f32>()),
            SampleFmt::I16 => {
                f32::from(core::ptr::read_unaligned(ptr.add(idx * 2).cast::<i16>())) / 32768.0
            }
        }
    }
}

/// 注入报告
#[derive(serde::Serialize)]
pub struct InjectReport {
    pub endpoint: String,
    pub endpoint_id: String,
    pub sample_rate: u32,
    pub channels: u16,
    pub frames: u64,
    pub seconds: f64,
}

/// 采集报告
#[derive(serde::Serialize)]
pub struct CaptureReport {
    pub endpoint: String,
    pub endpoint_id: String,
    pub sample_rate: u32,
    pub channels: u16,
    pub frames: u64,
    pub seconds: f64,
    pub rms_dbfs: f64,
    pub peak_dbfs: f64,
    pub wav: Option<String>,
}

/// 向 render 端点注入音频：`wav` 给定时播放文件，否则播 `tone_hz` 正弦
pub fn inject(
    name_filter: &str,
    wav: Option<&Wav>,
    tone_hz: f32,
    amplitude: f32,
    duration: f64,
) -> Result<InjectReport> {
    let ep = find_endpoint(eRender, name_filter)?;
    let (client, _fmt, kind, channels, rate) = ep.open()?;
    let buf_frames = unsafe { client.GetBufferSize() }.context("GetBufferSize 失败")?;
    let render: IAudioRenderClient = unsafe { client.GetService() }.context("GetService 失败")?;

    let want: u64 = (f64::from(rate) * duration.max(0.0)).round() as u64;
    let ch = usize::from(channels);
    unsafe {
        client.Start().context("Start 失败")?;
        let mut written: u64 = 0;
        while written < want {
            let padding = client
                .GetCurrentPadding()
                .context("GetCurrentPadding 失败")?;
            let avail = u64::from(buf_frames.saturating_sub(padding));
            if avail == 0 {
                std::thread::sleep(Duration::from_millis(2));
                continue;
            }
            let n = avail.min(want - written) as u32;
            let ptr = render
                .GetBuffer(n)
                .context("IAudioRenderClient::GetBuffer 失败")?;
            for f in 0..n as usize {
                let frame = written + f as u64;
                for c in 0..ch {
                    let v = match wav {
                        Some(w) => {
                            // 采样率不同就直接按帧号取样（调用方需自行保证一致；
                            // 不一致时等价于轻微变速，不做重采样）
                            let src_frame = if w.sample_rate == rate {
                                frame as usize % w.frames().max(1)
                            } else {
                                ((frame as f64) * f64::from(w.sample_rate) / f64::from(rate))
                                    as usize
                                    % w.frames().max(1)
                            };
                            w.sample(src_frame, c)
                        }
                        None => {
                            let t = frame as f64 / f64::from(rate);
                            (f64::from(amplitude)
                                * (2.0 * std::f64::consts::PI * f64::from(tone_hz) * t).sin())
                                as f32
                        }
                    };
                    store_sample(ptr, f * ch + c, kind, v);
                }
            }
            render
                .ReleaseBuffer(n, 0)
                .context("IAudioRenderClient::ReleaseBuffer 失败")?;
            written += u64::from(n);
        }
        client.Stop().context("Stop 失败")?;
        // fmt（MixFormatPtr）随作用域结束自动 CoTaskMemFree，含所有早退路径
        Ok(InjectReport {
            endpoint: ep.name,
            endpoint_id: ep.id,
            sample_rate: rate,
            channels,
            frames: written,
            seconds: written as f64 / f64::from(rate),
        })
    }
}

/// 从 capture 端点采集音频；`skip_seconds` 内的包只排空不计入统计（避开环回积压）
pub fn capture(
    name_filter: &str,
    duration: f64,
    skip_seconds: f64,
    wav_path: Option<&std::path::Path>,
) -> Result<CaptureReport> {
    let ep = find_endpoint(eCapture, name_filter)?;
    let (client, _fmt, kind, channels, rate) = ep.open()?;
    let cap: IAudioCaptureClient = unsafe { client.GetService() }.context("GetService 失败")?;
    let ch = usize::from(channels);
    let mut kept: Vec<f32> = Vec::new();
    let mut frames: u64 = 0;
    let start = std::time::Instant::now();
    unsafe {
        client.Start().context("Start 失败")?;
        loop {
            let elapsed = start.elapsed().as_secs_f64();
            if elapsed >= duration.max(0.0) {
                break;
            }
            let packet = cap.GetNextPacketSize().context("GetNextPacketSize 失败")?;
            if packet == 0 {
                std::thread::sleep(Duration::from_millis(5));
                continue;
            }
            let mut ptr: *mut u8 = core::ptr::null_mut();
            let mut got: u32 = 0;
            let mut flags: u32 = 0;
            cap.GetBuffer(&mut ptr, &mut got, &mut flags, None, None)
                .context("IAudioCaptureClient::GetBuffer 失败")?;
            let silent = AUDCLNT_BUFFERFLAGS_SILENT.0 as u32;
            let keep = elapsed >= skip_seconds && (flags & silent) == 0;
            if keep && !ptr.is_null() {
                for f in 0..got as usize {
                    for c in 0..ch {
                        kept.push(load_sample(ptr, f * ch + c, kind));
                    }
                }
                frames += u64::from(got);
            }
            cap.ReleaseBuffer(got).context("ReleaseBuffer 失败")?;
        }
        client.Stop().context("Stop 失败")?;
        // fmt（MixFormatPtr）随作用域结束自动 CoTaskMemFree，含所有早退路径
    }
    let (rms, peak) = crate::wav::rms_peak(&kept);
    let mut saved = None;
    if let Some(path) = wav_path {
        std::fs::write(path, crate::wav::write_wav16(rate, channels, &kept))
            .with_context(|| format!("写 WAV 失败：{}", path.display()))?;
        saved = Some(path.display().to_string());
    }
    Ok(CaptureReport {
        endpoint: ep.name,
        endpoint_id: ep.id,
        sample_rate: rate,
        channels,
        frames,
        seconds: frames as f64 / f64::from(rate),
        rms_dbfs: to_dbfs(rms),
        peak_dbfs: to_dbfs(peak),
        wav: saved,
    })
}
