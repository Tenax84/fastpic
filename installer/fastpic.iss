; FastPic telepítő (Inno Setup 6).
; Fordítás: .\scripts\build-release.ps1  (vagy: ISCC.exe installer\fastpic.iss)
;
; Alapból felhasználói telepítés (rendszergazda nélkül); a telepítő indításakor
; választható a minden felhasználóra kiterjedő telepítés is.

#define AppName "FastPic"
#define AppExe "fastpic.exe"
#define AppVersion GetVersionNumbersString(AddBackslash(SourcePath) + "..\target\release\fastpic.exe")
#define ProgId "FastPic.Image"
; A src/core/formats.rs EXTENSIONS listájával szinkronban tartandó.
#define Exts "jpg;jpeg;jpe;jfif;png;apng;bmp;dib;gif;tif;tiff;ico;cur;webp;jxl;svg;svgz;qoi;tga;pnm;pbm;pgm;ppm;pam;dds;hdr;heic;heif;hif;avif;jxr;wdp;hdp;"

[Setup]
AppId={{6F3C2B8E-5A41-4E0B-9C7D-FA57B1C0E2D4}
AppName={#AppName}
AppVersion={#AppVersion}
AppVerName={#AppName} {#AppVersion}
AppPublisher={#AppName}
VersionInfoVersion={#AppVersion}
VersionInfoDescription={#AppName} telepítő
DefaultDirName={autopf}\{#AppName}
DefaultGroupName={#AppName}
DisableProgramGroupPage=yes
PrivilegesRequired=lowest
PrivilegesRequiredOverridesAllowed=dialog
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
MinVersion=10.0.17763
OutputDir=..\dist
OutputBaseFilename=FastPic-Setup-{#AppVersion}
SetupIconFile=..\assets\fastpic.ico
UninstallDisplayIcon={app}\{#AppExe}
UninstallDisplayName={#AppName}
Compression=lzma2/ultra64
SolidCompression=yes
WizardStyle=modern
ChangesAssociations=yes
CloseApplications=yes

[Languages]
Name: "hu"; MessagesFile: "compiler:Languages\Hungarian.isl"
Name: "en"; MessagesFile: "compiler:Default.isl"

[Tasks]
Name: "desktopicon"; Description: "{cm:CreateDesktopIcon}"; GroupDescription: "{cm:AdditionalIcons}"; Flags: unchecked

[Files]
Source: "..\target\release\{#AppExe}"; DestDir: "{app}"; Flags: ignoreversion

[Icons]
Name: "{autoprograms}\{#AppName}"; Filename: "{app}\{#AppExe}"
Name: "{autodesktop}\{#AppName}"; Filename: "{app}\{#AppExe}"; Tasks: desktopicon

[Registry]
; ProgID: így jelenik meg a „Társítás ezzel” listában, saját ikonnal.
Root: HKA; Subkey: "Software\Classes\{#ProgId}"; ValueType: string; ValueName: ""; ValueData: "{#AppName} kép"; Flags: uninsdeletekey
Root: HKA; Subkey: "Software\Classes\{#ProgId}\DefaultIcon"; ValueType: string; ValueName: ""; ValueData: """{app}\{#AppExe}"",0"
Root: HKA; Subkey: "Software\Classes\{#ProgId}\shell\open\command"; ValueType: string; ValueName: ""; ValueData: """{app}\{#AppExe}"" ""%1"""
; Alkalmazás-bejegyzés.
Root: HKA; Subkey: "Software\Classes\Applications\{#AppExe}"; ValueType: string; ValueName: "FriendlyAppName"; ValueData: "{#AppName}"; Flags: uninsdeletekey
Root: HKA; Subkey: "Software\Classes\Applications\{#AppExe}\shell\open\command"; ValueType: string; ValueName: ""; ValueData: """{app}\{#AppExe}"" ""%1"""
; Capabilities: ettől jelenik meg a Beállítások → Alapértelmezett alkalmazások között.
Root: HKA; Subkey: "Software\{#AppName}"; Flags: uninsdeletekeyifempty
Root: HKA; Subkey: "Software\{#AppName}\Capabilities"; ValueType: string; ValueName: "ApplicationName"; ValueData: "{#AppName}"; Flags: uninsdeletekey
Root: HKA; Subkey: "Software\{#AppName}\Capabilities"; ValueType: string; ValueName: "ApplicationDescription"; ValueData: "Gyors képnézegető"
Root: HKA; Subkey: "Software\{#AppName}\Capabilities"; ValueType: string; ValueName: "ApplicationIcon"; ValueData: """{app}\{#AppExe}"",0"
Root: HKA; Subkey: "Software\RegisteredApplications"; ValueType: string; ValueName: "{#AppName}"; ValueData: "Software\{#AppName}\Capabilities"; Flags: uninsdeletevalue
; Kiterjesztésenként: „Társítás ezzel” lista, támogatott típusok, képességek.
#sub EmitExt
  #define Ext Copy(Exts, 1, Pos(";", Exts) - 1)
  #expr Exts = Copy(Exts, Pos(";", Exts) + 1, Len(Exts))
Root: HKA; Subkey: "Software\Classes\.{#Ext}\OpenWithProgids"; ValueType: string; ValueName: "{#ProgId}"; ValueData: ""; Flags: uninsdeletevalue
Root: HKA; Subkey: "Software\Classes\Applications\{#AppExe}\SupportedTypes"; ValueType: string; ValueName: ".{#Ext}"; ValueData: ""
Root: HKA; Subkey: "Software\{#AppName}\Capabilities\FileAssociations"; ValueType: string; ValueName: ".{#Ext}"; ValueData: "{#ProgId}"
#endsub
#define I
#for {I = 0; Len(Exts) > 0; I++} EmitExt

[Run]
Filename: "ms-settings:defaultapps?registeredAppUser={#AppName}"; Description: "Beállítás alapértelmezett képnézegetőként (Windows Beállítások)"; Flags: shellexec postinstall skipifsilent nowait unchecked
Filename: "{app}\{#AppExe}"; Description: "{cm:LaunchProgram,{#AppName}}"; Flags: postinstall skipifsilent nowait unchecked

[UninstallDelete]
; A mentett ablakállapot (felhasználónként).
Type: filesandordirs; Name: "{userappdata}\{#AppName}"
