; Sill for Windows installer (Inno Setup). CI builds it as SillSetup.exe.
#ifndef AppVersion
  #define AppVersion "0.1.0"
#endif

[Setup]
AppName=Sill for Windows
AppVersion={#AppVersion}
AppPublisher=Tanooki Studios
DefaultDirName={autopf}\Sill for Windows
DefaultGroupName=Sill for Windows
DisableProgramGroupPage=yes
OutputBaseFilename=SillSetup
Compression=lzma2
SolidCompression=yes
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
PrivilegesRequired=admin
UninstallDisplayName=Sill for Windows
WizardStyle=modern

[Tasks]
Name: "startup"; Description: "Start Sill when I sign in to Windows"; Flags: unchecked
Name: "desktop"; Description: "Add a desktop shortcut"; Flags: unchecked

[Files]
Source: "..\dist\SillHost.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\dist\README.txt"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\dist\LICENSE.txt"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\dist\THIRD-PARTY-NOTICES.txt"; DestDir: "{app}"; Flags: ignoreversion

[Icons]
Name: "{group}\Sill for Windows"; Filename: "{app}\SillHost.exe"
Name: "{group}\Uninstall Sill for Windows"; Filename: "{uninstallexe}"
Name: "{autodesktop}\Sill for Windows"; Filename: "{app}\SillHost.exe"; Tasks: desktop
Name: "{userstartup}\Sill for Windows"; Filename: "{app}\SillHost.exe"; Tasks: startup

[Run]
; Installing runs as administrator, so the firewall rule goes in here and Windows never has to ask.
Filename: "{sys}\netsh.exe"; Parameters: "advfirewall firewall add rule name=""Sill for Windows"" dir=in action=allow program=""{app}\SillHost.exe"" enable=yes profile=private,domain"; Flags: runhidden
Filename: "{app}\SillHost.exe"; Description: "Start Sill now"; Flags: nowait postinstall skipifsilent unchecked

[UninstallRun]
Filename: "{sys}\netsh.exe"; Parameters: "advfirewall firewall delete rule name=""Sill for Windows"""; Flags: runhidden; RunOnceId: "firewall"
