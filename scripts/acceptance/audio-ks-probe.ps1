# 用 IOCTL_KS_PROPERTY 直接向内核问每个 pin 的 DATAFLOW/COMMUNICATION/CATEGORY，
# 对 vdev 与同机可用的 ToDesk 虚拟声卡做逐格对照（METHOD_NEITHER：lpInBuffer/lpOutBuffer
# 传的就是用户态结构地址本身）。
#
# 用法：powershell -ExecutionPolicy Bypass -File scripts\acceptance\audio-ks-probe.ps1
#   ROOT\MEDIA\<实例号> 会随重装变化，默认 vdev=0001 / 对照=0000；
#   先 `pnputil /enum-devices /class MEDIA` 看实际实例号，再用 -VdevInstance / -RefInstance 覆盖。
param(
    [string]$VdevInstance = '0001',
    [string]$RefInstance = '0000',
    [string]$RefName = 'ToDesk'
)
Add-Type -TypeDefinition @'
using System;
using System.Runtime.InteropServices;
using System.Text;

public static class KsProbe
{
    [DllImport("kernel32.dll", CharSet = CharSet.Unicode, SetLastError = true)]
    static extern IntPtr CreateFileW(string name, uint access, uint share, IntPtr sa, uint disp, uint flags, IntPtr tmpl);
    [DllImport("kernel32.dll", SetLastError = true)]
    static extern bool DeviceIoControl(IntPtr h, uint code, IntPtr inBuf, uint inLen, IntPtr outBuf, uint outLen, out uint ret, IntPtr ov);
    [DllImport("kernel32.dll", SetLastError = true)]
    static extern bool CloseHandle(IntPtr h);

    const uint IOCTL_KS_PROPERTY = 0x002F0003;
    const uint GENERIC_READ = 0x80000000, GENERIC_WRITE = 0x40000000;
    const uint OPEN_EXISTING = 3;
    const uint KSPROPERTY_TYPE_GET = 1;

    // KSPROPSETID_Pin
    // ks.h：DEFINE_GUIDSTRUCT("8C134960-51AD-11CF-878A-94F801C10000", KSPROPSETID_Pin)
    static readonly Guid KSPSETID_PIN = new Guid("8C134960-51AD-11CF-878A-94F801C10000");
    const uint PIN_CINSTANCES = 0, PIN_CTYPES = 1, PIN_DATAFLOW = 2, PIN_DATARANGES = 3,
               PIN_COMMUNICATION = 7, PIN_PHYSICALCONNECTION = 10, PIN_CATEGORY = 11, PIN_NAME = 12,
               PIN_PROPOSEDATAFORMAT = 14, PIN_PROPOSEDATAFORMAT2 = 15;

    /// 滤波器级属性（KSPROPERTY + 可空 PinId）：返回原始字节十六进制，便于逐字节对照
    public static string FilterProperty(string path, string setId, uint id, int outLen)
    {
        IntPtr h = CreateFileW(path, GENERIC_READ | GENERIC_WRITE, 3, IntPtr.Zero, OPEN_EXISTING, 0, IntPtr.Zero);
        if (h.ToInt64() == -1)
            h = CreateFileW(path, GENERIC_READ, 3, IntPtr.Zero, OPEN_EXISTING, 0, IntPtr.Zero);
        if (h.ToInt64() == -1) return "OPENFAIL err=" + Marshal.GetLastWin32Error();
        IntPtr inBuf = Marshal.AllocHGlobal(64);
        IntPtr outBuf = Marshal.AllocHGlobal(outLen);
        try
        {
            byte[] g = new Guid(setId).ToByteArray();
            Marshal.Copy(g, 0, inBuf, 16);
            Marshal.WriteInt32(inBuf, 16, (int)id);
            Marshal.WriteInt32(inBuf, 20, (int)KSPROPERTY_TYPE_GET);
            for (int i = 0; i < outLen; i++) Marshal.WriteByte(outBuf, i, 0);
            uint ret;
            bool ok = DeviceIoControl(h, IOCTL_KS_PROPERTY, inBuf, 24, outBuf, (uint)outLen, out ret, IntPtr.Zero);
            if (!ok) return "IOCTLFAIL err=" + Marshal.GetLastWin32Error();
            StringBuilder sb = new StringBuilder();
            sb.Append("ret=").Append(ret).Append(" :");
            if (id == 1 || id == 2) // KSPROPERTY_TOPOLOGY_NODES / _CONNECTIONS 的头三段
            {
                sb.Append(" cats=").Append(Marshal.ReadInt32(outBuf))
                  .Append(" nodes=").Append(Marshal.ReadInt32(outBuf, 8))
                  .Append(" conns=").Append(Marshal.ReadInt32(outBuf, 16));
            }
            else
            {
                int show = Math.Min((int)ret, 64);
                for (int i = 0; i < show; i++) sb.Append(' ').Append(Marshal.ReadByte(outBuf, i).ToString("x2"));
            }
            return sb.ToString();
        }
        finally { Marshal.FreeHGlobal(inBuf); Marshal.FreeHGlobal(outBuf); CloseHandle(h); }
    }

