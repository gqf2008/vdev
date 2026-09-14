# 单端点"流是否真的在动"探测：render 看 GetCurrentPadding 是否随播放下降，
# capture 看 GetNextPacketSize 是否出现数据包。逐步 flush 输出，便于定位卡点。
# 端点 GUID 动态发现（重装驱动后会变），无需参数。
#
# 用法：powershell -ExecutionPolicy Bypass -File scripts\acceptance\audio-flow-probe.ps1
#       powershell -ExecutionPolicy Bypass -File scripts\acceptance\audio-flow-probe.ps1 -single
Add-Type -TypeDefinition @'
using System;
using System.Runtime.InteropServices;
using System.Text;
using System.Threading;

namespace VdevFlow
{
    [ComImport, Guid("BCDE0395-E52F-467C-8E3D-C4579291692E")]
    internal class MMDeviceEnumeratorComObject { }

    [Guid("A95664D2-9614-4F35-A746-DE8DB63617E6"), InterfaceType(ComInterfaceType.InterfaceIsIUnknown)]
    internal interface IMMDeviceEnumerator
    {
        [PreserveSig] int EnumAudioEndpoints(int dataFlow, int stateMask, out IMMDeviceCollection devices);
        [PreserveSig] int GetDefaultAudioEndpoint(int dataFlow, int role, out IntPtr endpoint);
        [PreserveSig] int GetDevice([MarshalAs(UnmanagedType.LPWStr)] string id, out IntPtr device);
        [PreserveSig] int RegisterEndpointNotificationCallback(IntPtr client);
        [PreserveSig] int UnregisterEndpointNotificationCallback(IntPtr client);
    }

    [Guid("0BD7A1BE-7A1A-44DB-8397-CC5392387B5E"), InterfaceType(ComInterfaceType.InterfaceIsIUnknown)]
    internal interface IMMDeviceCollection
    {
        [PreserveSig] int GetCount(out int count);
        [PreserveSig] int Item(int index, out IMMDevice device);
    }

    [Guid("D666063F-1587-4E43-81F1-B948E807363F"), InterfaceType(ComInterfaceType.InterfaceIsIUnknown)]
    internal interface IMMDevice
    {
        [PreserveSig] int Activate(ref Guid iid, int clsCtx, IntPtr activationParams, [MarshalAs(UnmanagedType.IUnknown)] out object iface);
        [PreserveSig] int OpenPropertyStore(int access, out IntPtr props);
        [PreserveSig] int GetId([MarshalAs(UnmanagedType.LPWStr)] out string id);
        [PreserveSig] int GetState(out int state);
    }

    [Guid("1CB9AD4C-DBFA-4C32-B178-C2F568A703B2"), InterfaceType(ComInterfaceType.InterfaceIsIUnknown)]
    internal interface IAudioClient
    {
        [PreserveSig] int Initialize(int shareMode, int streamFlags, long bufferDuration, long periodicity, IntPtr format, IntPtr sessionGuid);
        [PreserveSig] int GetBufferSize(out int frames);
        [PreserveSig] int GetStreamLatency(out long latency);
        [PreserveSig] int GetCurrentPadding(out int padding);
        [PreserveSig] int IsFormatSupported(int shareMode, IntPtr format, out IntPtr closest);
        [PreserveSig] int GetMixFormat(out IntPtr format);
        [PreserveSig] int GetDevicePeriod(out long defaultPeriod, out long minimumPeriod);
        [PreserveSig] int Start();
        [PreserveSig] int Stop();
        [PreserveSig] int Reset();
        [PreserveSig] int SetEventHandle(IntPtr handle);
        [PreserveSig] int GetService(ref Guid iid, [MarshalAs(UnmanagedType.IUnknown)] out object service);
    }

    [Guid("F294ACFC-3146-4483-A7BF-ADDCA7C260E2"), InterfaceType(ComInterfaceType.InterfaceIsIUnknown)]
    internal interface IAudioRenderClient
    {
        [PreserveSig] int GetBuffer(int frames, out IntPtr buffer);
        [PreserveSig] int ReleaseBuffer(int frames, int flags);
    }

    [Guid("C8ADBD64-E71E-48A0-A4DE-185C395CD317"), InterfaceType(ComInterfaceType.InterfaceIsIUnknown)]
    internal interface IAudioCaptureClient
    {
        [PreserveSig] int GetBuffer(out IntPtr data, out int frames, out int flags, out long devicePosition, out long qpcPosition);
        [PreserveSig] int ReleaseBuffer(int frames);
        [PreserveSig] int GetNextPacketSize(out int frames);
    }

