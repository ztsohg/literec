#ifndef AppVersion
  #define AppVersion "0.0.0"
#endif

[Setup]
AppId={{A2932C08-159C-4D02-B9FE-336008053176}
AppName=LiteRec
AppVersion={#AppVersion}
AppPublisher=LiteRec
DefaultDirName={autopf}\LiteRec
DefaultGroupName=LiteRec
OutputDir=..\dist
OutputBaseFilename=LiteRec-Setup-{#AppVersion}
SetupIconFile=..\assets\logo.ico
UninstallDisplayIcon={app}\literec.exe
Compression=lzma2
SolidCompression=yes
WizardStyle=modern
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
PrivilegesRequiredOverridesAllowed=dialog

[Tasks]
Name: "desktopicon"; Description: "Create a desktop shortcut"; GroupDescription: "Shortcuts:"

[Files]
Source: "..\target\release\literec.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\dist\ffmpeg\ffmpeg.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\dist\ffmpeg\LICENSE.txt"; DestDir: "{app}"; DestName: "ffmpeg-LICENSE.txt"; Flags: ignoreversion
Source: "..\THIRD_PARTY.txt"; DestDir: "{app}"; Flags: ignoreversion

[Icons]
Name: "{group}\LiteRec"; Filename: "{app}\literec.exe"
Name: "{group}\Uninstall LiteRec"; Filename: "{uninstallexe}"
Name: "{autodesktop}\LiteRec"; Filename: "{app}\literec.exe"; Tasks: desktopicon

[Run]
Filename: "{app}\literec.exe"; Description: "Launch LiteRec"; Flags: nowait postinstall skipifsilent
