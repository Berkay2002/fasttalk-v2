!include "LogicLib.nsh"

!macro NSIS_HOOK_POSTINSTALL
  SetRegView 64
  ReadRegDWord $0 HKLM "SOFTWARE\Microsoft\VisualStudio\14.0\VC\Runtimes\x64" "Installed"
  ${If} $0 != 1
    DetailPrint "Installing the Microsoft Visual C++ x64 runtime"
    ExecWait '"$INSTDIR\prerequisites\vc_redist.x64.exe" /install /quiet /norestart' $1
    ${If} $1 != 0
    ${AndIf} $1 != 1638
    ${AndIf} $1 != 3010
      MessageBox MB_ICONSTOP|MB_OK "The Microsoft Visual C++ runtime failed to install (exit code $1)."
      Abort
    ${EndIf}
  ${Else}
    DetailPrint "Microsoft Visual C++ x64 runtime is already installed"
  ${EndIf}
  SetRegView 32
!macroend
