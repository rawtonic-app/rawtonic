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

; The install runs no GPU warm-up: on a Radeon 680M the MSI's took over two
; minutes behind a busy cursor (notes: parity-gaps/G76). The app's first start
; compiles the pipelines and shows that it does.
!macro NSIS_HOOK_POSTINSTALL
  ; A silent install shows no page, so the legacy key is cleaned here too.
  Call RawtonicMoveLegacyKey
!macroend
