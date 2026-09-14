# 键鼠注入端到端验证（需要鼠标能动、会抢前台）：
#   1) 记事本实弹打字 a/b/c + Ctrl down/up + Enter，用 WM_GETTEXT 读回文本；
#   2) 相对移动：GetCursorPos 前后对比 dx；
#   3) 置顶观察窗记录 click / wheel 事件。
# 注意：运行期间会占用前台窗口，别在录音/演示时跑。
#
# 用法：powershell -ExecutionPolicy Bypass -File scripts\acceptance\hid-injection-verify.ps1
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
Add-Type -Namespace Win -Name Api -MemberDefinition @'
[DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr hWnd);
[DllImport("user32.dll", CharSet=CharSet.Unicode)] public static extern IntPtr FindWindowEx(IntPtr parent, IntPtr child, string cls, string title);
[DllImport("user32.dll", CharSet=CharSet.Unicode)] public static extern int SendMessage(IntPtr hWnd, int msg, int wParam, System.Text.StringBuilder lParam);
[DllImport("user32.dll")] public static extern bool GetCursorPos(out System.Drawing.Point p);
[DllImport("user32.dll")] public static extern bool SetCursorPos(int x, int y);
'@ -ReferencedAssemblies System.Drawing, System.Windows.Forms

$exe = $HidCli
$log = Join-Path $OutDir 'injection-events.txt'
if (Test-Path $log) { [System.IO.File]::Delete($log) }
$WM_GETTEXT = 0x000D

Write-Output "############ 1) 键盘：Notepad 实弹打字"
$np = Start-Process notepad -PassThru
Start-Sleep -Seconds 2
$np.Refresh()
[void][Win.Api]::SetForegroundWindow($np.MainWindowHandle)
Start-Sleep -Milliseconds 800

function Get-NotepadText {
    $edit = [Win.Api]::FindWindowEx($np.MainWindowHandle, [IntPtr]::Zero, "Edit", $null)
    if ($edit -eq [IntPtr]::Zero) { return "<no Edit>" }
    $sb = New-Object System.Text.StringBuilder 256
    [void][Win.Api]::SendMessage($edit, $WM_GETTEXT, 256, $sb)
    return $sb.ToString()
}

Write-Output ("注入前文本: [{0}]" -f (Get-NotepadText))
foreach ($k in @("a", "b", "c")) {
    $out = & $exe kernel key $k 2>&1 | Out-String
    Write-Output ("  kernel key {0} -> {1}" -f $k, $out.Trim())
    Start-Sleep -Milliseconds 400
}
Write-Output ("注入后文本: [{0}]" -f (Get-NotepadText))

Write-Output "  -- 修饰键 down/up（Ctrl）"
& $exe kernel key ctrl --action down 2>&1 | Out-String | ForEach-Object { $_.Trim() }
& $exe kernel key ctrl --action up 2>&1 | Out-String | ForEach-Object { $_.Trim() }
Start-Sleep -Milliseconds 300

Write-Output "  -- Enter（应多一个换行）"
& $exe kernel key enter 2>&1 | Out-String | ForEach-Object { $_.Trim() }
Start-Sleep -Milliseconds 400
$t = Get-NotepadText
Write-Output ("  Enter 后文本长度={0} 内容=[{1}]" -f $t.Length, ($t -replace "`r`n", "<CRLF>"))
$np.CloseMainWindow() | Out-Null
Start-Sleep -Milliseconds 500
if (-not $np.HasExited) { $np.Kill() }

Write-Output "############ 2) 鼠标：相对移动（GetCursorPos 前后对比）"
$p0 = New-Object System.Drawing.Point
[void][Win.Api]::GetCursorPos([ref]$p0)
$out = & $exe kernel mouse move 20 0 2>&1 | Out-String
Start-Sleep -Milliseconds 400
$p1 = New-Object System.Drawing.Point
[void][Win.Api]::GetCursorPos([ref]$p1)
Write-Output ("  {0} 光标 {1},{2} -> {3},{4}   dx={5}" -f $out.Trim(), $p0.X, $p0.Y, $p1.X, $p1.Y, ($p1.X - $p0.X))

Write-Output "############ 3) 鼠标：点击 / 滚轮（观察窗口记录事件）"
$form = New-Object System.Windows.Forms.Form
$form.Text = "vdev injection observer"
$form.StartPosition = "Manual"
$form.Location = New-Object System.Drawing.Point(400, 300)
$form.Size = New-Object System.Drawing.Size(420, 320)
$form.TopMost = $true
$form.Add_MouseDown({ param($s, $e) Add-Content -LiteralPath $log "MouseDown $($e.Button) at $($e.X),$($e.Y)" })
$form.Add_MouseWheel({ param($s, $e) Add-Content -LiteralPath $log "MouseWheel delta=$($e.Delta)" })
$step = 0
$timer = New-Object System.Windows.Forms.Timer
$timer.Interval = 900
$timer.Add_Tick({
    $script:step++
    switch ($script:step) {
       1 {
            # 用窗口自己的屏幕坐标定位（免疫 DPI 缩放与多屏偏移）——直接写死 610,460
            # 在 150% 缩放下会点偏，这正是历史上的坑。
            $c = $form.PointToScreen((New-Object System.Drawing.Point([int]($form.ClientSize.Width/2), [int]($form.ClientSize.Height/2))))
            Add-Content -LiteralPath $log ("form center on screen = {0},{1}" -f $c.X, $c.Y)
            [void][Win.Api]::SetCursorPos($c.X, $c.Y)
            $form.Activate()
            Start-Sleep -Milliseconds 200
            & $exe kernel mouse click 2>&1 | Out-Null
            Add-Content -LiteralPath $log "cmd: kernel mouse click -> exit $LASTEXITCODE"
        }
        2 {
            & $exe kernel mouse wheel 120 2>&1 | Out-Null
            Add-Content -LiteralPath $log "cmd: kernel mouse wheel 120 -> exit $LASTEXITCODE"
        }
        3 { $timer.Stop(); $form.Close() }
    }
})
$timer.Start()
[System.Windows.Forms.Application]::Run($form)
Write-Output "  观察窗口记录："
if (Test-Path $log) { Get-Content -LiteralPath $log | ForEach-Object { "    $_" } } else { Write-Output "    (无事件)" }
