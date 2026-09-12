use std::{
    mem::size_of,
    ptr::NonNull,
    sync::{LazyLock, Mutex, OnceLock, PoisonError},
    thread,
    time::Duration,
};

use driver_ipc::{
    Dimen, DriverCommand, EventCommand, Mode, Monitor, RefreshRate, ReplyCommand, RequestCommand,
    ServerCommand,
};
use log::{error, warn};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt as _},
    net::windows::named_pipe::{NamedPipeServer, ServerOptions},
    sync::broadcast::{self, error::RecvError, Sender},
    task,
};
use wdf_umdf::IddCxMonitorDeparture;
use wdf_umdf_sys::{IDDCX_ADAPTER__, IDDCX_MONITOR__};
use windows::{
    core::w,
    Win32::Security::{
        Authorization::{ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1},
        PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES,
    },
};

use crate::{context::DeviceContext, validate};

pub static ADAPTER: OnceLock<AdapterObject> = OnceLock::new();
pub static MONITOR_MODES: LazyLock<Mutex<Vec<MonitorObject>>> =
    LazyLock::new(|| Mutex::new(Vec::new()));

#[derive(Debug)]
pub struct AdapterObject(pub NonNull<IDDCX_ADAPTER__>);
unsafe impl Sync for AdapterObject {}
unsafe impl Send for AdapterObject {}

#[derive(Debug)]
pub struct MonitorObject {
    pub object: Option<NonNull<IDDCX_MONITOR__>>,
    pub data: Monitor,
}
unsafe impl Sync for MonitorObject {}
unsafe impl Send for MonitorObject {}

const BUFFER_SIZE: u32 = 4096;
// EOT
const EOF: char = '\x04';

// message processor
async fn process_message(
    id: usize,
    server: &mut NamedPipeServer,
    tx: &Sender<(usize, Vec<Monitor>)>,
    buf: &[u8],
    iter: impl Iterator<Item = usize>,
) -> Result<(), ()> {
    // process each message in the buffer
    let mut start = 0;
    for eidx in iter {
        let sidx = start;
        start = eidx + 1;

        let Ok(msg) = std::str::from_utf8(&buf[sidx..eidx]) else {
            continue;
        };

        let Ok(command) = serde_json::from_str::<ServerCommand>(msg) else {
            continue;
        };

        match command {
            // driver commands
            ServerCommand::Driver(cmd) => match cmd {
                DriverCommand::Notify(monitors) => {
                    // minor(c)：notify() 返回 sanitize 后的列表 —— 广播给其他客户端的
                    // EventCommand::Changed 必须与驱动实际生效（校验后）的状态同源，
                    // 不能广播未校验的原始输入
                    let monitors = notify(monitors);
                    _ = tx.send((id, monitors));
                }

                DriverCommand::Remove(ids) => {
                    remove(&ids);

                    // M3：互斥量中毒后继续使用既有数据（into_inner），避免 unwrap panic 跨 FFI 边界
                    let lock = MONITOR_MODES.lock().unwrap_or_else(PoisonError::into_inner);
                    let monitors = lock.iter().map(|m| m.data.clone()).collect();
                    _ = tx.send((id, monitors));
                }

                DriverCommand::RemoveAll => {
                    remove_all();
                    _ = tx.send((id, Vec::new()));
                }

                _ => (),
            },

            // request commands
            ServerCommand::Request(RequestCommand::State) => {
                let mut data = {
                    // M3：互斥量中毒后继续使用既有数据（into_inner），避免 unwrap panic 跨 FFI 边界
                    let lock = MONITOR_MODES.lock().unwrap_or_else(PoisonError::into_inner);
                    let monitors = lock.iter().map(|m| m.data.clone()).collect();
                    let command = ReplyCommand::State(monitors);

                    let Ok(serialized) = serde_json::to_string(&command) else {
                        error!("Command::Request - failed to serialize reply");
                        break;
                    };

                    serialized
                };

                data.push(EOF);

                if server.write_all(data.as_bytes()).await.is_err() {
                    // a server error means we should completely stop trying
                    return Err(());
                }
            }

            // Everything else is an invalid command
            _ => (),
        }
    }

    Ok(())
}

