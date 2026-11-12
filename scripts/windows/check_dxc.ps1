# SPDX-License-Identifier: AGPL-3.0-or-later
# Checks the DirectX Shader Compiler a Windows build ships (gap G76):
# dxcompiler.dll and dxil.dll beside ONNX Runtime's WebGPU plugin in
# resources\, each exactly Microsoft's release file pinned in
# src-tauri\windows\dxc.sha256 (the same pins src-tauri/build.rs uses), no
# other copy anywhere under -Root, and both licence texts in
# resources\licenses\.
#
#   pwsh scripts/windows/check_dxc.ps1 -Root <unpacked app folder> [-Arch x64|arm64]
#
# Exit 1 on any failure.
param(
  [Parameter(Mandatory = $true)][string]$Root,
  [ValidateSet('x64', 'arm64')][string]$Arch = 'x64'
)
$ErrorActionPreference = 'Stop'
$Root = (Resolve-Path $Root).Path.TrimEnd('\')
$pins = Join-Path $PSScriptRoot '..\..\src-tauri\windows\dxc.sha256'

$want = @{}
$pin = "^([0-9a-f]{64})  bin/$Arch/(\S+)$"
foreach ($line in Get-Content $pins) {
  if ($line -match $pin) { $want[$Matches[2]] = $Matches[1] }
}
if ($want.Count -ne 2 -or -not $want['dxcompiler.dll'] -or -not $want['dxil.dll']) {
  Write-Error "dxc.sha256 does not pin dxcompiler.dll and dxil.dll for $Arch"; exit 1
}

$resources = Join-Path $Root 'resources'
$failed = $false
if (-not (Test-Path (Join-Path $resources 'onnxruntime_providers_webgpu.dll'))) {
  "MISSING resources\onnxruntime_providers_webgpu.dll"
  $failed = $true
}
foreach ($name in 'dxil.dll', 'dxcompiler.dll') {
  $file = Join-Path $resources $name
  if (-not (Test-Path $file)) { "MISSING resources\$name"; $failed = $true; continue }
  $hash = (Get-FileHash $file -Algorithm SHA256).Hash.ToLower()
  $version = (Get-Item $file).VersionInfo.FileVersion
  if ($hash -ne $want[$name]) {
    "WRONG resources\$name $version sha256 $hash, pinned $($want[$name])"
    $failed = $true
  } else {
    "ok resources\$name $version sha256 $hash"
  }
}
# One copy only: another one elsewhere could be the one a lookup finds.
$copies = @(Get-ChildItem $Root -Recurse -File -Include dxcompiler.dll, dxil.dll |
    Where-Object { $_.DirectoryName -ne $resources })
foreach ($c in $copies) { "EXTRA $($c.FullName.Substring($Root.Length))"; $failed = $true }
foreach ($license in 'DirectXShaderCompiler-NCSA.txt', 'DirectXShaderCompiler-dxil-Microsoft.txt') {
  if (-not (Test-Path (Join-Path $resources "licenses\$license"))) { "MISSING resources\licenses\$license"; $failed = $true }
}
if ($failed) { Write-Error 'the bundled DirectX Shader Compiler is missing, extra or not the pinned release'; exit 1 }
'bundled DirectX Shader Compiler matches the pinned release'