    public static class Flow
    {
        static string H(int hr) { return "0x" + hr.ToString("X8"); }

        static IMMDevice Find(string guid, bool capture)
        {
            var en = (IMMDeviceEnumerator)(new MMDeviceEnumeratorComObject());
            IMMDeviceCollection col;
            int hr = en.EnumAudioEndpoints(capture ? 1 : 0, 1, out col);
            if (hr < 0) throw new Exception("EnumAudioEndpoints " + H(hr));
            int n; col.GetCount(out n);
            for (int i = 0; i < n; i++)
            {
                IMMDevice d; col.Item(i, out d);
                string id; d.GetId(out id);
                if (id.ToUpper().Contains(guid.ToUpper())) return d;
            }
            throw new Exception("未找到端点 " + guid);
        }

        /// render：播放 1 秒 1kHz 正弦，同时每 100ms 打印 padding（应随播放下降）
        public static string Render(string guid)
        {
            var sb = new StringBuilder();
            var dev = Find(guid, false);
            var iid = new Guid("1CB9AD4C-DBFA-4C32-B178-C2F568A703B2");
            object o; int hr = dev.Activate(ref iid, 0x17, IntPtr.Zero, out o);
            sb.AppendLine("Activate hr=" + H(hr));
            var ac = (IAudioClient)o;
            IntPtr mix;
            hr = ac.GetMixFormat(out mix);
            sb.AppendLine("GetMixFormat hr=" + H(hr) + " tag=0x" + Marshal.ReadInt16(mix, 0).ToString("X4")
                + " ch=" + Marshal.ReadInt16(mix, 2) + " rate=" + Marshal.ReadInt32(mix, 4)
                + " bits=" + Marshal.ReadInt16(mix, 14) + " blockAlign=" + Marshal.ReadInt16(mix, 12));
            hr = ac.Initialize(0, 0, 10000000, 0, mix, IntPtr.Zero);
            sb.AppendLine("Initialize(shared, mix) hr=" + H(hr));
            if (hr < 0) return sb.ToString();
            int buf; ac.GetBufferSize(out buf);
            sb.AppendLine("GetBufferSize=" + buf + " frames");
            var iidR = new Guid("F294ACFC-3146-4483-A7BF-ADDCA7C260E2");
            object svc; hr = ac.GetService(ref iidR, out svc);
            sb.AppendLine("GetService(render) hr=" + H(hr));
            var rc = (IAudioRenderClient)svc;
            hr = ac.Start();
            sb.AppendLine("Start hr=" + H(hr));
            int ch = Marshal.ReadInt16(mix, 2);
            int rate = Marshal.ReadInt32(mix, 4);
            int tag = (ushort)Marshal.ReadInt16(mix, 0);
            bool isFloat = tag == 3 || (tag == 0xFFFE && Marshal.ReadInt16(mix, 14) == 32);
            long written = 0, want = rate; // 1 秒
            for (int iter = 0; iter < 40 && written < want; iter++)
            {
                int pad; hr = ac.GetCurrentPadding(out pad);
                int avail = buf - pad;
                sb.AppendLine(string.Format("  t={0}ms padding={1} avail={2} written={3} hr={4}", iter * 100, pad, avail, written, H(hr)));
                if (avail > 0)
                {
                    int nn = (int)Math.Min(avail, want - written);
                    IntPtr pb; hr = rc.GetBuffer(nn, out pb);
                    if (hr == 0)
                    {
                        for (int f = 0; f < nn; f++)
                        {
                            double t = (written + f) / (double)rate;
                            double v = 0.5 * Math.Sin(2 * Math.PI * 1000.0 * t);
                            for (int c = 0; c < ch; c++)
                            {
                                int idx = f * ch + c;
                                if (isFloat) Marshal.StructureToPtr((float)v, IntPtr.Add(pb, idx * 4), false);
                                else Marshal.WriteInt16(IntPtr.Add(pb, idx * 2), (short)(v * 32767));
                            }
                        }
                        rc.ReleaseBuffer(nn, 0);
                        written += nn;
                    }
                    else sb.AppendLine("    GetBuffer(render) hr=" + H(hr));
                }
                Thread.Sleep(100);
            }
            ac.Stop();
            sb.AppendLine("写入完成 written=" + written + " / want=" + want);
            return sb.ToString();
        }

