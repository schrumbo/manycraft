Unicode true
!include "MUI2.nsh"

!ifdef BETA
  !define APP_ID "io.github.schrumbo.manycraft.beta"
  !define APP_NAME "Manycraft Beta"
  !define UNINSTALL_KEY "ManycraftBeta"
  !define ASSET_TAG "-beta"
!else
  !define APP_ID "io.github.schrumbo.manycraft"
  !define APP_NAME "Manycraft"
  !define UNINSTALL_KEY "Manycraft"
  !define ASSET_TAG ""
!endif

Name "${APP_NAME}"
OutFile "Manycraft-Setup-${VERSION}-windows-x64${ASSET_TAG}.exe"
InstallDir "$LOCALAPPDATA\Programs\${APP_NAME}"
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

LangString KeptText ${LANG_GERMAN} "${APP_NAME} wurde entfernt.$\r$\n$\r$\nInstanzen, Welten und Accounts bleiben in $LOCALAPPDATA\${APP_NAME}.$\r$\n$\r$\nDie Anmeldungen der Accounts bleiben in der Windows-Anmeldeinformationsverwaltung, auch wenn du die Daten löschst. Entferne die Accounts vorher in Manycraft, um sie mitzulöschen."
LangString KeptText ${LANG_ENGLISH} "${APP_NAME} has been removed.$\r$\n$\r$\nInstances, worlds and accounts stay in $LOCALAPPDATA\${APP_NAME}.$\r$\n$\r$\nAccount sign-ins stay in Windows Credential Manager, even if you delete the data. Remove the accounts in Manycraft first to delete them too."
LangString RemoveText ${LANG_GERMAN} "Auch diese Daten löschen"
LangString RemoveText ${LANG_ENGLISH} "Also delete this data"

Section "Install"
  SetOutPath "$INSTDIR"
  File "..\..\target\release\manycraft.exe"
  File "..\icons\manycraft.ico"
  CreateShortcut "$SMPROGRAMS\${APP_NAME}.lnk" "$INSTDIR\manycraft.exe" "" "$INSTDIR\manycraft.ico"
  WriteRegStr HKCU "Software\Classes\AppUserModelId\${APP_ID}" "DisplayName" "${APP_NAME}"
  WriteRegStr HKCU "Software\Classes\AppUserModelId\${APP_ID}" "IconUri" "$INSTDIR\manycraft.ico"
  WriteUninstaller "$INSTDIR\Uninstall.exe"
  WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\${UNINSTALL_KEY}" "DisplayName" "${APP_NAME}"
  WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\${UNINSTALL_KEY}" "DisplayVersion" "${VERSION}"
  WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\${UNINSTALL_KEY}" "DisplayIcon" "$INSTDIR\manycraft.ico"
  WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\${UNINSTALL_KEY}" "UninstallString" "$INSTDIR\Uninstall.exe"
  WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\${UNINSTALL_KEY}" "Publisher" "Schrumbo"
SectionEnd

Section "Uninstall"
  Delete "$INSTDIR\manycraft.exe"
  Delete "$INSTDIR\manycraft.ico"
  Delete "$INSTDIR\Uninstall.exe"
  Delete "$SMPROGRAMS\${APP_NAME}.lnk"
  RMDir "$INSTDIR"
  DeleteRegKey HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\${UNINSTALL_KEY}"
  DeleteRegKey HKCU "Software\Classes\AppUserModelId\${APP_ID}"
SectionEnd

Function un.RemoveData
  StrCmp $LOCALAPPDATA "" done
  IfFileExists "$LOCALAPPDATA\${APP_NAME}\*.*" 0 +2
  nsExec::Exec '"$SYSDIR\cmd.exe" /c rmdir /s /q "$LOCALAPPDATA\${APP_NAME}"'
  done:
FunctionEnd
