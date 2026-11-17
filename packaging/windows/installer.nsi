Unicode true
!include "MUI2.nsh"

!define APP_ID "io.github.schrumbo.manycraft"

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
!define MUI_FINISHPAGE_TEXT "$(KeptText)"
!define MUI_FINISHPAGE_RUN
!define MUI_FINISHPAGE_RUN_NOTCHECKED
!define MUI_FINISHPAGE_RUN_TEXT "$(RemoveText)"
!define MUI_FINISHPAGE_RUN_FUNCTION un.RemoveData
!insertmacro MUI_UNPAGE_FINISH
!insertmacro MUI_LANGUAGE "German"
!insertmacro MUI_LANGUAGE "English"

LangString KeptText ${LANG_GERMAN} "Manycraft wurde entfernt.$\r$\n$\r$\nInstanzen, Welten und Accounts bleiben in $LOCALAPPDATA\Manycraft.$\r$\n$\r$\nDie Anmeldungen der Accounts bleiben in der Windows-Anmeldeinformationsverwaltung, auch wenn du die Daten löschst. Entferne die Accounts vorher in Manycraft, um sie mitzulöschen."
LangString KeptText ${LANG_ENGLISH} "Manycraft has been removed.$\r$\n$\r$\nInstances, worlds and accounts stay in $LOCALAPPDATA\Manycraft.$\r$\n$\r$\nAccount sign-ins stay in Windows Credential Manager, even if you delete the data. Remove the accounts in Manycraft first to delete them too."
LangString RemoveText ${LANG_GERMAN} "Auch diese Daten löschen"
LangString RemoveText ${LANG_ENGLISH} "Also delete this data"

Section "Install"
  SetOutPath "$INSTDIR"
  File "..\..\target\release\manycraft.exe"
  File "..\icons\manycraft.ico"
  CreateShortcut "$SMPROGRAMS\Manycraft.lnk" "$INSTDIR\manycraft.exe" "" "$INSTDIR\manycraft.ico"
  WriteRegStr HKCU "Software\Classes\AppUserModelId\${APP_ID}" "DisplayName" "Manycraft"
  WriteRegStr HKCU "Software\Classes\AppUserModelId\${APP_ID}" "IconUri" "$INSTDIR\manycraft.ico"
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
  DeleteRegKey HKCU "Software\Classes\AppUserModelId\${APP_ID}"
SectionEnd

Function un.RemoveData
  StrCmp $LOCALAPPDATA "" done
  IfFileExists "$LOCALAPPDATA\Manycraft\*.*" 0 +2
  nsExec::Exec '"$SYSDIR\cmd.exe" /c rmdir /s /q "$LOCALAPPDATA\Manycraft"'
  done:
FunctionEnd