#[allow(clippy::too_many_lines)]
pub fn startup() {
    thread::spawn(move || {
        // M1：管道安全 —— 不再使用 NULL DACL（任何本地进程都能连上管道注入
        // Notify/Remove 命令）。改用 SDDL 限定描述符：
        //   D:P            DACL 存在且受保护（SE_DACL_PROTECTED，不被继承 ACL 穿透）
        //   (A;;GA;;;BA)   允许 BUILTIN\Administrators 完全访问
        //   (A;;GA;;;SY)   允许 LOCAL SYSTEM 完全访问
        // 出处：Win32 "Security Descriptor String Format"（learn.microsoft.com
        // secauthz/security-descriptor-string-format）与 "ACE Strings"（GA=GENERIC_ALL、
        // BA=BUILTIN\Administrators、SY=LOCAL SYSTEM）。客户端（vdev-display cli）
        // 因此须以管理员身份运行。
        let sddl = w!("D:P(A;;GA;;;BA)(A;;GA;;;SY)");

        // 一次性转换出的自相对 SD 由 listener 线程持有、驱动进程生命周期内持续使用；
        // listener 线程永不退出，故不做 LocalFree（进程退出时由系统回收）。
        let mut psd = PSECURITY_DESCRIPTOR::default();

        let converted = unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                sddl,
                SDDL_REVISION_1,
                &mut psd,
                None,
            )
        };

        if let Err(e) = converted {
            // 无法生成安全描述符时不能退回无保护管道，直接放弃启动 listener
            error!("Failed to convert SDDL to security descriptor: {e:?}");
            return;
        }

        let mut sa = SECURITY_ATTRIBUTES {
            #[allow(clippy::cast_possible_truncation)]
            nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: psd.0,
            bInheritHandle: false.into(),
        };

        // async time!
        let pipe_server = async {
            let (tx, _rx) = broadcast::channel(1);

            let mut id = 0usize;
            // M1：FILE_FLAG_FIRST_PIPE_INSTANCE 只能加在第一个实例上 ——
            // MS 文档（CreateNamedPipe dwOpenMode）明确：“creation of the first
            // instance succeeds, but creation of the next instance fails with
            // ERROR_ACCESS_DENIED”。它用于防止管道抢占（squatting）：恶意进程
            // 先建同名管道时我们的首个实例创建失败，而不是静默让客户端连到
            // 攻击者的管道上。
            let mut first_instance = true;

            loop {
                let mut options = ServerOptions::new()
                    .access_inbound(true)
                    .access_outbound(true)
                    .reject_remote_clients(true)
                    .in_buffer_size(BUFFER_SIZE)
                    .out_buffer_size(BUFFER_SIZE)
                    // default is unlimited instances
                    .first_pipe_instance(first_instance);

                // M1：创建失败不再 unwrap panic（原实现在抢占场景必然 panic 炸掉
                // listener 线程），改为记日志 + 退避重试
                let mut server = match unsafe {
                    options.create_with_security_attributes_raw(
                        r"\\.\pipe\vdev-display",
                        std::ptr::from_mut::<SECURITY_ATTRIBUTES>(&mut sa).cast(),
                    )
                } {
                    Ok(server) => {
                        first_instance = false;
                        server
                    }
                    Err(e) => {
                        error!("Failed to create named pipe instance: {e:?}");
                        thread::sleep(Duration::from_secs(1));
                        continue;
                    }
                };

                if server.connect().await.is_err() {
                    continue;
                }

                id += 1;

                let mut msg_buf: Vec<u8> = Vec::with_capacity(BUFFER_SIZE as usize);
                let mut buf = vec![0; BUFFER_SIZE as usize];
                let tx = tx.clone();
                let mut rx = tx.subscribe();

                task::spawn(async move {
                    loop {
                        tokio::select! {
                            val = server.read(&mut buf) =>  {
                                match val {
                                    // 0 = no more data to read
                                    // or break on err
                                    Ok(0) | Err(_) => break,

                                    Ok(size) => msg_buf.extend(&buf[..size]),
                                }

                                // M2(a)：单消息字节上限。EOF 边界之前积累的字节
                                // 超过上限说明对端在无边界地灌数据（或消息远超
                                // 合法规模），丢弃已积累 buffer，防止无界内存增长。
                                // 512 KiB 足以容纳 M2(b) 上限下的最坏合法 Notify
                                //（见 validate::MAX_MSG_BYTES 注释的推算）。
                                if validate::msg_over_limit(msg_buf.len()) {
                                    warn!(
                                        "IPC: buffered input exceeded {} bytes; dropping it",
                                        validate::MAX_MSG_BYTES
                                    );
                                    msg_buf.clear();
                                }

                                // get all eof boundary positions
                                let mut eof_iter = msg_buf.iter().enumerate().filter_map(|(i, &byte)| {
                                    if byte == EOF as u8 {
                                        Some(i)
                                    } else {
                                        None
                                    }
                                });

                                if process_message(id, &mut server, &tx, &msg_buf, eof_iter.clone()).await.is_err() {
                                    break;
                                }

                                // remove processed messages from buffer
                                // we can exploit the fact that these are sequential
                                // so just get the last index and chop off everything before that
                                if let Some(last) = eof_iter.next_back() {
                                    // remove everything up to and including the last EOF
                                    msg_buf.drain(..=last);
                                }
                            },

                            val = rx.recv() => {
                                let command = match val {
                                    // ignore if this value was sent for the current client (current client doesn't need notification)
                                    Ok((client_id, _)) if client_id == id => continue,

                                    Ok((_, data)) => EventCommand::Changed(data),

                                    Err(RecvError::Lagged(_)) => continue,

                                    // closed
                                    Err(_) => break
                                };

                                let Ok(mut serialized) = serde_json::to_string(&command) else {
                                    error!("Command::Request - failed to serialize reply");
                                    break;
                                };

                                serialized.push(EOF);

                                if server.write_all(serialized.as_bytes()).await.is_err() {
                                    break;
                                }
                            }
                        }
                    }
                });
            }
        };

        tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .expect("Failed building the Runtime")
            .block_on(pipe_server);
    });
}

