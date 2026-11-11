<#
Client for the QA control channel (src-tauri/src/qa_control.rs) in plain
PowerShell, for a Windows host without Python. The channel listens on
127.0.0.1 only, so this runs on the same machine as the app.

  $env:RAWTONIC_QA_PORT = 7979; Start-Process .\rawtonic-app.exe 'C:\path\photo.srw'
  . .\qa_control.ps1
  Connect-Qa 7979
  Invoke-Qa ping
  Invoke-Qa state.wait @{ path = 'editor.hasRenderedFirstFrame'; timeout_ms = 60000 }
  Invoke-Qa ui.type @{ name = 'Exposure'; text = '1' }
  Invoke-Qa ui.key  @{ key = 'Enter' }

Native input and window capture are macOS-only; here use ui.click / ui.drag /
ui.type (synthetic DOM events) and, for what the channel cannot do, qa_click.ps1.
#>
$script:QaClient = $null
$script:QaReader = $null
$script:QaWriter = $null
$script:QaId = 0

function Connect-Qa([int]$Port = 7979) {
    $script:QaClient = New-Object Net.Sockets.TcpClient('127.0.0.1', $Port)
    $stream = $script:QaClient.GetStream()
    $utf8 = New-Object Text.UTF8Encoding($false)
    $script:QaReader = New-Object IO.StreamReader($stream, $utf8)
    $script:QaWriter = New-Object IO.StreamWriter($stream, $utf8)
    $script:QaWriter.NewLine = "`n"
    $script:QaWriter.AutoFlush = $true
}

function Invoke-Qa([string]$Method, $Params = @{}) {
    $script:QaId++
    $line = @{ id = $script:QaId; method = $Method; params = $Params } | ConvertTo-Json -Depth 20 -Compress
    $script:QaWriter.WriteLine($line)
    $reply = $script:QaReader.ReadLine() | ConvertFrom-Json
    if ($null -ne $reply.error) { throw "${Method}: $($reply.error)" }
    $reply.result
}

# Block until the channel answers (the app needs a few seconds to start).
function Wait-Qa([int]$Port = 7979, [int]$TimeoutSec = 90) {
    $end = (Get-Date).AddSeconds($TimeoutSec)
    while ((Get-Date) -lt $end) {
        try { Connect-Qa $Port; Invoke-Qa ping | Out-Null; return } catch { Start-Sleep -Milliseconds 500 }
    }
    throw "QA channel on port $Port did not answer in $TimeoutSec s"
}

# state.wait / ui.wait are answered by the webview within 30 s at most (the
# native side gives up then), so a longer wait polls in slices of 20 s. A busy webview
# (first start, shader compile) can miss even that: "no webview reply" is retried.
function Wait-QaState([string]$Path, $Equals = $null, [int]$TimeoutSec = 120) {
    $end = (Get-Date).AddSeconds($TimeoutSec)
    do {
        $p = @{ path = $Path; timeout_ms = 20000 }
        if ($null -ne $Equals) { $p.equals = $Equals }
        try { return Invoke-Qa state.wait $p } catch { if ($_ -notmatch 'timed out|no webview reply') { throw } }
    } while ((Get-Date) -lt $end)
    throw "state $Path did not become ready in $TimeoutSec s"
}