        /// capture：采集 1 秒，打印包数与帧数
        public static string Capture(string guid)
        {
            var sb = new StringBuilder();
            var dev = Find(guid, true);
            var iid = new Guid("1CB9AD4C-DBFA-4C32-B178-C2F568A703B2");
            object o; int hr = dev.Activate(ref iid, 0x17, IntPtr.Zero, out o);
            sb.AppendLine("Activate hr=" + H(hr));
            var ac = (IAudioClient)o;
            IntPtr mix;
            hr = ac.GetMixFormat(out mix);
            sb.AppendLine("GetMixFormat hr=" + H(hr) + " tag=0x" + Marshal.ReadInt16(mix, 0).ToString("X4")
                + " ch=" + Marshal.ReadInt16(mix, 2) + " rate=" + Marshal.ReadInt32(mix, 4)
                + " bits=" + Marshal.ReadInt16(mix, 14));
            hr = ac.Initialize(0, 0, 10000000, 0, mix, IntPtr.Zero);
            sb.AppendLine("Initialize(shared, mix) hr=" + H(hr));
            if (hr < 0) return sb.ToString();
            int buf; ac.GetBufferSize(out buf);
            var iidC = new Guid("C8ADBD64-E71E-48A0-A4DE-185C395CD317");
            object svc; hr = ac.GetService(ref iidC, out svc);
            sb.AppendLine("GetService(capture) hr=" + H(hr) + " buffer=" + buf + " frames");
            var cc = (IAudioCaptureClient)svc;
            hr = ac.Start();
            sb.AppendLine("Start hr=" + H(hr));
            long total = 0; int packets = 0; double peak = 0;
            for (int iter = 0; iter < 20; iter++)
            {
                int pkt; hr = cc.GetNextPacketSize(out pkt);
                if (pkt > 0)
                {
                    IntPtr pd; int frames, flags; long dp, qp;
                    hr = cc.GetBuffer(out pd, out frames, out flags, out dp, out qp);
                    if (hr == 0)
                    {
                        packets++;
                        total += frames;
                        for (int i = 0; i < frames * 2; i++)
                        {
                            short s = Marshal.ReadInt16(IntPtr.Add(pd, i * 2));
                            double v = Math.Abs(s / 32768.0);
                            if (v > peak) peak = v;
                        }
                        cc.ReleaseBuffer(frames);
                    }
                }
                if (iter % 5 == 0) sb.AppendLine(string.Format("  t={0}ms packets={1} frames={2} peak={3:F4}", iter * 100, packets, total, peak));
                Thread.Sleep(100);
            }
            ac.Stop();
            sb.AppendLine(string.Format("采集完成 packets={0} frames={1} peak={2:F4}", packets, total, peak));
            return sb.ToString();
        }

