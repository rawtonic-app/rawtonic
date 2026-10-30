; Rawtonic installer hooks (Tauri NSIS, bundle.windows.nsis.installerHooks).

; After an install or update, fill the app's GPU pipeline cache so the first
; launch does not wait for shader compiles (src-tauri/src/gpu_warmup.rs).
; The installer runs as the installing user (installMode currentUser, not
; elevated), so the cache lands in that user's %LOCALAPPDATA%. The app ends
; the warm-up itself within 120 s; its outcome never fails the install.
!macro NSIS_HOOK_POSTINSTALL
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
