; Inno Setup script for Zavyx Download Manager.
; Built by scripts\package.ps1, which stages files into dist\staging first:
;   iscc /DAppVersion=0.1.0 installer\zavyx.iss
;
; Installs per-user (no admin prompt), registers the browser native messaging
; host for Chrome, Edge, Brave, Vivaldi, Opera and Firefox, and removes it all
; on uninstall.

#ifndef AppVersion
  #define AppVersion "0.0.0"
#endif
#define AppName "Zavyx Download Manager"
#define HostName "io.github.mohammadibrahimdev.zavyx"
#define ExtensionId "gileklmimbdimpngahanhmggcejpiaal"
#define FirefoxId "zavyx@mohammadibrahimdev.github.io"

[Setup]
AppId={{6F1E8D2B-5C7A-4B8E-9D3F-2A6C1E7B4D90}
AppName={#AppName}
AppVersion={#AppVersion}
AppVerName={#AppName} {#AppVersion}
AppPublisher=Mohammad Ibrahim
AppPublisherURL=https://github.com/mohammadibrahimdev
AppSupportURL=https://github.com/mohammadibrahimdev/zavyx/issues
AppUpdatesURL=https://github.com/mohammadibrahimdev/zavyx/releases
DefaultDirName={localappdata}\Programs\Zavyx
DefaultGroupName=Zavyx
DisableProgramGroupPage=yes
PrivilegesRequired=lowest
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
OutputDir=..\dist
OutputBaseFilename=Zavyx-Setup-{#AppVersion}
SetupIconFile=..\assets\zavyx.ico
UninstallDisplayIcon={app}\Zavyx.exe
UninstallDisplayName={#AppName}
LicenseFile=..\LICENSE
WizardStyle=modern
Compression=lzma2/max
SolidCompression=yes
CloseApplications=force

[Tasks]
Name: "desktopicon"; Description: "Create a desktop shortcut"; GroupDescription: "Shortcuts:"

[Files]
Source: "..\dist\staging\*"; DestDir: "{app}"; Flags: recursesubdirs ignoreversion

[Icons]
Name: "{group}\{#AppName}"; Filename: "{app}\Zavyx.exe"
Name: "{group}\Uninstall {#AppName}"; Filename: "{uninstallexe}"
Name: "{userdesktop}\{#AppName}"; Filename: "{app}\Zavyx.exe"; Tasks: desktopicon

[Registry]
Root: HKCU; Subkey: "Software\Google\Chrome\NativeMessagingHosts\{#HostName}"; ValueType: string; ValueName: ""; ValueData: "{app}\{#HostName}.json"; Flags: uninsdeletekey
Root: HKCU; Subkey: "Software\Microsoft\Edge\NativeMessagingHosts\{#HostName}"; ValueType: string; ValueName: ""; ValueData: "{app}\{#HostName}.json"; Flags: uninsdeletekey
Root: HKCU; Subkey: "Software\BraveSoftware\Brave-Browser\NativeMessagingHosts\{#HostName}"; ValueType: string; ValueName: ""; ValueData: "{app}\{#HostName}.json"; Flags: uninsdeletekey
Root: HKCU; Subkey: "Software\Vivaldi\Vivaldi\NativeMessagingHosts\{#HostName}"; ValueType: string; ValueName: ""; ValueData: "{app}\{#HostName}.json"; Flags: uninsdeletekey
Root: HKCU; Subkey: "Software\OperaSoftware\Opera\NativeMessagingHosts\{#HostName}"; ValueType: string; ValueName: ""; ValueData: "{app}\{#HostName}.json"; Flags: uninsdeletekey
Root: HKCU; Subkey: "Software\Mozilla\NativeMessagingHosts\{#HostName}"; ValueType: string; ValueName: ""; ValueData: "{app}\{#HostName}.firefox.json"; Flags: uninsdeletekey

[Run]
Filename: "{app}\Zavyx.exe"; Description: "Start Zavyx now"; Flags: nowait postinstall skipifsilent

[UninstallRun]
Filename: "{sys}\taskkill.exe"; Parameters: "/IM Zavyx.exe /F"; Flags: runhidden; RunOnceId: "StopZavyx"

[UninstallDelete]
Type: files; Name: "{app}\{#HostName}.json"
Type: files; Name: "{app}\{#HostName}.firefox.json"

[Code]
// Write the native messaging manifests with the real install path.
procedure WriteHostManifest(FileName, AllowKey, AllowValue: String);
var
  HostPath: String;
begin
  HostPath := ExpandConstant('{app}\zavyx-host.exe');
  StringChangeEx(HostPath, '\', '\\', True);
  SaveStringToFile(ExpandConstant('{app}\') + FileName,
    '{' + #13#10 +
    '  "name": "{#HostName}",' + #13#10 +
    '  "description": "Zavyx Download Manager browser connector",' + #13#10 +
    '  "path": "' + HostPath + '",' + #13#10 +
    '  "type": "stdio",' + #13#10 +
    '  "' + AllowKey + '": [ "' + AllowValue + '" ]' + #13#10 +
    '}' + #13#10, False);
end;

procedure CurStepChanged(CurStep: TSetupStep);
begin
  if CurStep = ssPostInstall then
  begin
    WriteHostManifest('{#HostName}.json', 'allowed_origins', 'chrome-extension://{#ExtensionId}/');
    WriteHostManifest('{#HostName}.firefox.json', 'allowed_extensions', '{#FirefoxId}');
  end;
end;