        /// 同时开 render + capture：边写正弦边收包，打印 render padding 与 capture 包数
        /// （padding 应随播放下降 → 驱动时钟在走；capture 应收到数据包 → 环回搬运在走）
        public static string Loopback(string renderGuid, string captureGuid, int seconds)
        {
            var sb = new StringBuilder();
            var iid = new Guid("1CB9AD4C-DBFA-4C32-B178-C2F568A703B2");

            var devR = Find(renderGuid, false);
            object oR; int hr = devR.Activate(ref iid, 0x17, IntPtr.Zero, out oR);
            var acR = (IAudioClient)oR;
            IntPtr mixR; hr = acR.GetMixFormat(out mixR);
            hr = acR.Initialize(0, 0, 10000000, 0, mixR, IntPtr.Zero);
            sb.AppendLine("render Initialize hr=" + H(hr));
            int bufR; acR.GetBufferSize(out bufR);
            var iidR = new Guid("F294ACFC-3146-4483-A7BF-ADDCA7C260E2");
            object svcR; hr = acR.GetService(ref iidR, out svcR);
            var rc = (IAudioRenderClient)svcR;

            var devC = Find(captureGuid, true);
            object oC; hr = devC.Activate(ref iid, 0x17, IntPtr.Zero, out oC);
            var acC = (IAudioClient)oC;
            IntPtr mixC; hr = acC.GetMixFormat(out mixC);
            hr = acC.Initialize(0, 0, 10000000, 0, mixC, IntPtr.Zero);
            sb.AppendLine("capture Initialize hr=" + H(hr));
            int bufC; acC.GetBufferSize(out bufC);
            var iidC = new Guid("C8ADBD64-E71E-48A0-A4DE-185C395CD317");
            object svcC; hr = acC.GetService(ref iidC, out svcC);
            var cc = (IAudioCaptureClient)svcC;

            sb.AppendLine("render buffer=" + bufR + " frames, capture buffer=" + bufC + " frames");
            acC.Start(); acR.Start();
            int ch = Marshal.ReadInt16(mixR, 2);
            int rate = Marshal.ReadInt32(mixR, 4);
            int tag = (ushort)Marshal.ReadInt16(mixR, 0);
            bool isFloat = tag == 3 || (tag == 0xFFFE && Marshal.ReadInt16(mixR, 14) == 32);
            long written = 0; long total = 0; int packets = 0; double peak = 0;
            var sw = System.Diagnostics.Stopwatch.StartNew();
            int tick = 0;
            while (sw.Elapsed.TotalSeconds < seconds)
            {
                int pad; acR.GetCurrentPadding(out pad);
                int avail = bufR - pad;
                if (avail > 0)
                {
                    IntPtr pb; int h2 = rc.GetBuffer(avail, out pb);
                    if (h2 == 0)
                    {
                        for (int f = 0; f < avail; f++)
                        {
                            double t = (written + f) / (double)rate;
                            double v = 0.5 * Math.Sin(2 * Math.PI * 1000.0 * t);
                            for (int c = 0; c < ch; c++)
                            {
                                int idx = f * ch + c;
                                if (isFloat) Marshal.StructureToPtr((float)v, IntPtr.Add(pb, idx * 4), false);
                                else Marshal.WriteInt16(IntPtr.Add(pb, idx * 2), (short)(v * 32767));
                            }
                        }
                        rc.ReleaseBuffer(avail, 0);
                        written += avail;
                    }
                }
                int pkt; cc.GetNextPacketSize(out pkt);
                while (pkt > 0)
                {
                    IntPtr pd; int frames, flags; long dp, qp;
                    int h3 = cc.GetBuffer(out pd, out frames, out flags, out dp, out qp);
                    if (h3 != 0) break;
                    packets++; total += frames;
                    for (int i = 0; i < frames * 2; i++)
                    {
                        double v = Math.Abs(Marshal.ReadInt16(IntPtr.Add(pd, i * 2)) / 32768.0);
                        if (v > peak) peak = v;
                    }
                    cc.ReleaseBuffer(frames);
                    cc.GetNextPacketSize(out pkt);
                }
                if (tick++ % 5 == 0)
                    sb.AppendLine(string.Format("  t={0:F1}s render_padding={1} written={2} capture_packets={3} frames={4} peak={5:F4}",
                        sw.Elapsed.TotalSeconds, pad, written, packets, total, peak));
                Thread.Sleep(100);
            }
            acR.Stop(); acC.Stop();
            double rmsDb = peak <= 0 ? -100 : 20 * Math.Log10(peak);
            sb.AppendLine(string.Format("结论：环回 packets={0} frames={1} peak={2:F4} ({3:F1} dBFS) → {4}",
                packets, total, peak, rmsDb, packets > 0 ? "收到数据" : "无数据"));
            return sb.ToString();
        }
    }
}
'@

# 端点 GUID 动态取（每次重装驱动会变）
$eps = Get-PnpDevice -Class AudioEndpoint -ErrorAction SilentlyContinue |
       Where-Object { $_.Status -eq 'OK' -and $_.FriendlyName -match 'vdev' -and $_.InstanceId -match 'SWD\\MMDEVAPI' }
$renderGuid = $null; $captureGuid = $null
foreach ($e in $eps) {
    $g = ([regex]::Match($e.InstanceId, '\{([0-9a-fA-F\-]{36})\}')).Groups[1].Value
    if ($e.InstanceId -like '*{0.0.1.*') { $captureGuid = $g } else { $renderGuid = $g }
}
if ($args -contains '-single') {
    Write-Output ("########## render  {0}" -f $renderGuid)
    Write-Output ([VdevFlow.Flow]::Render($renderGuid))
    Write-Output ("########## capture {0}" -f $captureGuid)
    Write-Output ([VdevFlow.Flow]::Capture($captureGuid))
} else {
    Write-Output ("########## 环回：render {0} → capture {1}" -f $renderGuid, $captureGuid)
    Write-Output ([VdevFlow.Flow]::Loopback($renderGuid, $captureGuid, 4))
}
