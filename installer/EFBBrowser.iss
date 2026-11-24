; Installer for the EFB Web Browser (MSFS 2024): built by installer\build.ps1 with Inno Setup 6.
;
; It only copies the companion .exe: the EFB package is embedded in it, and
; `efb-browser-companion.exe --install` deploys it to the Community folder and registers the
; companion in the sim's EXE.xml. Uninstalling runs `--uninstall` first.

#ifndef AppVersion
  #define AppVersion "0.0.0"
#endif
#define ExeName "efb-browser-companion.exe"
#define DataDir "{localappdata}\EFB Browser Companion"

[Setup]
AppId={{6A08559A-D3A6-46AB-9994-0477F400A5BE}
AppName=EFB Web Browser for MSFS 2024
AppVersion={#AppVersion}
AppVerName=EFB Web Browser {#AppVersion}
AppPublisher=ButterSim
AppPublisherURL=https://butr.cc
AppSupportURL=https://github.com/emergencybutter/butter-efb-browser/issues
AppUpdatesURL=https://github.com/emergencybutter/butter-efb-browser/releases
AppCopyright=MIT License
; Version info and icon on Setup.exe itself (an installer without them looks suspicious to AV heuristics).
VersionInfoVersion={#AppVersion}
VersionInfoCompany=ButterSim
VersionInfoDescription=EFB Web Browser for MSFS 2024 Setup
VersionInfoProductName=EFB Web Browser for MSFS 2024
VersionInfoProductVersion={#AppVersion}
SetupIconFile=..\companion\assets\app.ico
; Must match install::install_dir() so that --install finds itself already in place.
DefaultDirName={localappdata}\Programs\EFB Browser Companion
DisableDirPage=yes
DisableProgramGroupPage=yes
DisableReadyPage=yes
; Per-user install: EXE.xml and the Community folder belong to the user, no admin rights needed.
PrivilegesRequired=lowest
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
OutputDir=..\dist
OutputBaseFilename=EFB-Browser-Setup-{#AppVersion}
Compression=lzma2
SolidCompression=yes
WizardStyle=modern
UninstallDisplayIcon={app}\{#ExeName}
UninstallDisplayName=EFB Web Browser for MSFS 2024
; A running companion locks the exe: close it (setup refuses to run while the sim is up, see PrepareToInstall).
CloseApplications=force
RestartApplications=no

[Files]
Source: "..\companion\target\release\{#ExeName}"; DestDir: "{app}"; Flags: ignoreversion

[Icons]
; Manual start, e.g. to use the browser without the sim's auto-start.
Name: "{userprograms}\EFB Browser Companion"; Filename: "{app}\{#ExeName}"

[Messages]
FinishedLabel=The EFB Browser app is installed. Start Microsoft Flight Simulator 2024 and open the Browser app on the EFB: the companion starts and stops with the sim.

[Code]
function SimRunning(): Boolean;
var
  Locator, Wmi, Procs: Variant;
begin
  Result := False;
  try
    Locator := CreateOleObject('WbemScripting.SWbemLocator');
    Wmi := Locator.ConnectServer('.', 'root\CIMV2');
    Procs := Wmi.ExecQuery('SELECT ProcessId FROM Win32_Process WHERE Name = ''FlightSimulator2024.exe''');
    Result := Procs.Count > 0;
  except
  end;
end;

// The sim only loads Community packages when it starts: installing under a running sim makes the
// Browser app vanish from the EFB until the next start.
function PrepareToInstall(var NeedsRestart: Boolean): String;
begin
  Result := '';
  while SimRunning() do
    if MsgBox('Microsoft Flight Simulator 2024 is running. Quit the sim, then click Retry.', mbError, MB_RETRYCANCEL) = IDCANCEL then
    begin
      Result := 'Quit Microsoft Flight Simulator 2024, then run the installer again.';
      Exit;
    end;
end;

function RunCompanion(const Args: String): Integer;
var
  ResultCode: Integer;
begin
  if Exec(ExpandConstant('{app}\{#ExeName}'), Args, '', SW_HIDE, ewWaitUntilTerminated, ResultCode) then
    Result := ResultCode
  else
    Result := -1;
end;

procedure CurStepChanged(CurStep: TSetupStep);
begin
  if CurStep = ssPostInstall then
  begin
    if RunCompanion('--install') <> 0 then
      MsgBox('The files were copied, but the EFB app could not be set up for Microsoft Flight Simulator 2024.' + #13#10#13#10 +
        'Make sure the sim has been started at least once, then run the installer again.' + #13#10 +
        'Details: ' + ExpandConstant('{#DataDir}\install.log'), mbError, MB_OK);
  end;
end;

procedure CurUninstallStepChanged(CurUninstallStep: TUninstallStep);
begin
  if CurUninstallStep = usUninstall then
    RunCompanion('--uninstall');
end;