    public static string Query(string path, uint pinId, uint propId, int outLen)
    {
        IntPtr h = CreateFileW(path, GENERIC_READ | GENERIC_WRITE, 3, IntPtr.Zero, OPEN_EXISTING, 0, IntPtr.Zero);
        if (h.ToInt64() == -1)
            h = CreateFileW(path, GENERIC_READ, 3, IntPtr.Zero, OPEN_EXISTING, 0, IntPtr.Zero);
        if (h.ToInt64() == -1)
            return "OPENFAIL err=" + Marshal.GetLastWin32Error();

        IntPtr inBuf = Marshal.AllocHGlobal(64);
        IntPtr outBuf = Marshal.AllocHGlobal(outLen);
        try
        {
            // KSP_PIN { KSPROPERTY {Set,Id,Flags}, PinId, Reserved }
            byte[] g = KSPSETID_PIN.ToByteArray();
            Marshal.Copy(g, 0, inBuf, 16);
            Marshal.WriteInt32(inBuf, 16, (int)propId);
            Marshal.WriteInt32(inBuf, 20, (int)KSPROPERTY_TYPE_GET);
            Marshal.WriteInt32(inBuf, 24, (int)pinId);
            Marshal.WriteInt32(inBuf, 28, 0);
            for (int i = 0; i < outLen; i++) Marshal.WriteByte(outBuf, i, 0);

            uint ret;
            bool ok = DeviceIoControl(h, IOCTL_KS_PROPERTY, inBuf, 32, outBuf, (uint)outLen, out ret, IntPtr.Zero);
            if (!ok) return "IOCTLFAIL err=" + Marshal.GetLastWin32Error();

            switch (propId)
            {
                case PIN_CTYPES:
                    return "count=" + Marshal.ReadInt32(outBuf) + " (ret=" + ret + ")";
                case PIN_CINSTANCES:
                    return "possible=" + Marshal.ReadInt32(outBuf) + " current=" + Marshal.ReadInt32(outBuf, 4) + " (ret=" + ret + ")";
                case 5: // PIN_INTERFACES：KSMULTIPLE_ITEM{Size,Count}
                case 6: // PIN_MEDIUMS
                    {
                        if (ret == 0) return "count=0 (ret=0)";
                        int cnt = ret >= 8 ? Marshal.ReadInt32(outBuf, 4) : 0;
                        int sz = ret >= 8 ? Marshal.ReadInt32(outBuf, 0) : 0;
                        StringBuilder sb = new StringBuilder();
                        sb.Append("count=").Append(cnt).Append(" size=").Append(sz);
                        // KSPIN_INTERFACE / KSPIN_MEDIUM = {GUID Set; ULONG Id; ULONG Flags} = 24 字节
                        for (int i = 0; i < cnt && 8 + i * 24 + 24 <= ret; i++)
                        {
                            byte[] gi = new byte[16];
                            Marshal.Copy(IntPtr.Add(outBuf, 8 + i * 24), gi, 0, 16);
                            sb.Append(" [").Append(i).Append("]=")
                              .Append(new Guid(gi).ToString("B"))
                              .Append(":id=0x").Append(Marshal.ReadInt32(outBuf, 8 + i * 24 + 16).ToString("X8"));
                        }
                        sb.Append(" (ret=").Append(ret).Append(')');
                        return sb.ToString();
                    }
                case PIN_DATAFLOW:
                    // ks.h: KSPIN_DATAFLOW_IN = 1, KSPIN_DATAFLOW_OUT = 2
                    int df = Marshal.ReadInt32(outBuf);
                    return (df == 1 ? "IN(1)" : df == 2 ? "OUT(2)" : "?" + df) + " (ret=" + ret + ")";
                case PIN_DATARANGES:
                    {
                        // 直接 dump 原始字节（KS 返回的 DATARANGES 缓冲区布局含前导字段，
                        // 逐字节 dump 后与可用驱动对照最可靠）
                        StringBuilder sb = new StringBuilder();
                        sb.Append("ret=").Append(ret).Append(" :");
                        int show = Math.Min((int)ret, 104);
                        for (int i = 0; i < show; i++)
                        {
                            if (i % 16 == 0) sb.Append(" | ");
                            sb.Append(Marshal.ReadByte(outBuf, i).ToString("x2")).Append(' ');
                        }
                        return sb.ToString();
                    }
                case PIN_COMMUNICATION:
                    int c = Marshal.ReadInt32(outBuf);
                    return (c == 0 ? "NONE" : c == 1 ? "SINK" : c == 2 ? "SOURCE" : "BRIDGE/" + c) + " (ret=" + ret + ")";
                case PIN_CATEGORY:
                    byte[] cat = new byte[16];
                    Marshal.Copy(outBuf, cat, 0, 16);
                    return new Guid(cat).ToString("B") + " (ret=" + ret + ")";
                case PIN_PHYSICALCONNECTION:
                    // KSPIN_PHYSICALCONNECTION { ULONG Pin; WCHAR SymbolicLinkName[1]; }
                    // —— 符号链接名是**内联**宽字符串（不是 UNICODE_STRING 指针）
                    int pinCount = Marshal.ReadInt32(outBuf);
                    string sym = ret > 4 ? Marshal.PtrToStringUni(IntPtr.Add(outBuf, 4)) : "(none)";
                    return "pin=" + pinCount + " link=" + (sym ?? "(null)") + " (ret=" + ret + ")";
                default:
                    return "ret=" + ret;
            }
        }
        finally
        {
            Marshal.FreeHGlobal(inBuf); Marshal.FreeHGlobal(outBuf); CloseHandle(h);
        }
    }

