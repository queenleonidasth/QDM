Unicode true
!include "MUI2.nsh"
!include "x64.nsh"
!ifndef QDM_VERSION
  !error "QDM_VERSION must be supplied by the verified packaging script"
!endif
!ifndef QDM_BINARY
  !error "QDM_BINARY must be supplied by the verified packaging script"
!endif
!ifndef QDM_SETUP
  !error "QDM_SETUP must be supplied by the verified packaging script"
!endif
Name "QDM — QUEEN Download Manager"
OutFile "${QDM_SETUP}"
InstallDir "$LOCALAPPDATA\Programs\QDM"
InstallDirRegKey HKCU "Software\QUEEN\QDM" "InstallLocation"
RequestExecutionLevel user
SetCompressor /SOLID lzma
SetCompressorDictSize 16
ShowInstDetails show
ShowUninstDetails show
VIProductVersion "${QDM_VERSION}.0"
VIAddVersionKey "ProductName" "QDM — QUEEN Download Manager"
VIAddVersionKey "FileDescription" "QDM native Windows installer"
VIAddVersionKey "FileVersion" "${QDM_VERSION}"
VIAddVersionKey "ProductVersion" "${QDM_VERSION}"
VIAddVersionKey "LegalCopyright" "QUEEN"
!define MUI_ICON "..\resources\qdm.ico"
!define MUI_UNICON "..\resources\qdm.ico"
!define MUI_ABORTWARNING
!insertmacro MUI_PAGE_WELCOME
!insertmacro MUI_PAGE_DIRECTORY
!insertmacro MUI_PAGE_INSTFILES
!define MUI_FINISHPAGE_RUN "$INSTDIR\QDM.exe"
!insertmacro MUI_PAGE_FINISH
!insertmacro MUI_UNPAGE_CONFIRM
!insertmacro MUI_UNPAGE_INSTFILES
!insertmacro MUI_LANGUAGE "English"

Function .onInit
  ${IfNot} ${RunningX64}
    MessageBox MB_ICONSTOP "QDM requires 64-bit Windows."
    Abort
  ${EndIf}
FunctionEnd

Section "QDM" SEC_MAIN
  SetShellVarContext current
qdm_check:
  IfFileExists "$INSTDIR\QDM.exe" 0 qdm_install
  ClearErrors
  FileOpen $0 "$INSTDIR\QDM.exe" a
  IfErrors qdm_running
  FileClose $0
  Goto qdm_install
qdm_running:
  IfSilent qdm_abort
  MessageBox MB_RETRYCANCEL|MB_ICONEXCLAMATION "Please choose Exit in QDM, then retry the installation." IDRETRY qdm_check
qdm_abort:
  SetErrorLevel 2
  Abort
qdm_install:
  SetOutPath "$INSTDIR"
  File "${QDM_BINARY}"
  File "..\README.md"
  SetOutPath "$INSTDIR\extensions"
  File /r "..\extensions\*.*"
  SetOutPath "$INSTDIR\third-party"
  File /r "..\docs\third-party\*.*"
  SetOutPath "$INSTDIR"
  WriteUninstaller "$INSTDIR\Uninstall QDM.exe"
  CreateDirectory "$SMPROGRAMS\QDM"
  CreateShortCut "$DESKTOP\QDM — QUEEN Download Manager.lnk" "$INSTDIR\QDM.exe" "" "$INSTDIR\QDM.exe" 0
  CreateShortCut "$SMPROGRAMS\QDM\QDM.lnk" "$INSTDIR\QDM.exe" "" "$INSTDIR\QDM.exe" 0
  CreateShortCut "$SMPROGRAMS\QDM\Uninstall QDM.lnk" "$INSTDIR\Uninstall QDM.exe"
  WriteRegStr HKCU "Software\QUEEN\QDM" "InstallLocation" "$INSTDIR"
  WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\QDM.Native" "DisplayName" "QDM — QUEEN Download Manager"
  WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\QDM.Native" "DisplayVersion" "${QDM_VERSION}"
  WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\QDM.Native" "Publisher" "QUEEN"
  WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\QDM.Native" "InstallLocation" "$INSTDIR"
  WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\QDM.Native" "DisplayIcon" "$INSTDIR\QDM.exe"
  WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\QDM.Native" "UninstallString" '$\"$INSTDIR\Uninstall QDM.exe$\"'
  WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\QDM.Native" "QuietUninstallString" '$\"$INSTDIR\Uninstall QDM.exe$\" /S'
  WriteRegDWORD HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\QDM.Native" "NoModify" 1
  WriteRegDWORD HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\QDM.Native" "NoRepair" 1
SectionEnd

Section "Uninstall"
  SetShellVarContext current
  Delete "$INSTDIR\QDM.exe"
  IfErrors 0 +3
    MessageBox MB_ICONSTOP "Please exit QDM before uninstalling."
    Abort
  Delete "$INSTDIR\README.md"
  RMDir /r "$INSTDIR\extensions"
  RMDir /r "$INSTDIR\third-party"
  Delete "$DESKTOP\QDM — QUEEN Download Manager.lnk"
  Delete "$SMPROGRAMS\QDM\QDM.lnk"
  Delete "$SMPROGRAMS\QDM\Uninstall QDM.lnk"
  RMDir "$SMPROGRAMS\QDM"
  DeleteRegKey HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\QDM.Native"
  DeleteRegKey HKCU "Software\QUEEN\QDM"
  Delete "$INSTDIR\Uninstall QDM.exe"
  RMDir "$INSTDIR"
SectionEnd
