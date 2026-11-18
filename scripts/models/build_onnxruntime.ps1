# Build ONNX Runtime 1.30.0 for Windows x64 without Microsoft's usage
# telemetry for the model pack (PRIVACY.md); the Windows counterpart of
# build_onnxruntime.sh (same source archive, same SHA-256).
#
# Microsoft's Windows library registers the ETW TraceLogging provider
# "Microsoft.ML.ONNXRuntime" {3a26b1ff-7484-7484-7484-15261f42614d} in
# Windows' telemetry provider group, so the Connected User Experiences and
# Telemetry service collects its events. On Windows `--no_telemetry` only
# leaves out the group; the provider is still compiled and registered. The
# patch beside this script (onnxruntime-1.30.0-windows-no-etw-telemetry.patch)
# drops the provider, its registration and its events when telemetry is off;
# ORT's own diagnostic ETW logging provider ("ONNXRuntimeTraceLoggingProvider",
# in no provider group, silent unless a local trace session enables it) stays.
# Everything else follows Microsoft's Windows x64 packaging job (win-ci.yml:
# RelWithDebInfo, `--use_binskim_compliant_compile_flags --enable_lto
# --disable_rtti --build_shared_lib`, Visual Studio 17 2022), less Java, the
# ONNX test runner, unit tests and vcpkg (deps.txt pins the same versions).
#
# Usage: powershell -ExecutionPolicy Bypass -File scripts\models\build_onnxruntime.ps1 SOURCE_ARCHIVE BUILD_DIR [JOBS]
#   SOURCE_ARCHIVE  as in build_onnxruntime.sh (GitHub's archive of SOURCE_COMMIT)
#   BUILD_DIR       a new, empty directory; keep its path short (e.g. C:\o\b),
#                   MSBuild hits Windows' 260-character path limit otherwise
#   JOBS            build.py --parallel (default 3); MSBuild runs up to
#                   JOBS x JOBS compilers, about 1 GB each
#   -NoSpectre      test builds only: compile without /Qspectre when Visual
#                   Studio lacks the Spectre-mitigated libraries (the library
#                   is then named ...-nospectre-test-..., not a release
#                   candidate)
#
# Needs Python 3.10+ (PYTHON, default python), CMake 3.28+ on PATH, Visual
# Studio 2022 (Build Tools) with the C++ workload and the component
# Microsoft.VisualStudio.Component.VC.Runtimes.x86.x64.Spectre ("MSVC C++
# x64/x86 Spectre-mitigated libs"; adding it to an installation needs
# administrator rights once: the script prints the command), git (applies the patch) and
# tar (Windows' own); no administrator rights. Downloads the third-party
# sources pinned in the archive's cmake/deps.txt. Prints the library's SHA-256
# and checks it holds neither the telemetry provider nor an uploader.
param(
  [Parameter(Mandatory = $true)][string]$Archive,
  [Parameter(Mandatory = $true)][string]$Build,
  [int]$Jobs = 3,
  [switch]$NoSpectre
)
$ErrorActionPreference = 'Stop'

$SourceCommit = 'f2c39fe2f838cf35ce7da92824f5a5e3ee6e88a7'
$SourceSha256 = '1b4ec55fa26ee8fe622ea256894dfe9381bdfa99d41489994a1961720f556c48'
$Patch = Join-Path $PSScriptRoot 'onnxruntime-1.30.0-windows-no-etw-telemetry.patch'
$Python = if ($env:PYTHON) { $env:PYTHON } else { 'python' }

function Fail([string]$message) { Write-Error "build_onnxruntime: $message"; exit 1 }
function Run([string]$exe, [string[]]$arguments) {
  & $exe @arguments
  if ($LASTEXITCODE -ne 0) { Fail "$exe exited with $LASTEXITCODE" }
}

