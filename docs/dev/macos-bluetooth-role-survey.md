# 调研笔记：macOS 能否把蓝牙仿真成手机可识别的耳麦 / 音箱

> **结论先行（2026-09-18 实测，macOS 26.5.2 + 一加 13T / Android）**：
> **耳麦只能做到"被手机认出来"，通话音频拿不到；音箱完全不可行。不要再投入。**

| 目标 | 结果 |
|---|---|
| 手机把 Mac 认成**蓝牙耳麦** | ✅ 能认：HFP 控制面完全可用（SLC、来电显示、从 Mac 拨号、通话状态、挂断） |
| 通话音频落到 Mac | ❌ 做不到：两轮真实通话中每一次请求都返回 `kIOReturnUnsupported` |
| 手机把 Mac 认成**蓝牙音箱** | ❌ 完全不可行：macOS 没有 A2DP Sink，也没有注册该角色的 API |

## 需求与背景

目标曾是「把 Mac 上的蓝牙仿真为手机能识别的蓝牙耳麦**和**蓝牙音箱」。这个方向的可行性
**必须按角色方向拆开判断**，只看协议名（HFP / A2DP）会得出错误结论：

| | 主机侧 | 外设侧 |
|---|---|---|
| A2DP | **Source**（放歌给音箱）—— macOS 有 | **Sink**（当音箱收歌）—— macOS **没有** |
| HFP | **AG**（音网关，耳机连它）—— macOS 有 | **HF**（免手持，连手机的 AG）—— 有 API，**但音频面已被删** |

macOS 的蓝牙栈只实现了主机侧。两条证据：

1. **公开头文件里没有任何 A2DP 符号**（`IOBluetooth.framework/Headers/` 下只有私有的
   `_A2DPLog`），而 `IOBluetoothDevice.isA2DPSink` / `isA2DPSource` 的语义是
   **"对端设备"**是不是 sink/source——不是本机角色。
2. `bluetoothd` 里的 `sink` 语义**全部指向对端**（`Ignoring sink's SBC Maximum Bitpool Value`、
   `LastA2DPSinkSupportedFeatures`、`Audio sink service not supported by device %s`），
   同时能直接看到字面量 `Advertising HFP AG`（本机对外广播的是 AG）。字符串证据单独看
   可以被反向解读，所以只作旁证，主证据是第 1 条。

所以"当音箱"从一开始就没有实现可依附。

## 1. 系统 API 路线（HFP-HF）：控制面可用，音频面被苹果删掉了

`IOBluetooth.framework` 提供 `IOBluetoothHandsFreeDevice`（macOS 10.7+），这是唯一
「本机当耳机」的入口；系统 UI 从不使用它，历史上只有 Phone Amego / BluePhoneElite 这类
第三方 App 在用。

### 实测（macOS 26.5.2 + 一加 13T）

- **控制面全部可用**：SLC 约 1 秒建立；手机主动上报 `service=1 signal=5 batt=5 roam=0`；
  本机 HF `supportedFeatures=0x74`（CLIP + 远端音量 + 增强呼叫状态/控制），
  对端 AG `deviceSupportedFeatures=0x12F`；`--dial` 能真的把电话拨出去
  （`callsetup` 走 2→3→`call=1`→`call=0` 完整流程）。
- **音频面拿不到**：通话接通的 18.9 秒里每 3 秒重试一次 `transferAudioToComputer`
  （共 6 次），**每一次重试后都收到 SCO 打开失败回调，`status=-536870201`
  （`0xE00002C7 = kIOReturnUnsupported`）**。
  口径说明：`transferAudioToComputer` 的签名是 `- (void)`（本机 SDK
  `IOBluetoothHandsFreeDevice.h:165`），**没有返回值**；失败码来自
  `handsFree:scoConnectionOpened:` 委托回调。探针会把回调编号（`[SCO 回调 #N]`）
  与重试序号成对打印，便于把每一条失败与被调方的哪一次请求对上。
  `isSCOConnected` 恒为 0、`inputAudioDeviceID/outputAudioDeviceID` 恒为 0、
  CoreAudio 全程不出现任何蓝牙端点。与上一轮 31.8 秒的通话合并看，
  两轮真实通话、**每次请求都失败**。
- **AG 侧也对不上**：手机通话界面里**能看到那台 Mac、却切不过去**——说明不是"手机不肯给音频"，
  而是对面接不住。

### 关键证据：同一个 ivar，旧 SDK 有、现在没了

同一份 `IOBluetoothHandsFree` 头文件：

- **10.7 / 10.8 SDK**（GitHub `phracker/MacOSX-SDKs`）里有
  `@class IOBluetoothSCOAudioDevice;` 与 `IOBluetoothSCOAudioDevice * _scoAudioDevice;`