/// used to check the validity of a Vec<Monitor>
/// the validity invariants are:
/// 1. unique monitor ids
/// 2. unique monitor modes (width+height must be unique per array element)
/// 3. unique refresh rates per monitor mode
fn has_duplicates(monitors: &[Monitor]) -> bool {
    let mut monitor_iter = monitors.iter();
    while let Some(monitor) = monitor_iter.next() {
        let duplicate_id = monitor_iter.clone().any(|b| monitor.id == b.id);
        if duplicate_id {
            warn!("Found duplicate monitor id {}", monitor.id);
            return true;
        }

        let mut mode_iter = monitor.modes.iter();
        while let Some(mode) = mode_iter.next() {
            let duplicate_mode = mode_iter
                .clone()
                .any(|m| mode.height == m.height && mode.width == m.width);
            if duplicate_mode {
                warn!(
                    "Found duplicate mode {}x{} on monitor {}",
                    mode.width, mode.height, monitor.id
                );
                return true;
            }

            let mut refresh_iter = mode.refresh_rates.iter().copied();
            while let Some(rr) = refresh_iter.next() {
                let duplicate_rr = refresh_iter.clone().any(|r| rr == r);
                if duplicate_rr {
                    warn!(
                        "Found duplicate refresh rate {rr} on mode {}x{} for monitor {}",
                        mode.width, mode.height, monitor.id
                    );
                    return true;
                }
            }
        }
    }

    false
}

