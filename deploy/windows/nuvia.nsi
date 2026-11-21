; nuvia VPN installer (NSIS 3)
; Build from the repository root:
;   makensis -DVERSION=1.5.0 deploy/windows/nuvia.nsi

Unicode true
ManifestDPIAware true
RequestExecutionLevel admin
SetCompressor /SOLID lzma
CRCCheck on

!include "MUI2.nsh"
!include "x64.nsh"

!ifndef VERSION
  !define VERSION "1.5.0"
!endif

!ifndef ROOT
  !define ROOT "${__FILEDIR__}/../.."
!endif
!define APP_EXE "${ROOT}/dist/windows/nuvia-gui.exe"
!define HELPER_EXE "${ROOT}/dist/windows/nuvia-helper.exe"
!define CLI_EXE "${ROOT}/dist/windows/nuvia.exe"
!define PROBE_EXE "${ROOT}/dist/windows/lanprobe.exe"
!define DRIVER_DIR "${ROOT}/dist/windows/driver"
!define SOFT_DIR "${ROOT}/dist/windows/soft"
!define TAP_SCRIPT "${ROOT}/dist/windows/install-tap.ps1"
!define APP_README "${ROOT}/dist/windows/README.md"
!define APP_ICON "${ROOT}/deploy/windows/nuvia-bmp.ico"

Name "Nuuvia VPN"
OutFile "${ROOT}/dist/nuvia-setup-x86_64.exe"
InstallDir "$PROGRAMFILES64\nuvia"
InstallDirRegKey HKLM "Software\nuvia" "InstallDir"
BrandingText "Nuuvia"

!ifndef ICON
  !define ICON "${__FILEDIR__}/nuvia-nsis.ico"
!endif

!define MUI_ICON "${ICON}"
!define MUI_UNICON "${ICON}"
!define MUI_ABORTWARNING
!define MUI_FINISHPAGE_RUN "$INSTDIR\nuvia-gui.exe"
!define MUI_FINISHPAGE_RUN_TEXT "Abrir o Nuuvia VPN agora"
!define MUI_WELCOMEPAGE_TITLE "Nuuvia VPN"
!define MUI_WELCOMEPAGE_TEXT "LAN virtual criptografada para jogos, Linux e Windows.$\r$\n$\r$\nO instalador copia o Nuuvia e o driver TAP assinado. É preciso ser administrador."
!define MUI_DIRECTORYPAGE_TEXT_TOP "Escolha a pasta de instalação do Nuuvia."

VIProductVersion "${VERSION}.0"
VIAddVersionKey "CompanyName" "Nuuvia contributors"
VIAddVersionKey "ProductName" "Nuuvia"
VIAddVersionKey "FileDescription" "Instalador do Nuuvia VPN"
VIAddVersionKey "FileVersion" "${VERSION}"
VIAddVersionKey "ProductVersion" "${VERSION}"
VIAddVersionKey "InternalName" "nuvia-setup"
VIAddVersionKey "OriginalFilename" "nuvia-setup-x86_64.exe"
VIAddVersionKey "LegalCopyright" "MIT License"

!insertmacro MUI_PAGE_WELCOME
!insertmacro MUI_PAGE_DIRECTORY
!insertmacro MUI_PAGE_INSTFILES
!insertmacro MUI_PAGE_FINISH

!insertmacro MUI_UNPAGE_CONFIRM
!insertmacro MUI_UNPAGE_INSTFILES

!insertmacro MUI_LANGUAGE "PortugueseBR"

Function .onInit
  ${IfNot} ${RunningX64}
    MessageBox MB_ICONSTOP "O Nuuvia requer Windows 64-bit."
    Abort
  ${EndIf}
  SetShellVarContext all
FunctionEnd

Function un.onInit
  SetShellVarContext all
FunctionEnd

