Unicode true
!include "MUI2.nsh"

Name "Manycraft"
OutFile "Manycraft-Setup-${VERSION}-windows-x64.exe"
InstallDir "$LOCALAPPDATA\Programs\Manycraft"
RequestExecutionLevel user
SetCompressor /SOLID lzma

!define MUI_ICON "..\icons\manycraft.ico"
!define MUI_UNICON "..\icons\manycraft.ico"
!insertmacro MUI_PAGE_DIRECTORY
!insertmacro MUI_PAGE_INSTFILES
!define MUI_FINISHPAGE_RUN "$INSTDIR\manycraft.exe"
!insertmacro MUI_PAGE_FINISH
!insertmacro MUI_UNPAGE_CONFIRM
!insertmacro MUI_UNPAGE_INSTFILES
!insertmacro MUI_LANGUAGE "German"
!insertmacro MUI_LANGUAGE "English"

Section "Install"
  SetOutPath "$INSTDIR"
  File "..\..\target\release\manycraft.exe"
  File "..\icons\manycraft.ico"
  CreateShortcut "$SMPROGRAMS\Manycraft.lnk" "$INSTDIR\manycraft.exe" "" "$INSTDIR\manycraft.ico"
  WriteUninstaller "$INSTDIR\Uninstall.exe"
  WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\Manycraft" "DisplayName" "Manycraft"
  WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\Manycraft" "DisplayVersion" "${VERSION}"
  WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\Manycraft" "DisplayIcon" "$INSTDIR\manycraft.ico"
  WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\Manycraft" "UninstallString" "$INSTDIR\Uninstall.exe"
  WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\Manycraft" "Publisher" "Schrumbo"
SectionEnd

Section "Uninstall"
  Delete "$INSTDIR\manycraft.exe"
  Delete "$INSTDIR\manycraft.ico"
  Delete "$INSTDIR\Uninstall.exe"
  Delete "$SMPROGRAMS\Manycraft.lnk"
  RMDir "$INSTDIR"
  DeleteRegKey HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\Manycraft"
SectionEnd
