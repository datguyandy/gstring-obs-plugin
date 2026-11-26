#ifndef PluginVersion
  #define PluginVersion "0.1.0"
#endif

[Setup]
AppId={{8C9E1B5E-6D43-4F2F-9F1A-2B9E42C1D7A4}
AppName=GString Captions (OBS Plugin)
AppVersion={#PluginVersion}
AppPublisher=GString
AppPublisherURL=https://gstringbot.com
DefaultDirName={commonappdata}\obs-studio\plugins\gstring-captions
DisableDirPage=yes
DisableProgramGroupPage=yes
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
PrivilegesRequired=admin
OutputDir=..\..\dist
OutputBaseFilename=gstring-captions-windows-x64-setup
Compression=lzma2
SolidCompression=yes
WizardStyle=modern

[Files]
Source: "..\..\target\release\gstring_captions.dll"; DestDir: "{app}\bin\64bit"; DestName: "gstring-captions.dll"; Flags: ignoreversion

[Messages]
FinishedLabel=GString Captions has been installed for OBS Studio.%n%nRestart OBS Studio, open Filters on your Microphone or audio source in the Audio Mixer, add "GString Captions", and paste your OBS plugin key from the GString dashboard.