    /// 读 vdev 自定义诊断属性集（{7f4e2a11-9c3b-4b6e-8f2a-1d2c3b4a5e60},0）= 15 个 u32 计数器
    public static string VdevStats(string path)
    {
        IntPtr h = CreateFileW(path, GENERIC_READ | GENERIC_WRITE, 3, IntPtr.Zero, OPEN_EXISTING, 0, IntPtr.Zero);
        if (h.ToInt64() == -1)
            h = CreateFileW(path, GENERIC_READ, 3, IntPtr.Zero, OPEN_EXISTING, 0, IntPtr.Zero);
        if (h.ToInt64() == -1) return "OPENFAIL err=" + Marshal.GetLastWin32Error();
        IntPtr inBuf = Marshal.AllocHGlobal(64);
        IntPtr outBuf = Marshal.AllocHGlobal(512);
        try
        {
            Marshal.Copy(new Guid("7f4e2a11-9c3b-4b6e-8f2a-1d2c3b4a5e60").ToByteArray(), 0, inBuf, 16);
            Marshal.WriteInt32(inBuf, 16, 0);
            Marshal.WriteInt32(inBuf, 20, (int)KSPROPERTY_TYPE_GET);
            for (int i = 0; i < 512; i++) Marshal.WriteByte(outBuf, i, 0);
            uint ret;
            bool ok = DeviceIoControl(h, IOCTL_KS_PROPERTY, inBuf, 24, outBuf, 512, out ret, IntPtr.Zero);
            if (!ok) return "IOCTLFAIL err=" + Marshal.GetLastWin32Error();
            string[] names = { "init", "new_stream", "set_format", "set_format_ok", "last_tag(hex)",
                               "last_ch", "last_rate", "last_bits", "last_size", "last_status(hex)",
                               "range", "range_ok", "prop_set", "prop_get", "prop_last_tag",
                               "rng_major(hex)", "rng_sub(hex)", "rng_spec(hex)", "rng_fsize",
                               "rng_outlen", "rng_status(hex)",
                               "qi_cnt", "qi_ok", "qi0(hex)", "qi1(hex)", "qi2(hex)", "qi3(hex)",
                               "qi4(hex)", "qi5(hex)", "qi6(hex)", "qi7(hex)", "range_ok_seen",
                               "prop_set_ok", "ae_get_mix_fmt", "ae_get_dev_fmt",
                               "ae_set_dev_fmt", "ae_set_status(hex)", "ae_stored_tag(hex)",
                               "ae_stored_rate", "ae_stored_bits", "ae_fail_idx", "ae_fail_status(hex)",
                               "prop_get_size", "prop_attr_len",
                                "ae_fmt_type", "ae_fmt_size_out", "ae_set_buf_size",
                                "modes_calls", "modes_last_pin", "modes_last_count",
                                "prop_last_rate", "prop_last_bits", "prop_reject",
                                "ae_desc", "ae_gfx_get", "ae_gfx_set", "ae_fmt_size",
                               "ae_mix_dup", "ae_dev_dup", "ae_set_dup", "ae_supported",
                               "ae_chcount", "ae_steppings", "ae_vol_get", "ae_vol_set",
                               "ae_mute_get", "ae_mute_set", "ae_peak", "ae_bufrange" };
            StringBuilder sb = new StringBuilder();
            sb.Append("ret=").Append(ret).Append(" :");
            for (int i = 0; i < names.Length; i++)
            {
                int v = Marshal.ReadInt32(outBuf, i * 4);
                sb.Append(' ').Append(names[i]).Append('=');
                if (names[i].EndsWith("(hex)")) sb.Append("0x").Append(v.ToString("X8"));
                else sb.Append(v);
            }
            return sb.ToString();
        }
        finally { Marshal.FreeHGlobal(inBuf); Marshal.FreeHGlobal(outBuf); CloseHandle(h); }
    }

