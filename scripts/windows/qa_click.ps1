<#
Real mouse input and UI Automation for what the QA channel cannot do on Windows
(it has only synthetic DOM events there): hover-driven UI, native dialogs.

  . .\qa_click.ps1
  Click-Screen 800 400            # absolute screen pixels (left button)
  Click-Screen 800 400 -Double
  Invoke-UiaButton 'Open'         # press a button of any visible window by name
#>
Add-Type @"
using System; using System.Runtime.InteropServices;
public static class QaMouse {
  [DllImport("user32.dll")] public static extern bool SetCursorPos(int x, int y);
  [DllImport("user32.dll")] public static extern void mouse_event(uint f, uint dx, uint dy, uint d, UIntPtr e);
}
"@
function Click-Screen([int]$X, [int]$Y, [switch]$Double) {
    [QaMouse]::SetCursorPos($X, $Y) | Out-Null
    Start-Sleep -Milliseconds 50
    foreach ($i in 1..($(if ($Double) { 2 } else { 1 }))) {
        [QaMouse]::mouse_event(0x2, 0, 0, 0, [UIntPtr]::Zero)   # left down
        [QaMouse]::mouse_event(0x4, 0, 0, 0, [UIntPtr]::Zero)   # left up
        Start-Sleep -Milliseconds 60
    }
}
function Invoke-UiaButton([string]$Name) {
    Add-Type -AssemblyName UIAutomationClient, UIAutomationTypes
    $cond = New-Object Windows.Automation.PropertyCondition([Windows.Automation.AutomationElement]::NameProperty, $Name)
    $el = [Windows.Automation.AutomationElement]::RootElement.FindFirst([Windows.Automation.TreeScope]::Descendants, $cond)
    if (-not $el) { throw "no UI element named '$Name'" }
    $el.GetCurrentPattern([Windows.Automation.InvokePattern]::Pattern).Invoke()
}
