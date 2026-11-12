; nibwarden's Windows installer (Inno Setup 6), made by `cargo xtask dist`, which passes
; AppVersion, SourceDir (the programs), OutputDir, OutputName and RepoRoot.
;
; A per-user installation, like nibwarden itself: no administrator rights, into
; %LOCALAPPDATA%\Programs\nibwarden, with that folder added to the user's PATH so
; `nibwarden` and `nibwarden-ssh-sign` work in a terminal.

#ifndef AppVersion
  #error AppVersion is not defined; build with cargo xtask dist
#endif

[Setup]
AppId={{6F1C7D2A-4E1B-4B8E-9C55-0D6A2E9B5F31}
AppName=nibwarden
AppVersion={#AppVersion}
AppVerName=nibwarden {#AppVersion}
AppPublisher=Hans
AppPublisherURL=https://github.com/hans00/nibwarden
AppSupportURL=https://github.com/hans00/nibwarden/issues
DefaultDirName={localappdata}\Programs\nibwarden
DefaultGroupName=nibwarden
DisableProgramGroupPage=yes
DisableDirPage=yes
PrivilegesRequired=lowest
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
LicenseFile={#RepoRoot}\LICENSE
SetupIconFile={#RepoRoot}\packaging\windows\nibwarden.ico
UninstallDisplayIcon={app}\nibwarden-gui.exe
OutputDir={#OutputDir}
OutputBaseFilename={#OutputName}
Compression=lzma2
SolidCompression=yes
WizardStyle=modern
ChangesEnvironment=yes
; The daemon, the tray icon and the shortcut's holder keep the programs open.
CloseApplications=force
RestartApplications=no

[Languages]
Name: "en"; MessagesFile: "compiler:Default.isl"

[Files]
Source: "{#SourceDir}\nibwarden-gui.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#SourceDir}\nibwardend.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#SourceDir}\nibwarden.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#SourceDir}\nibwarden-ssh-sign.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#SourceDir}\nibwarden-browser-host.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#SourceDir}\README.md"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#SourceDir}\LICENSE"; DestDir: "{app}"; Flags: ignoreversion

[Icons]
Name: "{userprograms}\nibwarden"; Filename: "{app}\nibwarden-gui.exe"
Name: "{userprograms}\nibwarden quick access"; Filename: "{app}\nibwarden-gui.exe"; Parameters: "quick"

[Registry]
; Entries nibwarden itself adds at run time (start at login, the tray icon, the shortcut's
; holder): only removed when uninstalling.
Root: HKCU; Subkey: "Software\Microsoft\Windows\CurrentVersion\Run"; ValueName: "nibwardend"; ValueType: none; Flags: uninsdeletevalue
Root: HKCU; Subkey: "Software\Microsoft\Windows\CurrentVersion\Run"; ValueName: "nibwarden-tray"; ValueType: none; Flags: uninsdeletevalue
Root: HKCU; Subkey: "Software\Microsoft\Windows\CurrentVersion\Run"; ValueName: "nibwarden-shortcut"; ValueType: none; Flags: uninsdeletevalue

[Run]
Filename: "{app}\nibwarden-gui.exe"; Description: "Open nibwarden"; Flags: nowait postinstall skipifsilent

[UninstallRun]
; The browsers' registrations point into {app}.
Filename: "{app}\nibwarden.exe"; Parameters: "browser disable"; Flags: runhidden; RunOnceId: "BrowserDisable"
Filename: "{sys}\taskkill.exe"; Parameters: "/f /im nibwarden-gui.exe"; Flags: runhidden; RunOnceId: "StopGui"
Filename: "{sys}\taskkill.exe"; Parameters: "/f /im nibwardend.exe"; Flags: runhidden; RunOnceId: "StopDaemon"

[Code]
const
  EnvironmentKey = 'Environment';

function PathHas(Paths, Dir: string): Boolean;
begin
  Result := Pos(';' + Uppercase(Dir) + ';', ';' + Uppercase(Paths) + ';') > 0;
end;

procedure CurStepChanged(CurStep: TSetupStep);
var
  Paths: string;
begin
  if CurStep = ssPostInstall then
  begin
    if not RegQueryStringValue(HKCU, EnvironmentKey, 'Path', Paths) then
      Paths := '';
    if not PathHas(Paths, ExpandConstant('{app}')) then
    begin
      if (Paths <> '') and (Copy(Paths, Length(Paths), 1) <> ';') then
        Paths := Paths + ';';
      RegWriteExpandStringValue(HKCU, EnvironmentKey, 'Path', Paths + ExpandConstant('{app}'));
    end;
  end;
end;

procedure CurUninstallStepChanged(CurUninstallStep: TUninstallStep);
var
  Paths, Dir: string;
  P: Integer;
begin
  if CurUninstallStep = usPostUninstall then
  begin
    if RegQueryStringValue(HKCU, EnvironmentKey, 'Path', Paths) then
    begin
      Dir := ExpandConstant('{app}');
      Paths := ';' + Paths + ';';
      P := Pos(';' + Uppercase(Dir) + ';', Uppercase(Paths));
      if P > 0 then
      begin
        Delete(Paths, P, Length(Dir) + 1);
        Paths := Copy(Paths, 2, Length(Paths) - 2);
        RegWriteExpandStringValue(HKCU, EnvironmentKey, 'Path', Paths);
      end;
    end;
  end;
end;
