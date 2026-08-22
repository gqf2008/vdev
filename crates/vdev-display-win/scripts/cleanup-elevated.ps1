$ErrorActionPreference = "Continue"
$log = "$env:TEMP\vdev-cleanup.log"
Remove-Item $log -ErrorAction SilentlyContinue
Start-Transcript -Path $log -Force
$base = "HKLM:\SYSTEM\CurrentControlSet\Enum\ROOT\DISPLAY"
Get-ChildItem $base -ErrorAction SilentlyContinue | ForEach-Object {
    $id = $_.PSChildName
    $hw = (Get-ItemProperty $_.PSPath -ErrorAction SilentlyContinue).HardwareID
    if ($hw -match "vdev-display") {
        Write-Host "removing $id"
        pnputil /remove-device "ROOT\DISPLAY\$id" 2>&1 | Out-String
    }
}
Stop-Transcript
