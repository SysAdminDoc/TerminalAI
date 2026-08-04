; NSIS installer hooks for TerminalAI.
;
; Tauri's own CheckIfAppIsRunning stops ${MAINBINARYNAME}.exe and nothing else. This app ships
; two sidecars, and terminalai-daemon.exe is deliberately designed to outlive the window it was
; started from — so on every upgrade over an existing install the daemon is still running, still
; holding its named pipe, and still holding an open image section on its own executable. NSIS
; then fails partway through the Install section trying to overwrite a locked file, which is the
; path every existing user takes.
;
; The reboot fallback is not available here: /REBOOTOK applies to Delete and RMDir only, and the
; MOVEFILE_DELAY_UNTIL_REBOOT it maps to needs Administrators while this installer's mode is
; currentUser. So the sidecars have to be stopped, not deferred.
;
; NSIS_HOOK_PREINSTALL is expanded at the top of Section Install, before the first `File` write,
; and NSIS_HOOK_PREUNINSTALL before the uninstaller deletes anything — an uninstall over a
; running daemon leaves the same locked files behind.

; Stop one sidecar, then confirm it is gone and try once more if it is not.
;
; No labels and no ${__LINE__} defines: this macro is inserted more than once from inside another
; macro, where every insertion would expand on the same source line and collide.
!macro TerminalAIStopSidecar executableName
  nsis_tauri_utils::FindProcessCurrentUser "${executableName}"
  Pop $R0
  ${If} $R0 = 0
    DetailPrint "Stopping ${executableName}"
    nsis_tauri_utils::KillProcessCurrentUser "${executableName}"
    Pop $R0
    Sleep 500
    nsis_tauri_utils::FindProcessCurrentUser "${executableName}"
    Pop $R0
    ${If} $R0 = 0
      nsis_tauri_utils::KillProcessCurrentUser "${executableName}"
      Pop $R0
      Sleep 1000
      nsis_tauri_utils::FindProcessCurrentUser "${executableName}"
      Pop $R0
      ${If} $R0 = 0
        DetailPrint "${executableName} is still running; the file write below may fail"
      ${EndIf}
    ${EndIf}
  ${EndIf}
!macroend

!macro TerminalAIStopSidecars
  ; Ask the daemon to shut itself down first so its session store is flushed rather than losing
  ; whatever accumulated since the last write. Both the connect and the call are bounded at five
  ; seconds inside the probe, so ExecWait cannot hang the installer; a missing probe, a daemon
  ; that is not running, and a refused request are all fine — the kills below are the backstop.
  ${If} ${FileExists} "$INSTDIR\terminalai-probe.exe"
    DetailPrint "Asking the TerminalAI daemon to shut down"
    ExecWait '"$INSTDIR\terminalai-probe.exe" shutdown' $R1
    Sleep 500
  ${EndIf}

  !insertmacro TerminalAIStopSidecar "terminalai-daemon.exe"
  !insertmacro TerminalAIStopSidecar "terminalai-probe.exe"
!macroend

!macro NSIS_HOOK_PREINSTALL
  !insertmacro TerminalAIStopSidecars
!macroend

!macro NSIS_HOOK_PREUNINSTALL
  !insertmacro TerminalAIStopSidecars
!macroend