- **macOS 26 SDK** 里该 ivar **已被删除**（本地 `xcrun --show-sdk-path` 后看
  `IOBluetooth.framework/Headers/objc/IOBluetoothHandsFree.h`），剩下的 ivar 是
  `_rfcommChannel / _supportedFeatures / _previousInputVolume / … / _connectSCOAfterSLCConnected / _reserved`。

骨架还在（`connectSCO`、`isSCOConnected`、`_connectSCOAfterSLCConnected` 都保留），
**只有音频桥没了**——这与实测的**即时**失败完全吻合（原始日志里发起与失败回调落在
**同一个时间戳** `[18.029s]`，即同一毫秒内；精确到毫秒以下无法从该日志推断）：
没有任何 HCI 尝试的痕迹，是本地代码路径不存在，不是链路层超时。

### 对照组：macOS 做 HFP 音频本身没问题，缺的是 HF 侧的桥

同一台 Mac 接一副真蓝牙耳机时，CoreAudio 里正常出现两个端点：

```
id=175  OpenDots ONE by Shokz   in=1   out=0   16000 Hz   ← HFP 麦克风（mSBC 宽频）
id=169  OpenDots ONE by Shokz   in=0   out=2   44100 Hz   ← A2DP 播放
```

即：**macOS 当 AG 时 HFP 音频是好的，当 HF 时没有音频面。**

## 2. 更老的 SCO 音频 API：10.9 起已废弃

`IOBluetoothAddSCOAudioDevice` / `IOBluetoothRemoveSCOAudioDevice` 的公开头文件标注是
`DEPRECATED_IN_MAC_OS_X_VERSION_10_9_AND_LATER`。这与 Phone Amego 作者的自述吻合：

> "Under Mavericks, Phone Amego doesn't process this audio itself, it just enables the
> corresponding network service. **Prior to Mavericks, Phone Amego would route the call
> audio from the Bluetooth audio driver** to the selected audio devices."

出处：Phone Amego 用户指南 *Apple Feedback* 页，`http://www.sustworks.com/pa_guide/AppleFeedback.html`
（2026-09-18 抓取；作者自述，未做第三方独立复核）。

沿革可以拼成一条线：**10.9 之前 App 自己搬 SCO 音频 → 10.9 之后交给系统 →
`_scoAudioDevice` ivar 被删 → macOS 26 上 HF 侧再无音频。**

## 3. 极端路线（绕过框架直接发 HCI）：通路可用，但拿不到句柄

既然类接口被掏空，就试了控制器层。`scripts/acceptance/macos-bluetooth-hci-probe.m` 用
`IOBluetoothHostController` 的私有 HCI 方法（签名取自 GitHub `colemancda/HCITool` 的
class-dump 头）直接发命令。

- ✅ **用户态确实能跟控制器说上话，且不需要 root**。证据必须用"投毒—断言"取，
  **不能只看返回码**：
  - 反面例子：`-[IOBluetoothHostController BluetoothHCIReadVoiceSetting:]` 返回
    `kIOReturnSuccess`，但**完全不写 out 参数**（预置 `0xABCD` 后原样保留）。
    只看返回码会得到"无条件通过"的假阳性——初版探针就踩了这个坑。
  - 正面例子：`-[IOBluetoothHostController BluetoothHCIReadLocalName:]` 把缓冲区
    从投毒值 `0xAA` 覆盖成真实名字（实测得到本机名），据此才成立。
  探针现在把这两条都打出来（自检 A 反面 / 自检 B 正面），把坑本身留作可复现的教材。
- ❌ **拿不到连接句柄**：`[IOBluetoothDevice connectionHandle]` 与 `getConnectionHandle`
  在 macOS 26 上**恒为 0**（此时 `isConnected=1`、SLC 正常），`getLinkType` 返回 255（无效值）。
  没有句柄就无法构造指向该链路的 `Setup Synchronous Connection`。
- ⚠️ **假阳性陷阱**：用 `handle=0` 调用 `BluetoothHCISetupSynchronousConnection:…`
  会返回 `kIOReturnSuccess` 却**什么都不做**（out 结构全零）。探针因此以
  `out.connectionHandle != 0` 作为真正的成功判据，而不是只看返回码。

剩下的续命做法是从 HCI 事件流里抓 Connection Complete 事件取句柄（需自行逆向
`IOBluetoothHCIEventNotificationMessage` 结构 + 强制重连一次）。**但天花板没变**：
即使 eSCO 建起来，**音频桥已经不存在**，内置 BCM 控制器的 SCO 音频不走 HCI 到用户态，
macOS 也没有任何 API 把 SCO 数据递给应用进程。所以这条路**不再投入**。

## 4. 第三方产品是怎么做的（不能当作可行性依据）

