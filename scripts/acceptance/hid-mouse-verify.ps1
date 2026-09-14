# 鼠标注入实弹验证：开一个置顶 WinForms 观察窗，注入 click / wheel / move，
# 记录 MouseDown、MouseWheel 事件与 GetCursorPos 前后位移（相对移动 ±127、滚轮 120 的倍数）。
#
# 用法（普通用户即可）：
#   powershell -ExecutionPolicy Bypass -File scripts\acceptance\hid-mouse-verify.ps1
param(
    [string]$RepoRoot,
    [string]$HidCli,
    [string]$OutDir
)
$here = $PSScriptRoot
if (-not $RepoRoot) { $RepoRoot = Split-Path -Parent (Split-Path -Parent $here) }
if (-not $OutDir) { $OutDir = Join-Path $env:TEMP 'vdev-acceptance' }
if (-not $HidCli) {
    $HidCli = Join-Path $RepoRoot 'crates\vdev-hid-win\target\x86_64-pc-windows-msvc\release\vdev-hid-win.exe'
}
New-Item -ItemType Directory -Force -Path $OutDir | Out-Null
if (-not (Test-Path $HidCli)) {
    throw "找不到 vdev-hid-win.exe：$HidCli`n先构建：cd crates\vdev-hid-win; cargo build --release"
}

Add-Type -AssemblyName System.Windows.Forms, System.Drawing
Add-Type -Namespace Win2 -Name Api -MemberDefinition @'
[DllImport("user32.dll")] public static extern bool SetCursorPos(int x, int y);
[DllImport("user32.dll")] public static extern bool GetCursorPos(out System.Drawing.Point p);
'@ -ReferencedAssemblies System.Drawing, System.Windows.Forms

$exe = $HidCli
$log = Join-Path $OutDir 'mouse-events.txt'
if (Test-Path $log) { [System.IO.File]::Delete($log) }

$form = New-Object System.Windows.Forms.Form
$form.Text = "vdev 鼠标注入观察窗"
$form.StartPosition = "Manual"
$form.Location = New-Object System.Drawing.Point(600, 400)
$form.Size = New-Object System.Drawing.Size(520, 380)
$form.TopMost = $true
$form.Add_MouseDown({ param($s, $e) Add-Content -LiteralPath $log "MouseDown $($e.Button) at $($e.X),$($e.Y)" })
$form.Add_MouseWheel({ param($s, $e) Add-Content -LiteralPath $log "MouseWheel delta=$($e.Delta)" })

$step = 0
$timer = New-Object System.Windows.Forms.Timer
$timer.Interval = 1200
$timer.Add_Tick({
    $script:step++
    switch ($script:step) {
        1 {
            # 用窗口自己的屏幕坐标定位（免疫 DPI 缩放与多屏偏移）
            $c = $form.PointToScreen((New-Object System.Drawing.Point([int]($form.ClientSize.Width/2), [int]($form.ClientSize.Height/2))))
            Add-Content -LiteralPath $log ("form center on screen = {0},{1}" -f $c.X, $c.Y)
            [void][Win2.Api]::SetCursorPos($c.X, $c.Y)
            $form.Activate()
            Start-Sleep -Milliseconds 300
            & $exe kernel mouse click 2>&1 | Out-Null
            Add-Content -LiteralPath $log "cmd: kernel mouse click -> exit $LASTEXITCODE"
        }
        2 {
            & $exe kernel mouse wheel 120 2>&1 | Out-Null
            Add-Content -LiteralPath $log "cmd: kernel mouse wheel 120 -> exit $LASTEXITCODE"
        }
        3 {
            $p = New-Object System.Drawing.Point
            [void][Win2.Api]::GetCursorPos([ref]$p)
            Add-Content -LiteralPath $log ("cursor now = {0},{1}" -f $p.X, $p.Y)
            & $exe kernel mouse move 20 0 2>&1 | Out-Null
            Start-Sleep -Milliseconds 300
            $p2 = New-Object System.Drawing.Point
            [void][Win2.Api]::GetCursorPos([ref]$p2)
            Add-Content -LiteralPath $log ("after kernel mouse move 20 0 -> {0},{1} (dx={2}) exit={3}" -f $p2.X, $p2.Y, ($p2.X - $p.X), $LASTEXITCODE)
            $timer.Stop(); $form.Close()
        }
    }
})
$timer.Start()
[System.Windows.Forms.Application]::Run($form)

Write-Output "观察窗记录："
if (Test-Path $log) { Get-Content -LiteralPath $log | ForEach-Object { "  $_" } } else { Write-Output "  (无事件)" }

# 判定：点击/滚轮要有事件、相对移动要有位移。任何一项缺了就红——避免"CLI 打印成功"当通过。
$text = if (Test-Path $log) { Get-Content -LiteralPath $log -Raw } else { '' }
$missing = @()
if ($text -notmatch 'MouseDown') { $missing += '左键点击（MouseDown）' }
if ($text -notmatch 'MouseWheel delta=120') { $missing += '滚轮（MouseWheel delta=120）' }
if ($text -notmatch '\(dx=(?!0\))') { $missing += '相对移动（dx≠0）' }
if ($missing) {
    Write-Output ("❌ 未观察到：" + ($missing -join '、'))
    exit 1
}
Write-Output '✅ 点击 / 滚轮 / 相对移动 三项均有实测记录'
exit 0