Section "Nuuvia VPN" SEC_MAIN
  SectionIn RO

  ; Encerra instâncias antigas (inclusive o helper de tray) para liberar os
  ; arquivos e o adaptador TAP.
  DetailPrint "Fechando instâncias do Nuuvia..."
  nsExec::Exec 'taskkill /F /T /IM nuvia-gui.exe'
  Pop $0
  nsExec::Exec 'taskkill /F /T /IM nuvia.exe'
  Pop $0
  Sleep 1000

  SetOutPath "$INSTDIR"
  File "/oname=nuvia-gui.exe" "${APP_EXE}"
  File "/oname=nuvia-helper.exe" "${HELPER_EXE}"
  File "/oname=nuvia.exe" "${CLI_EXE}"
  File "/oname=lanprobe.exe" "${PROBE_EXE}"
  File "/oname=install-tap.ps1" "${TAP_SCRIPT}"
  File "/oname=reset-tap.ps1" "${ROOT}/dist/windows/reset-tap.ps1"
  File "/oname=README.md" "${APP_README}"

  SetOutPath "$INSTDIR\driver"
  File /r "${DRIVER_DIR}/*.*"

  ; Compatibility renderer: Windows loads opengl32.dll from the executable's
  ; directory first, so this copy always uses Mesa llvmpipe (software OpenGL).
  SetOutPath "$INSTDIR\soft"
  File "/oname=opengl32.dll" "${SOFT_DIR}/opengl32.dll"
  File "/oname=libgallium_wgl.dll" "${SOFT_DIR}/libgallium_wgl.dll"
  File "/oname=MESA-LICENSE.txt" "${SOFT_DIR}/MESA-LICENSE.txt"
  File "/oname=nuvia-gui.exe.local" "${SOFT_DIR}/nuvia-gui.exe.local"
  Delete "$INSTDIR\soft\nuvia-gui.exe"
  CopyFiles "$INSTDIR\nuvia-gui.exe" "$INSTDIR\soft"

  SetOutPath "$INSTDIR"
  WriteUninstaller "$INSTDIR\uninstall.exe"

  WriteRegStr HKLM "Software\nuvia" "InstallDir" "$INSTDIR"
  WriteRegStr HKLM "Software\Microsoft\Windows\CurrentVersion\Uninstall\nuvia" "DisplayName" "Nuuvia VPN"
  WriteRegStr HKLM "Software\Microsoft\Windows\CurrentVersion\Uninstall\nuvia" "DisplayVersion" "${VERSION}"
  WriteRegStr HKLM "Software\Microsoft\Windows\CurrentVersion\Uninstall\nuvia" "Publisher" "Nuuvia"
  WriteRegStr HKLM "Software\Microsoft\Windows\CurrentVersion\Uninstall\nuvia" "InstallLocation" "$INSTDIR"
  WriteRegStr HKLM "Software\Microsoft\Windows\CurrentVersion\Uninstall\nuvia" "DisplayIcon" "$INSTDIR\nuvia-gui.exe"
  WriteRegStr HKLM "Software\Microsoft\Windows\CurrentVersion\Uninstall\nuvia" "UninstallString" '"$INSTDIR\uninstall.exe"'
  WriteRegDWORD HKLM "Software\Microsoft\Windows\CurrentVersion\Uninstall\nuvia" "NoModify" 1
  WriteRegDWORD HKLM "Software\Microsoft\Windows\CurrentVersion\Uninstall\nuvia" "NoRepair" 1

  CreateDirectory "$SMPROGRAMS\Nuuvia"
  CreateShortcut "$SMPROGRAMS\Nuuvia\Nuuvia VPN.lnk" "$INSTDIR\nuvia-gui.exe" "" "$INSTDIR\nuvia-gui.exe" 0
  CreateShortcut "$SMPROGRAMS\Nuuvia\Nuuvia VPN (compatibilidade).lnk" "$INSTDIR\soft\nuvia-gui.exe" "" "$INSTDIR\soft\nuvia-gui.exe" 0
  CreateShortcut "$DESKTOP\Nuuvia VPN.lnk" "$INSTDIR\nuvia-gui.exe" "" "$INSTDIR\nuvia-gui.exe" 0
  Delete "$DESKTOP\nuvia VPN.lnk"
  Delete "$SMPROGRAMS\nuvia\nuvia VPN.lnk"
  RMDir "$SMPROGRAMS\nuvia"

  DetailPrint "Instalando o driver TAP-Windows6..."
  nsExec::ExecToLog 'powershell.exe -NoProfile -NonInteractive -ExecutionPolicy Bypass -File "$INSTDIR\install-tap.ps1" -DriverDir "$INSTDIR\driver"'
  Pop $0
  ${If} $0 != 0
    DetailPrint "Aviso: install-tap.ps1 retornou $0; o Nuuvia tentará reparar o TAP ao conectar."
  ${EndIf}

  DetailPrint "Instalando o serviço privilegiado do Nuuvia..."
  nsExec::ExecToLog '"$INSTDIR\nuvia-helper.exe" --install-service'
  Pop $0
  ${If} $0 != 0
    DetailPrint "Aviso: não foi possível instalar o serviço nuvia-helper ($0)."
  ${EndIf}
SectionEnd

Section "Uninstall"
  nsExec::Exec 'taskkill /F /T /IM nuvia-gui.exe'
  Pop $0
  nsExec::Exec 'taskkill /F /T /IM nuvia.exe'
  Pop $0
  nsExec::Exec 'taskkill /F /IM nuvia-helper.exe'
  Pop $0
  nsExec::ExecToLog '"$INSTDIR\nuvia-helper.exe" --uninstall-service'
  Pop $0
  Sleep 500

  Delete "$DESKTOP\Nuuvia VPN.lnk"
  Delete "$DESKTOP\nuvia VPN.lnk"
  Delete "$SMPROGRAMS\Nuuvia\Nuuvia VPN.lnk"
  Delete "$SMPROGRAMS\Nuuvia\Nuuvia VPN (compatibilidade).lnk"
  Delete "$SMPROGRAMS\nuvia\nuvia VPN.lnk"
  RMDir "$SMPROGRAMS\Nuuvia"
  RMDir "$SMPROGRAMS\nuvia"

  RMDir /r "$INSTDIR"

  DeleteRegKey HKLM "Software\Microsoft\Windows\CurrentVersion\Uninstall\nuvia"
  DeleteRegKey HKLM "Software\nuvia"
SectionEnd
