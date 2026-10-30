; Perk per-user installer (no admin rights needed).
; Built inside the dev container: docker compose run --rm dev package-windows
; Defines passed by scripts/dev.sh: VERSION, EXE, ICON, OUTFILE.

Unicode true
SetCompressor /SOLID lzma

!include "MUI2.nsh"

Name "Perk"
OutFile "${OUTFILE}"
InstallDir "$LOCALAPPDATA\Programs\Perk"
InstallDirRegKey HKCU "Software\Perk" "InstallDir"
RequestExecutionLevel user

VIProductVersion "${VERSION}.0"
VIAddVersionKey "ProductName" "Perk"
VIAddVersionKey "FileDescription" "Perk installer"
VIAddVersionKey "FileVersion" "${VERSION}"
VIAddVersionKey "ProductVersion" "${VERSION}"
VIAddVersionKey "LegalCopyright" "Perk contributors"

!define UNINST_KEY "Software\Microsoft\Windows\CurrentVersion\Uninstall\Perk"

!define MUI_ICON "${ICON}"
!define MUI_UNICON "${ICON}"
!define MUI_FINISHPAGE_RUN "$INSTDIR\perk.exe"
!define MUI_FINISHPAGE_RUN_TEXT "Start Perk now"

!insertmacro MUI_PAGE_DIRECTORY
!insertmacro MUI_PAGE_INSTFILES
!insertmacro MUI_PAGE_FINISH
!insertmacro MUI_UNPAGE_CONFIRM
!insertmacro MUI_UNPAGE_INSTFILES
!insertmacro MUI_LANGUAGE "English"

Section "Install"
  ; Stop a running copy so the executable can be replaced.
  nsExec::Exec 'taskkill /IM perk.exe /F'
  SetOutPath "$INSTDIR"
  File "/oname=perk.exe" "${EXE}"
  WriteUninstaller "$INSTDIR\uninstall.exe"

  CreateShortcut "$SMPROGRAMS\Perk.lnk" "$INSTDIR\perk.exe"

  WriteRegStr HKCU "Software\Perk" "InstallDir" "$INSTDIR"
  WriteRegStr HKCU "${UNINST_KEY}" "DisplayName" "Perk"
  WriteRegStr HKCU "${UNINST_KEY}" "DisplayVersion" "${VERSION}"
  WriteRegStr HKCU "${UNINST_KEY}" "Publisher" "Perk contributors"
  WriteRegStr HKCU "${UNINST_KEY}" "DisplayIcon" "$INSTDIR\perk.exe"
  WriteRegStr HKCU "${UNINST_KEY}" "UninstallString" '"$INSTDIR\uninstall.exe"'
  WriteRegDWORD HKCU "${UNINST_KEY}" "NoModify" 1
  WriteRegDWORD HKCU "${UNINST_KEY}" "NoRepair" 1
SectionEnd

Section "Uninstall"
  nsExec::Exec 'taskkill /IM perk.exe /F'
  ; Remove the launch-at-startup entry Perk may have created.
  DeleteRegValue HKCU "Software\Microsoft\Windows\CurrentVersion\Run" "Perk"
  Delete "$SMPROGRAMS\Perk.lnk"
  Delete "$INSTDIR\perk.exe"
  Delete "$INSTDIR\uninstall.exe"
  RMDir "$INSTDIR"
  DeleteRegKey HKCU "${UNINST_KEY}"
  DeleteRegKey HKCU "Software\Perk"
SectionEnd