    /// 手工发一次 KSPROPERTY_PIN_DATAINTERSECTION(4)：
    /// 输入 = KSP_PIN(32) + 实例 KSMULTIPLE_ITEM(8) + 1 条 KSDATARANGE_AUDIO(84, PCM/16bit/48k/2ch)
    /// 输出 = outLen 字节；返回错误码或输出头 24 字节
    public static string DataIntersection(string path, uint pinId, int outLen)
    {
        IntPtr h = CreateFileW(path, GENERIC_READ | GENERIC_WRITE, 3, IntPtr.Zero, OPEN_EXISTING, 0, IntPtr.Zero);
        if (h.ToInt64() == -1)
            h = CreateFileW(path, GENERIC_READ, 3, IntPtr.Zero, OPEN_EXISTING, 0, IntPtr.Zero);
        if (h.ToInt64() == -1) return "OPENFAIL err=" + Marshal.GetLastWin32Error();
        const int RANGE_SIZE = 84;
        const int IN_LEN = 32 + 8 + RANGE_SIZE;
        IntPtr inBuf = Marshal.AllocHGlobal(IN_LEN);
        int cap = Math.Max(outLen, 1);
        IntPtr outBuf = Marshal.AllocHGlobal(cap);
        try
        {
            for (int i = 0; i < IN_LEN; i++) Marshal.WriteByte(inBuf, i, 0);
            Marshal.Copy(KSPSETID_PIN.ToByteArray(), 0, inBuf, 16);
            Marshal.WriteInt32(inBuf, 16, 4);                       // KSPROPERTY_PIN_DATAINTERSECTION
            Marshal.WriteInt32(inBuf, 20, (int)KSPROPERTY_TYPE_GET);
            Marshal.WriteInt32(inBuf, 24, (int)pinId);
            Marshal.WriteInt32(inBuf, 32, 8 + RANGE_SIZE);           // KSMULTIPLE_ITEM.Size
            Marshal.WriteInt32(inBuf, 36, 1);                        // KSMULTIPLE_ITEM.Count
            int r = 40;
            Marshal.WriteInt32(inBuf, r + 0, RANGE_SIZE);            // KSDATARANGE.FormatSize
            Marshal.Copy(new Guid("73647561-0000-0010-8000-00AA00389B71").ToByteArray(), 0, IntPtr.Add(inBuf, r + 16), 16);
            Marshal.Copy(new Guid("00000001-0000-0010-8000-00AA00389B71").ToByteArray(), 0, IntPtr.Add(inBuf, r + 32), 16);
            Marshal.Copy(new Guid("05589F81-C356-11CE-BF01-00AA0055595A").ToByteArray(), 0, IntPtr.Add(inBuf, r + 48), 16);
            Marshal.WriteInt32(inBuf, r + 64, 2);
            Marshal.WriteInt32(inBuf, r + 68, 16);
            Marshal.WriteInt32(inBuf, r + 72, 16);
            Marshal.WriteInt32(inBuf, r + 76, 48000);
            Marshal.WriteInt32(inBuf, r + 80, 48000);
            for (int i = 0; i < cap; i++) Marshal.WriteByte(outBuf, i, 0);
            uint ret;
            bool ok = DeviceIoControl(h, IOCTL_KS_PROPERTY, inBuf, IN_LEN, outBuf, (uint)cap, out ret, IntPtr.Zero);
            if (!ok) return "FAIL err=" + Marshal.GetLastWin32Error();
            StringBuilder sb = new StringBuilder();
            sb.Append("OK ret=").Append(ret).Append(" :");
            int show = Math.Min((int)ret, 24);
            for (int i = 0; i < show; i++) sb.Append(' ').Append(Marshal.ReadByte(outBuf, i).ToString("x2"));
            return sb.ToString();
        }
        finally { Marshal.FreeHGlobal(inBuf); Marshal.FreeHGlobal(outBuf); CloseHandle(h); }
    }