| 产品 | 实际做法 | 今天的状态 |
|---|---|---|
| BluePhoneElite（2008 前后） | 早于 10.7，**自己实现 HFP-HF**，用 `IOBluetoothSCOAudioDevice` | 该类已删，跑不动 |
| Phone Amego（10.7–10.8） | 同样自己搬音频 | `IOBluetoothAddSCOAudioDevice` 10.9 起废弃 |
| Phone Amego（10.9+） | 只启用服务，音频交给系统 | 最后更新 2023-02（Ventura 上编译），release notes 顶部单列 "Bluetooth audio compatibility note" |
| HandsFree 2（Tunabelly） | 同一条 HFP-HF 路，**只承诺 Android** | 已从厂商官网下架，`/handsfree/` 页面被 Texty 顶替 |
| Windows Phone Link | **Windows 系统自带** HFP HF + SCO 音频路由 | 仍可用；通话只支持 Android 是因为呼叫控制要走手机端 App，与蓝牙无关 |

Phone Amego 作者还贴过当年失败的内核日志
（`[0x0407] (Add SCO Connection) … kBluetoothHCIErrorHostTimeout` +
`IOBluetoothSCOAudioDevice.cpp:882`），同一配置在 MacBook Pro 能用、在 2009 Mac Pro 不能用，
他给出的"极端解法"是**换第三方 USB 蓝牙 dongle**。也就是说这条路当年就脆弱、
依赖蓝牙芯片，现在被直接移除。GitHub 上也**没有任何替代实现**：代码搜索
`IOBluetoothHandsFreeDevice` 只有 SDK 归档与 darling 的 stub，没有真实项目在用它。

## 复现方法

探针在同目录 [`scripts/acceptance/`](../../scripts/acceptance/) 下，一条命令构建：

```bash
# 只读：列出已配对设备、CoD 解码、SDP 角色识别（不改变持久状态）
./scripts/acceptance/macos-bluetooth-role-probe.sh list

# 只读：对某台设备发起 SDP 查询并打印服务（会向对端发起查询、建立 ACL 连接，但不留持久状态）
./scripts/acceptance/macos-bluetooth-role-probe.sh sdp "<地址|名字>"

# 会连手机（可逆，退出即断）：观察 SLC 与通话中 SCO 的真实状态
./scripts/acceptance/macos-bluetooth-role-probe.sh hfp "<地址|名字>" --seconds 90 --audio

# 会真的拨号（慎用）：把「通话中 SCO 是否 status=0」这条最关键的判据跑出来
./scripts/acceptance/macos-bluetooth-role-probe.sh hfp "<地址|名字>" --seconds 90 --audio --reset --dial 10086

# 控制器层：先看"投毒—断言"式自检 A/B，再看 SCO 命令结果
./scripts/acceptance/macos-bluetooth-role-probe.sh hci --addr <aa:bb:cc:dd:ee:ff>
```

产物默认写 `${TMPDIR:-/tmp}/vdev-bt-probe`（`-OutDir` 可覆盖），源文件没变不重编。

## 留给后来者的三条判定顺序

1. **先把"通话中 SCO 是否 status=0"跑出来再决定投入**。只看到 SLC 建立成功、
   无通话时 `connectSCO` 返回 `kIOReturnUnsupported`，就推断"等有通话就好了"，
   是本轮实际发生过的误判。
2. **阳性对照必须是"投毒—断言"式，不能只看返回码**：先往 out 参数/缓冲区写哨兵值，
   再断言它被改写。本轮就有一条命令（Read Voice Setting）返回 `kIOReturnSuccess`
   却什么都不写，只看返回码会无条件通过。
3. **对关键符号做 A/B**：怀疑某个能力被系统移除时，拿旧 SDK 头文件与当前 SDK 对比
   （`phracker/MacOSX-SDKs` 可直接取 10.5–11.x）。本轮就是靠 `_scoAudioDevice` 的
   有无把结论钉死的。

## 如果将来一定要做

唯一确定可行的方向是**外挂一颗自带协议栈的蓝牙模块**（如 ESP32-S3 跑 ESP-ADF 的
A2DP Sink + HFP HF，再用 USB UAC 接回 Mac）——手机看到的是模块而非 Mac，
Mac 侧只当一个普通 USB 声卡，完全不碰 macOS 蓝牙栈。

**本轮明确不做**：2026-09-18 用户决定暂不引入硬件方案，因此本文只入档结论与探针，
不实现任何蓝牙虚拟设备。

## 相关

- 逐条经验（含三处假阳性陷阱、IOReturn 解码表、HCI 阳性对照方法）沉淀在
  `~/.agents/rules/LESSON_macOS蓝牙角色分方向HFP-HF控制面可用音频面不可用A2DP-Sink不可做.md`
- 路线级背景见 [macos-route-survey.md](macos-route-survey.md)