# Microsoft's flags compile with /Qspectre and link the Spectre-mitigated CRT.
$vswhere = "${env:ProgramFiles(x86)}\Microsoft Visual Studio\Installer\vswhere.exe"
$vs = & $vswhere -latest -products * -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -property installationPath
if (-not $vs) { Fail 'Visual Studio 2022 with the C++ x64 tools not found' }
$msvc = (Get-Content "$vs\VC\Auxiliary\Build\Microsoft.VCToolsVersion.default.txt").Trim()
if (-not $NoSpectre -and -not (Test-Path "$vs\VC\Tools\MSVC\$msvc\lib\spectre\x64")) {
  Fail ("Visual Studio lacks the Spectre-mitigated libraries; add them (administrator):`n" +
    "  & `"${env:ProgramFiles(x86)}\Microsoft Visual Studio\Installer\setup.exe`" modify --installPath `"$vs`" " +
    "--add Microsoft.VisualStudio.Component.VC.Runtimes.x86.x64.Spectre --passive --norestart")
}

$got = (Get-FileHash -Algorithm SHA256 $Archive).Hash.ToLower()
if ($got -ne $SourceSha256) { Fail "$Archive has SHA-256 $got, expected $SourceSha256" }
if ((Test-Path $Build) -and (Get-ChildItem -Force $Build | Select-Object -First 1)) { Fail "$Build is not empty" }
New-Item -ItemType Directory -Force $Build | Out-Null
$Build = (Resolve-Path $Build).Path
Run "$env:SystemRoot\System32\tar.exe" @('-xzf', (Resolve-Path $Archive).Path, '-C', $Build)
$src = Join-Path $Build "onnxruntime-$SourceCommit"
if ((Get-Content -Raw "$src\VERSION_NUMBER").Trim() -ne '1.30.0') { Fail 'not 1.30.0' }
Run git @('-C', $src, 'apply', '--whitespace=nowarn', $Patch)
if ($NoSpectre) {
  $buildPy = "$src\tools\ci_build\build.py"
  $text = [IO.File]::ReadAllText($buildPy)
  $flag = '                    cflags += ["/Qspectre"]'
  if (-not $text.Contains($flag)) { Fail 'build.py: /Qspectre line not found' }
  [IO.File]::WriteAllText($buildPy, $text.Replace($flag, '                    pass  # -NoSpectre test build'))
  Write-Warning 'test build without /Qspectre: not a release candidate'
}

# Warnings are not errors: without the provider the telemetry events' inputs
# are unused (as in the macOS build, newer toolchain warnings too).
Run $Python @("$src\tools\ci_build\build.py",
  '--update', '--build', '--build_dir', "$Build\out", '--skip_submodule_sync', '--parallel', "$Jobs",
  '--config', 'RelWithDebInfo', '--use_binskim_compliant_compile_flags', '--enable_lto', '--disable_rtti',
  '--build_shared_lib', '--cmake_generator', 'Visual Studio 17 2022',
  '--no_telemetry', '--skip_tests', '--compile_no_warning_as_error',
  '--cmake_extra_defines', 'onnxruntime_BUILD_UNIT_TESTS=OFF')

$name = if ($NoSpectre) { 'onnxruntime-1.30.0-notelemetry-nospectre-test-windows-x86_64.dll' } else { 'onnxruntime-1.30.0-notelemetry-windows-x86_64.dll' }
Copy-Item "$Build\out\RelWithDebInfo\RelWithDebInfo\onnxruntime.dll" "$Build\$name"

# Neither the telemetry provider (its name, or its GUID as TraceLogging stores
# it: little-endian bytes) nor a 1DS uploader or collector address.
$bytes = [IO.File]::ReadAllBytes("$Build\$name")
$latin1 = [Text.Encoding]::GetEncoding(28591).GetString($bytes)
$guid = [Text.Encoding]::GetEncoding(28591).GetString(([guid]'3a26b1ff-7484-7484-7484-15261f42614d').ToByteArray())
foreach ($needle in @('Microsoft.ML.ONNXRuntime', $guid, 'OneCollector', 'events.data.microsoft', 'MATSDK')) {
  if ($latin1.Contains($needle)) { Fail "$name still contains the telemetry provider or uploader" }
}
$sum = (Get-FileHash -Algorithm SHA256 "$Build\$name").Hash.ToLower()
Write-Output "$sum  $Build\$name"