    /// 解码 KSPROPERTY_TOPOLOGY_NODES 的 KSTOPOLOGY 头（x64：计数在 0/16/32，指针在 8/24/40）
    public static string Topology(string path)
    {
        IntPtr h = CreateFileW(path, GENERIC_READ | GENERIC_WRITE, 3, IntPtr.Zero, OPEN_EXISTING, 0, IntPtr.Zero);
        if (h.ToInt64() == -1)
            h = CreateFileW(path, GENERIC_READ, 3, IntPtr.Zero, OPEN_EXISTING, 0, IntPtr.Zero);
        if (h.ToInt64() == -1) return "OPENFAIL err=" + Marshal.GetLastWin32Error();
        IntPtr inBuf = Marshal.AllocHGlobal(64);
        IntPtr outBuf = Marshal.AllocHGlobal(512);
        try
        {
            Marshal.Copy(new Guid("720D4AC0-7533-11D0-A5D6-28DB04C10000").ToByteArray(), 0, inBuf, 16);
            Marshal.WriteInt32(inBuf, 16, 1);                        // KSPROPERTY_TOPOLOGY_NODES
            Marshal.WriteInt32(inBuf, 20, (int)KSPROPERTY_TYPE_GET);
            for (int i = 0; i < 512; i++) Marshal.WriteByte(outBuf, i, 0);
            uint ret;
            bool ok = DeviceIoControl(h, IOCTL_KS_PROPERTY, inBuf, 24, outBuf, 512, out ret, IntPtr.Zero);
            if (!ok) return "IOCTLFAIL err=" + Marshal.GetLastWin32Error();
            int cats = Marshal.ReadInt32(outBuf, 0);
            int nodes = Marshal.ReadInt32(outBuf, 16);
            int conns = Marshal.ReadInt32(outBuf, 32);
            IntPtr nodesPtr = Marshal.ReadIntPtr(outBuf, 24);
            IntPtr connsPtr = Marshal.ReadIntPtr(outBuf, 40);
            StringBuilder sb = new StringBuilder();
            sb.Append("cats=").Append(cats).Append(" nodes=").Append(nodes).Append(" conns=").Append(conns);
            for (int i = 0; i < nodes && nodesPtr != IntPtr.Zero; i++)
            {
                byte[] g = new byte[16];
                Marshal.Copy(IntPtr.Add(nodesPtr, i * 16), g, 0, 16);
                sb.Append(" node[").Append(i).Append("]=").Append(new Guid(g).ToString("B"));
            }
            for (int i = 0; i < conns && connsPtr != IntPtr.Zero; i++)
            {
                int from = Marshal.ReadInt32(connsPtr, i * 16);
                int fromPin = Marshal.ReadInt32(connsPtr, i * 16 + 4);
                int to = Marshal.ReadInt32(connsPtr, i * 16 + 8);
                int toPin = Marshal.ReadInt32(connsPtr, i * 16 + 12);
                sb.Append(" conn[").Append(i).Append("]=").Append(from).Append(':').Append(fromPin)
                  .Append("->").Append(to).Append(':').Append(toPin);
            }
            return sb.ToString();
        }
        finally { Marshal.FreeHGlobal(inBuf); Marshal.FreeHGlobal(outBuf); CloseHandle(h); }
    }
}
'@ -ErrorAction SilentlyContinue