/// M2(b)：对客户端发来的显示器列表做输入校验（上限/范围），越界部分拒收并记日志。
/// 决策逻辑在纯函数模块 [`crate::validate`]（可宿主单测），这里只做 Vec 搬运。
fn sanitize_monitors(mut monitors: Vec<Monitor>) -> Vec<Monitor> {
    let capped_len = validate::enforce_monitor_cap(monitors.len());
    if monitors.len() > capped_len {
        warn!(
            "IPC: {} monitors exceed cap {}; dropping the rest",
            monitors.len(),
            validate::MAX_MONITORS
        );
        monitors.truncate(capped_len);
    }

    monitors.retain_mut(|monitor| {
        if monitor.modes.len() > validate::MAX_MODES_PER_MONITOR {
            warn!(
                "IPC: monitor {} has {} modes, capped at {}",
                monitor.id,
                monitor.modes.len(),
                validate::MAX_MODES_PER_MONITOR
            );
            monitor.modes.truncate(validate::MAX_MODES_PER_MONITOR);
        }

        monitor.modes.retain_mut(|mode| {
            match validate::validate_mode(mode.width, mode.height, &mode.refresh_rates) {
                Ok(rates) => {
                    let dropped = mode.refresh_rates.len() - rates.len();
                    if dropped > 0 {
                        warn!(
                            "IPC: monitor {} mode {}x{}: dropped {} refresh rate(s) \
                             (out-of-range or over per-mode cap {})",
                            monitor.id,
                            mode.width,
                            mode.height,
                            dropped,
                            validate::MAX_RATES_PER_MODE
                        );
                    }
                    mode.refresh_rates = rates;
                    true
                }
                Err(reason) => {
                    warn!(
                        "IPC: monitor {} rejected mode {}x{}: {reason}",
                        monitor.id, mode.width, mode.height
                    );
                    false
                }
            }
        });

        // 启用中的显示器一个合法模式都不剩 => 整个显示器拒收；
        // 禁用中的显示器保留条目（它的删除语义依赖状态留存）
        if monitor.enabled && monitor.modes.is_empty() {
            warn!(
                "IPC: enabled monitor {} has no valid modes left; rejected",
                monitor.id
            );
            return false;
        }

        true
    });

    monitors
}

/// Notifies driver of new system monitor state
///
/// Adds, updates, or removes monitors as needed
///
/// Note that updated monitors causes a detach, update, and reattach. (Required for windows to see the changes)
///
/// Only detaches/reattaches if required
/// e.g. only a monitor name update would not detach/arrive a monitor
fn notify(monitors: Vec<Monitor>) -> Vec<Monitor> {
    // M2(b)：先做输入校验（数量/分辨率/刷新率上限与范围）；返回校验后的列表，
    // 供 process_message 广播（minor(c)），使广播与驱动实际生效状态同源
    let monitors = sanitize_monitors(monitors);

    // Duplicated id's will not cause any issue, however duplicated resolutions/refresh rates are possible
    // They should all be unique anyways. So warn + noop if the sender sent incorrect data
    if has_duplicates(&monitors) {
        warn!("notify(): Duplicate data was detected; update aborted");
        return monitors;
    }

    // M3：adapter 未就绪（IddCxAdapterInitFinished 尚未回调）时收敛为记日志并忽略，
    // 不再 unwrap panic。listener 在 adapter 就绪后才启动，正常路径不会走到这里。
    let Some(adapter) = ADAPTER.get() else {
        error!("notify(): adapter is not initialized yet; ignoring notify");
        return monitors;
    };
    let adapter = adapter.0.as_ptr();

    // M3：互斥量中毒后继续使用既有数据（into_inner），避免 unwrap panic 跨 FFI 边界
    let mut lock = MONITOR_MODES.lock().unwrap_or_else(PoisonError::into_inner);

    // Remove monitors from internal list which are missing from the provided list

    lock.retain_mut(|mon| {
        let id = mon.data.id;
        let found = monitors.iter().any(|m| m.id == id);

        // if it doesn't exist, then add to removal list
        if !found {
            // monitor not found in monitors list, so schedule to remove it
            if let Some(mut obj) = mon.object.take() {
                // remove any monitors scheduled for removal
                let obj = unsafe { obj.as_mut() };
                if let Err(e) = unsafe { IddCxMonitorDeparture(obj) } {
                    error!("Failed to remove monitor: {e:?}");
                }
            }
        }

        found
    });

    // minor(c)：广播列表取 sanitize 后的快照（消费前克隆，与驱动实际生效状态同源）
    let broadcast = monitors.clone();

    let should_arrive = monitors
        .into_iter()
        .map(|monitor| {
            let id = monitor.id;

            let should_arrive;

            let cur_mon = lock.iter_mut().find(|mon| mon.data.id == id);

            if let Some(mon) = cur_mon {
                let modes_changed = mon.data.modes != monitor.modes;

                #[allow(clippy::nonminimal_bool)]
                {
                    should_arrive =
                        // previously was disabled, and it was just enabled
                        (!mon.data.enabled && monitor.enabled) ||
                        // OR monitor is enabled and the display modes changed
                        (monitor.enabled && modes_changed) ||
                        // OR monitor is enabled and the monitor was disconnected
                        (monitor.enabled && mon.object.is_none());
                }

                // should only detach if modes changed, or if state is false
                if modes_changed || !monitor.enabled {
                    if let Some(mut obj) = mon.object.take() {
                        let obj = unsafe { obj.as_mut() };
                        if let Err(e) = unsafe { IddCxMonitorDeparture(obj) } {
                            error!("Failed to remove monitor: {e:?}");
                        }
                    }
                }

                // update monitor data
                mon.data = monitor;
            } else {
                should_arrive = monitor.enabled;

                lock.push(MonitorObject {
                    object: None,
                    data: monitor,
                });
            }

            (id, should_arrive)
        })
        .collect::<Vec<_>>();

    // context.create_monitor locks again, so this avoids deadlock
    drop(lock);

    let cb = |context: &mut DeviceContext| {
        // arrive any monitors that need arriving
        for (id, arrive) in should_arrive {
            if arrive {
                if let Err(e) = context.create_monitor(id) {
                    error!("Failed to create monitor: {e:?}");
                }
            }
        }
    };

    // M3：取设备上下文失败（含未初始化/锁失败）收敛为记日志，不再 unwrap panic
    if let Err(e) = unsafe { DeviceContext::get_mut(adapter.cast(), cb) } {
        error!("notify(): failed to access device context: {e:?}");
    }

    broadcast
}

