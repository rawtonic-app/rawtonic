; Rawtonic installer hooks (Tauri NSIS, bundle.windows.nsis.installerHooks).
;
; This file is included before the template's pages, so the "Page custom" below
; becomes the installer's first page: the privacy notice (SignPath condition,
; docs/CODE_SIGNING.md). It also moves the install-folder key that older
; installers wrote under the publisher "rawtonic" (the identifier's second
; part, Tauri's default) to the key of the publisher "Rawtonic Project"
; (bundle.publisher; keep the names below in sync with it), because the
; template reads that key when it replaces an installed copy (_?= folder).
; The ASCII-only English text matches the English-only installer; passive and
; silent installs show no page. The marker PrivacyNoticeShown tells the app the
; notice was shown (src-tauri/src/privacy_notice.rs).
!include nsDialogs.nsh
!include FileFunc.nsh

; Tauri's .onInit picks the install folder (the default one, then the folder of
; the new publisher key) before any page runs, so an earlier installation in a
; custom folder is found here, by its legacy key; /D= still wins.
InstallDirRegKey HKCU "Software\rawtonic\Rawtonic" ""

Page custom RawtonicPrivacyPre RawtonicPrivacyLeave

Function RawtonicMoveLegacyKey
  ReadRegStr $0 HKCU "Software\Rawtonic Project\Rawtonic" ""
  ReadRegStr $1 HKCU "Software\rawtonic\Rawtonic" ""
  ${If} $0 == ""
  ${AndIf} $1 != ""
    WriteRegStr HKCU "Software\Rawtonic Project\Rawtonic" "" $1
  ${EndIf}
  DeleteRegKey HKCU "Software\rawtonic\Rawtonic"
  DeleteRegKey /ifempty HKCU "Software\rawtonic"
FunctionEnd

Function RawtonicPrivacyPre
  Call RawtonicMoveLegacyKey
  ClearErrors
  ${GetOptions} $CMDLINE "/P" $0
  ${IfNot} ${Errors}
    Abort
  ${EndIf}
  ClearErrors
  !insertmacro MUI_HEADER_TEXT "Privacy" "What Rawtonic sends over the internet"
  nsDialogs::Create 1018
  Pop $0
  ${NSD_CreateLabel} 0 0 100% 100u "Rawtonic sends nothing on its own except one update check. At startup, at most once every 24 hours, it asks GitHub's public Releases API whether a newer version exists. GitHub receives your IP address and the version number; no photos, paths or identifiers are sent.$\r$\n$\r$\nThe check is on by default. To turn it off, clear $\"Check for updates at startup$\" in Preferences > General.$\r$\n$\r$\nEverything else that goes online happens only when you ask for it."
  Pop $0
  ${NSD_CreateLink} 0 106u 100% 12u "https://rawtonic.cc/privacy/"
  Pop $0
  ${NSD_OnClick} $0 RawtonicPrivacyOpenLink
  nsDialogs::Show
FunctionEnd

Function RawtonicPrivacyOpenLink
  ExecShell "open" "https://rawtonic.cc/privacy/"
FunctionEnd

Function RawtonicPrivacyLeave
  WriteRegDWORD HKCU "Software\Rawtonic Project\Rawtonic" "PrivacyNoticeShown" 1
FunctionEnd

; After an install or update, fill the app's GPU pipeline cache so the first
; launch does not wait for shader compiles (src-tauri/src/gpu_warmup.rs).
; The installer runs as the installing user (installMode currentUser, not
; elevated), so the cache lands in that user's %LOCALAPPDATA%. The app ends
; the warm-up itself within 120 s; its outcome never fails the install.
!macro NSIS_HOOK_POSTINSTALL
  ; A silent install shows no page, so the legacy key is cleaned here too.
  Call RawtonicMoveLegacyKey
  DetailPrint "Preparing the GPU shaders for the first start..."
  ClearErrors
  ExecWait '"$INSTDIR\${MAINBINARYNAME}.exe" --warm-gpu-cache' $0
  ${If} ${Errors}
    DetailPrint "GPU shader warm-up skipped: it could not be started."
  ${Else}
    DetailPrint "GPU shader warm-up ended (exit code $0)."
  ${EndIf}
  ClearErrors
!macroend