$cat = @{ AUDIO = '{6994ad04-93ef-11d0-a3cc-00a0c9223196}'; RENDER = '{65e8773e-8f56-11d0-a3b9-00a0c9223196}' }
$devices = @(
    @{ Name = 'vdev   WaveRender-0';   Path = "\\?\ROOT#MEDIA#$VdevInstance#$($cat.AUDIO)\WaveRender-0" },
    @{ Name = 'vdev   WaveCapture-0';  Path = "\\?\ROOT#MEDIA#$VdevInstance#$($cat.AUDIO)\WaveCapture-0" },
    @{ Name = 'vdev   TopoRender-0';   Path = "\\?\ROOT#MEDIA#$VdevInstance#$($cat.AUDIO)\TopologyRender-0" },
    @{ Name = 'vdev   TopoCapture-0';  Path = "\\?\ROOT#MEDIA#$VdevInstance#$($cat.AUDIO)\TopologyCapture-0" },
    @{ Name = "$RefName WaveRender-0"; Path = "\\?\ROOT#MEDIA#$RefInstance#$($cat.AUDIO)\WaveRender-0" },
    @{ Name = "$RefName WaveCapture-0";Path = "\\?\ROOT#MEDIA#$RefInstance#$($cat.AUDIO)\WaveCapture-0" },
    @{ Name = "$RefName TopoRender-0"; Path = "\\?\ROOT#MEDIA#$RefInstance#$($cat.AUDIO)\TopologyRender-0" },
    @{ Name = "$RefName TopoCapture-0";Path = "\\?\ROOT#MEDIA#$RefInstance#$($cat.AUDIO)\TopologyCapture-0" }
)