fn remove_all() {
    // M3：互斥量中毒后继续使用既有数据（into_inner），避免 unwrap panic 跨 FFI 边界
    let mut lock = MONITOR_MODES.lock().unwrap_or_else(PoisonError::into_inner);

    for monitor in lock.drain(..) {
        if let Some(mut monitor_object) = monitor.object {
            let obj = unsafe { monitor_object.as_mut() };
            if let Err(e) = unsafe { IddCxMonitorDeparture(obj) } {
                error!("Failed to remove monitor: {e:?}");
            }
        }
    }
}

fn remove(ids: &[u32]) {
    // M3：互斥量中毒后继续使用既有数据（into_inner），避免 unwrap panic 跨 FFI 边界
    let mut lock = MONITOR_MODES.lock().unwrap_or_else(PoisonError::into_inner);

    for &id in ids {
        lock.retain_mut(|monitor| {
            if id == monitor.data.id {
                if let Some(mut monitor_object) = monitor.object.take() {
                    let obj = unsafe { monitor_object.as_mut() };
                    if let Err(e) = unsafe { IddCxMonitorDeparture(obj) } {
                        error!("Failed to remove monitor: {e:?}");
                    }
                }

                false
            } else {
                true
            }
        });
    }
}

pub trait FlattenModes {
    fn flatten(&self) -> impl Iterator<Item = ModeItem>;
}

#[derive(Copy, Clone)]
pub struct ModeItem {
    pub width: Dimen,
    pub height: Dimen,
    pub refresh_rate: RefreshRate,
}

/// Takes a slice of modes and creates a flattened structure that can be iterated over
impl FlattenModes for Vec<Mode> {
    fn flatten(&self) -> impl Iterator<Item = ModeItem> {
        self.iter().flat_map(|m| {
            m.refresh_rates.iter().map(|&rr| ModeItem {
                width: m.width,
                height: m.height,
                refresh_rate: rr,
            })
        })
    }
}
