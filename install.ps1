#requires -Version 5.1
<#
.SYNOPSIS
    MatrixOS Installer for Windows.

.DESCRIPTION
    Creates and configures the MatrixOS virtual machine in Oracle VirtualBox.

      1. Locates VBoxManage.exe (the VirtualBox CLI).
      2. Removes any pre-existing "MatrixOS" VM (keeps the ISO file intact).
      3. Creates a new x86_64 VM tuned for bare-metal LLM inference:
           - 6 CPU cores
           - 8 GB RAM
           - 32 MB video memory, 1024x768 framebuffer
           - PS/2 keyboard, no audio, no network
      4. Attaches matrixos.iso as the bootable IDE optical device.
      5. Creates a Start-menu + Desktop shortcut that launches the VM.

.NOTES
    Run from an unrestricted PowerShell (Set-ExecutionPolicy -Scope Process Bypass).
#>

[CmdletBinding()]
param(
    [string]$IsoPath  = "$PSScriptRoot\matrixos.iso",
    [string]$VmName   = "MatrixOS",
    [int]   $Cpus     = 6,
    [int]   $RamMB    = 8192,
    [int]   $VRamMB   = 32,
    [switch]$NoShortcut,
    [switch]$LaunchAfter
)

$ErrorActionPreference = "Stop"

function Write-Step($msg)  { Write-Host "[+] $msg" -ForegroundColor Cyan }
function Write-Ok($msg)    { Write-Host "[OK] $msg"  -ForegroundColor Green }
function Write-Warn2($msg) { Write-Host "[!] $msg"   -ForegroundColor Yellow }
function Write-Err($msg)   { Write-Host "[X] $msg"   -ForegroundColor Red }

# ── 1. Locate VBoxManage ──────────────────────────────────────────────────
Write-Step "Locating VirtualBox..."
$VBoxPaths = @(
    "$env:ProgramFiles\Oracle\VirtualBox\VBoxManage.exe",
    "${env:ProgramFiles(x86)}\Oracle\VirtualBox\VBoxManage.exe"
)
$VBox = $VBoxPaths | Where-Object { Test-Path $_ } | Select-Object -First 1
if (-not $VBox) {
    Write-Err "VirtualBox not found. Please install from https://www.virtualbox.org/"
    exit 1
}
$VBoxVer = & $VBox --version
Write-Ok "VirtualBox $VBoxVer at $VBox"

# ── 2. Verify ISO ─────────────────────────────────────────────────────────
Write-Step "Verifying MatrixOS ISO..."
if (-not (Test-Path $IsoPath)) {
    Write-Err "matrixos.iso not found at: $IsoPath"
    exit 1
}
$IsoSize = (Get-Item $IsoPath).Length
$IsoGb   = [math]::Round($IsoSize / 1GB, 2)
Write-Ok "ISO found ($IsoGb GB)"

# ── 3. Remove existing VM if present ──────────────────────────────────────
Write-Step "Checking for existing $VmName VM..."
$existing = & $VBox list vms 2>$null | Select-String "^`"$VmName`""
if ($existing) {
    Write-Warn2 "Existing $VmName VM found — unregistering and deleting"
    try { & $VBox controlvm $VmName poweroff 2>$null | Out-Null } catch {}
    Start-Sleep -Milliseconds 700
    & $VBox unregistervm $VmName --delete 2>$null | Out-Null
    Write-Ok "Old VM removed"
} else {
    Write-Ok "No previous installation found"
}

# ── 4. Create the new VM ──────────────────────────────────────────────────
Write-Step "Creating VM ($Cpus cores, $RamMB MB RAM)..."
& $VBox createvm --name $VmName --ostype "Other_64" --register | Out-Null

& $VBox modifyvm $VmName `
    --memory          $RamMB `
    --cpus            $Cpus `
    --vram            $VRamMB `
    --graphicscontroller vmsvga `
    --firmware        efi64 `
    --chipset         ich9 `
    --pae             on `
    --hpet            on `
    --x2apic          on `
    --nested-hw-virt  off `
    --boot1           dvd `
    --boot2           none `
    --boot3           none `
    --boot4           none `
    --audio-driver    none `
    --nic1            none `
    --usbohci         off `
    --usbehci         off `
    --mouse           usbtablet `
    --keyboard        ps2 `
    | Out-Null

# Try to expose the host CPU profile so AVX is available. Falls back silently.
try {
    & $VBox modifyvm $VmName --cpu-profile "host" 2>$null | Out-Null
} catch {}

Write-Ok "VM created"

# ── 5. Attach the ISO as bootable optical drive ───────────────────────────
Write-Step "Attaching ISO..."
& $VBox storagectl $VmName --name "IDE" --add ide --controller PIIX4 | Out-Null
& $VBox storageattach $VmName `
    --storagectl "IDE" `
    --port 0 --device 0 `
    --type dvddrive `
    --medium $IsoPath | Out-Null
Write-Ok "ISO attached"

# ── 6. Set framebuffer + viewport size ────────────────────────────────────
& $VBox setextradata $VmName "VBoxInternal2/EfiGraphicsResolution" "1024x768" | Out-Null
& $VBox setextradata $VmName "GUI/LastGuestSizeHint" "1024,768"                 | Out-Null

# ── 7. Optional Start-menu + Desktop shortcut ─────────────────────────────
if (-not $NoShortcut) {
    Write-Step "Creating shortcut..."
    $launcher = & $VBox --help 2>$null | Out-Null  # noop to keep ws clean
    $VBoxExe  = Split-Path $VBox -Parent
    $VBoxExe  = Join-Path  $VBoxExe "VirtualBoxVM.exe"

    $desktop  = [Environment]::GetFolderPath("Desktop")
    $startMnu = [Environment]::GetFolderPath("StartMenu")
    $shortcuts = @(
        (Join-Path $desktop  "MatrixOS.lnk"),
        (Join-Path $startMnu "Programs\MatrixOS.lnk")
    )

    $wsh = New-Object -ComObject WScript.Shell
    foreach ($s in $shortcuts) {
        $dir = Split-Path $s -Parent
        if (-not (Test-Path $dir)) { New-Item -ItemType Directory -Force -Path $dir | Out-Null }
        $lnk = $wsh.CreateShortcut($s)
        $lnk.TargetPath       = $VBoxExe
        $lnk.Arguments        = "--startvm `"$VmName`" --fullscreen"
        $lnk.WorkingDirectory = Split-Path $VBoxExe -Parent
        $lnk.IconLocation     = "$VBoxExe,0"
        $lnk.Description      = "Launch MatrixOS bare-metal LLM"
        $lnk.Save()
    }
    Write-Ok "Shortcuts created (Desktop + Start menu)"
}

# ── 8. Summary ────────────────────────────────────────────────────────────
Write-Host ""
Write-Host "═══════════════════════════════════════" -ForegroundColor Green
Write-Host "  MatrixOS installed successfully!     " -ForegroundColor Green
Write-Host "═══════════════════════════════════════" -ForegroundColor Green
Write-Host ""
Write-Host "  VM name:  $VmName"
Write-Host "  CPUs:     $Cpus"
Write-Host "  RAM:      $RamMB MB"
Write-Host "  ISO:      $IsoPath ($IsoGb GB)"
Write-Host ""
Write-Host "  Start it from your Desktop, or run:"
Write-Host "    `"$VBoxExe`" --startvm `"$VmName`""
Write-Host ""

if ($LaunchAfter) {
    Write-Step "Launching MatrixOS..."
    Start-Process -FilePath $VBoxExe -ArgumentList "--startvm","$VmName"
}