foreach ($d in $devices) {
    Write-Output ("===== {0}" -f $d.Name)
    if ($d.Name -match 'vdev') {
        Write-Output ("  VDEV_STATS      : {0}" -f [KsProbe]::VdevStats($d.Path))
    }
    Write-Output ("  TOPO_CATEGORIES : {0}" -f [KsProbe]::FilterProperty($d.Path, '720D4AC0-7533-11D0-A5D6-28DB04C10000', 0, 512))
    Write-Output ("  TOPO_NODES      : {0}" -f [KsProbe]::FilterProperty($d.Path, '720D4AC0-7533-11D0-A5D6-28DB04C10000', 1, 512))
    Write-Output ("  TOPOLOGY(解码)  : {0}" -f [KsProbe]::Topology($d.Path))
    if ($d.Name -match 'Wave') {
        Write-Output ("  TOPO_NODES raw  : {0}" -f [KsProbe]::FilterProperty($d.Path, '720D4AC0-7533-11D0-A5D6-28DB04C10000', 1, 64))
    }
    Write-Output ("  TOPO_CONNECTIONS: {0}" -f [KsProbe]::FilterProperty($d.Path, '720D4AC0-7533-11D0-A5D6-28DB04C10000', 2, 512))
    $ct = [KsProbe]::Query($d.Path, 0, 1, 32)
    Write-Output ("  PIN_CTYPES       : {0}" -f $ct)
    if ($ct -like 'count=*') {
        $n = [int](($ct -split ' ')[0] -replace 'count=', '')
        for ($i = 0; $i -lt $n; $i++) {
            $ci = [KsProbe]::Query($d.Path, [uint32]$i, 0, 32)
            $df = [KsProbe]::Query($d.Path, [uint32]$i, 2, 32)
            $dr = [KsProbe]::Query($d.Path, [uint32]$i, 3, 2048)
            $cm = [KsProbe]::Query($d.Path, [uint32]$i, 7, 32)
            $cg = [KsProbe]::Query($d.Path, [uint32]$i, 11, 32)
            $ifc = [KsProbe]::Query($d.Path, [uint32]$i, 5, 256)
            $med = [KsProbe]::Query($d.Path, [uint32]$i, 6, 256)
            $nm = [KsProbe]::Query($d.Path, [uint32]$i, 12, 32)
            $pc = [KsProbe]::Query($d.Path, [uint32]$i, 10, 512)
            $pf = [KsProbe]::Query($d.Path, [uint32]$i, 14, 256)
            $pf2 = [KsProbe]::Query($d.Path, [uint32]$i, 15, 256)
            # KSPROPERTY_PIN_DATAINTERSECTION(4)：先按"0 长度缓冲"探测（引擎就是这么问的），再给足缓冲
            $di0 = [KsProbe]::Query($d.Path, [uint32]$i, 4, 0)
            $di256 = [KsProbe]::Query($d.Path, [uint32]$i, 4, 256)
            Write-Output ("  pin{0}: CINSTANCES={1}  DATAFLOW={2}  COMM={3}" -f $i, $ci, $df, $cm)
            Write-Output ("        CATEGORY={0}" -f $cg)
            Write-Output ("        INTERFACES={0}" -f $ifc)
            Write-Output ("        MEDIUMS   ={0}" -f $med)
            Write-Output ("        PIN_NAME  ={0}" -f $nm)
            Write-Output ("        DATARANGES={0}" -f $dr)
            Write-Output ("        PHYSICALCONNECTION: {0}" -f $pc)
            Write-Output ("        PROPOSEDATAFORMAT : {0}" -f $pf)
            Write-Output ("        PROPOSEDATAFORMAT2: {0}" -f $pf2)
            Write-Output ("        DATAINTERSECTION(len=0)  : {0}" -f $di0)
            Write-Output ("        DATAINTERSECTION(len=256): {0}" -f $di256)
            Write-Output ("        DATAINTERSECTION(手工实例, len=0)  : {0}" -f [KsProbe]::DataIntersection($d.Path, [uint32]$i, 0))
            Write-Output ("        DATAINTERSECTION(手工实例, len=256): {0}" -f [KsProbe]::DataIntersection($d.Path, [uint32]$i, 256))
        }
    }
}
