; Installer for Hesewake Media Bridge (Inno Setup 6). Build with: scripts\build-installer.ps1
; The script builds the program first and passes the version: ISCC /DAppVersion=0.1.0 ...

#ifndef AppVersion
  #error Pass the version: ISCC /DAppVersion=X.Y.Z
#endif
#define AppName "Hesewake Media Bridge"
#define AppExe "hesewake-media-bridge.exe"

[Setup]
; A fixed GUID, so that an upgrade replaces the old install.
AppId={{6F1D2A9C-7E34-4B5A-9C0E-3B8A41D7E2F5}
AppName={#AppName}
AppVersion={#AppVersion}
AppPublisher=dirga
AppComments=Media from an Android phone in Hesewake Notch, as an extension.
DefaultDirName={localappdata}\Programs\Hesewake Media Bridge
DisableProgramGroupPage=yes
DisableDirPage=auto
PrivilegesRequired=lowest
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
MinVersion=10.0.17763
OutputDir=..\installer_output
OutputBaseFilename=HesewakeMediaBridge-Setup-{#AppVersion}
UninstallDisplayIcon={app}\{#AppExe}
UninstallDisplayName={#AppName}
Compression=lzma2
SolidCompression=yes
WizardStyle=modern
LicenseFile=..\LICENSE
InfoBeforeFile=info.txt

[Languages]
Name: "english"; MessagesFile: "compiler:Default.isl"

[Files]
Source: "..\bridge\target\release\{#AppExe}"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\LICENSE"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\README.md"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\docs\protocol.md"; DestDir: "{app}\docs"; Flags: ignoreversion

[UninstallDelete]
; the manifest that tells the notch where the program is (it is not in the install folder)
Type: filesandordirs; Name: "{userappdata}\hesewake-notch\extensions\media-bridge"

[Code]
procedure StopBridge();
var
  R: Integer;
begin
  Exec(ExpandConstant('{sys}\taskkill.exe'), '/IM {#AppExe} /F', '', SW_HIDE, ewWaitUntilTerminated, R);
end;

function InitializeSetup(): Boolean;
begin
  StopBridge();
  Result := True;
end;

function InitializeUninstall(): Boolean;
begin
  StopBridge();
  Result := True;
end;

{ The notch finds an extension by its manifest, which names the program by its full path, so it is written here. }
procedure CurStepChanged(CurStep: TSetupStep);
var
  Dir, Exe, Json: String;
begin
  if CurStep = ssPostInstall then
  begin
    Dir := ExpandConstant('{userappdata}\hesewake-notch\extensions\media-bridge');
    ForceDirectories(Dir);
    Exe := ExpandConstant('{app}\{#AppExe}');
    StringChangeEx(Exe, '\', '\\', True);
    Json := '{"id":"media-bridge","name":"Media Bridge","version":"{#AppVersion}","exe":"' + Exe + '","slide":false,"settings":"panel","proto":1}';
    SaveStringsToUTF8File(Dir + '\extension.json', [Json], False);
  end;
end;
