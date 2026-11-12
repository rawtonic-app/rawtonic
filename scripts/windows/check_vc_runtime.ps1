# SPDX-License-Identifier: AGPL-3.0-or-later
# Checks the Visual C++ runtime a Windows build ships (gap G76): every
# executable or DLL under -Root that imports the VC++ runtime dynamically
# (MSVCP140*, VCRUNTIME140*, CONCRT140) must find each imported runtime DLL
# bundled in its own folder, and every bundled runtime DLL must be at least
# as new (major.minor) as the toolset that built this build and the linker
# of every importer. An older runtime is unsupported by Microsoft and since
# VS 2022 17.10 (14.40) crashes in std::mutex (MSVCP140.dll access
# violation with Windows' own 14.24 copy).
#
#   pwsh scripts/windows/check_vc_runtime.ps1 -Root <unpacked app folder>
#
# Needs Visual Studio's dumpbin (vswhere). Exit 1 on any failure.
param([Parameter(Mandatory = $true)][string]$Root)
$ErrorActionPreference = 'Stop'
$Root = (Resolve-Path $Root).Path.TrimEnd('\')

$vswhere = "${env:ProgramFiles(x86)}\Microsoft Visual Studio\Installer\vswhere.exe"
$vs = & $vswhere -latest -products * -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -property installationPath
$dumpbin = & $vswhere -latest -products * -find 'VC\Tools\MSVC\**\bin\Hostx64\x64\dumpbin.exe' | Select-Object -First 1
if (-not $vs -or -not $dumpbin) { Write-Error 'no Visual Studio with C++ tools (dumpbin)'; exit 1 }
$tools = (Get-Content (Join-Path $vs 'VC\Auxiliary\Build\Microsoft.VCToolsVersion.default.txt') -Raw).Trim()
if ($tools -notmatch '^(\d+)\.(\d+)') { Write-Error "unreadable toolset version '$tools'"; exit 1 }
$need = [version]"$($Matches[1]).$($Matches[2])"
"build toolset $tools"

$runtimeName = '^(?i)(msvcp140[a-z0-9_]*|vcruntime140[a-z0-9_]*|concrt140)\.dll$'
$failed = $false
$importers = 0
foreach ($file in Get-ChildItem $Root -Recurse -File -Include *.dll, *.exe) {
  if ($file.Name -match $runtimeName) { continue }
  $deps = & $dumpbin /nologo /dependents $file.FullName |
    ForEach-Object { $_.Trim() } | Where-Object { $_ -match $runtimeName }
  if (-not $deps) { continue }
  $importers++
  $headers = & $dumpbin /nologo /headers $file.FullName
  $linker = $headers | Select-String '^\s*(\d+)\.(\d+) linker version' | Select-Object -First 1
  if (-not $linker) { Write-Error "no linker version in $($file.Name)"; exit 1 }
  $linkerVersion = [version]"$($linker.Matches[0].Groups[1].Value).$($linker.Matches[0].Groups[2].Value)"
  $want = if ($linkerVersion -gt $need) { $linkerVersion } else { $need }
  "$($file.FullName.Substring($Root.Length)) (linker $linkerVersion) imports $($deps -join ', ')"
  foreach ($dep in $deps) {
    $bundled = Join-Path $file.DirectoryName $dep
    if (-not (Test-Path $bundled)) {
      "  MISSING $dep beside $($file.Name)"
      $failed = $true
      continue
    }
    $info = (Get-Item $bundled).VersionInfo
    $have = [version]"$($info.FileMajorPart).$($info.FileMinorPart)"
    $full = "$($info.FileMajorPart).$($info.FileMinorPart).$($info.FileBuildPart).$($info.FilePrivatePart)"
    if ($have -lt $want) {
      "  TOO OLD $dep $full < $want"
      $failed = $true
    } else {
      "  ok $dep $full >= $want"
    }
  }
}
if ($importers -eq 0) { Write-Error "nothing under $Root imports the VC++ runtime: the check would prove nothing"; exit 1 }
if ($failed) { Write-Error 'the bundled VC++ runtime is missing or older than the build needs'; exit 1 }
'bundled VC++ runtime covers every import and is new enough'
