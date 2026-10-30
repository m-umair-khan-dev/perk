//! For Windows targets: generate the application icon from the same renderer
//! as the tray icons, and embed it with a manifest and version info.

use std::env;
use std::fs;
use std::path::PathBuf;

#[path = "src/icon/render.rs"]
mod render;

fn main() {
    println!("cargo:rerun-if-changed=src/icon/render.rs");
    println!("cargo:rerun-if-changed=build.rs");

    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }

    let out = PathBuf::from(env::var("OUT_DIR").unwrap());
    let version = env::var("CARGO_PKG_VERSION").unwrap();
    let parts: Vec<u16> = version
        .split(['.', '-'])
        .take(3)
        .map(|p| p.parse().unwrap_or(0))
        .collect();
    let (major, minor, patch) = (parts[0], parts[1], parts[2]);

    fs::write(
        out.join("perk.ico"),
        render::ico(render::IconKind::Logo, &[16, 20, 24, 32, 40, 48, 64, 256]),
    )
    .unwrap();
    fs::write(
        out.join("perk.manifest"),
        MANIFEST.replace("{version}", &format!("{major}.{minor}.{patch}.0")),
    )
    .unwrap();

    let rc = format!(
        r#"1 ICON "perk.ico"
1 24 "perk.manifest"
1 VERSIONINFO
FILEVERSION {major},{minor},{patch},0
PRODUCTVERSION {major},{minor},{patch},0
FILEOS 0x40004
FILETYPE 0x1
BEGIN
  BLOCK "StringFileInfo"
  BEGIN
    BLOCK "040904B0"
    BEGIN
      VALUE "CompanyName", "Perk contributors"
      VALUE "FileDescription", "Perk - keep your computer awake"
      VALUE "FileVersion", "{version}"
      VALUE "InternalName", "perk"
      VALUE "OriginalFilename", "perk.exe"
      VALUE "ProductName", "Perk"
      VALUE "ProductVersion", "{version}"
    END
  END
  BLOCK "VarFileInfo"
  BEGIN
    VALUE "Translation", 0x409, 1200
  END
END
"#
    );
    let rc_path = out.join("perk.rc");
    fs::write(&rc_path, rc).unwrap();
    embed_resource::compile(&rc_path, embed_resource::NONE)
        .manifest_required()
        .unwrap();
}

const MANIFEST: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<assembly xmlns="urn:schemas-microsoft-com:asm.v1" manifestVersion="1.0">
  <assemblyIdentity type="win32" name="Perk" version="{version}"/>
  <dependency>
    <dependentAssembly>
      <assemblyIdentity type="win32" name="Microsoft.Windows.Common-Controls" version="6.0.0.0"
                        processorArchitecture="*" publicKeyToken="6595b64144ccf1df" language="*"/>
    </dependentAssembly>
  </dependency>
  <application xmlns="urn:schemas-microsoft-com:asm.v3">
    <windowsSettings>
      <dpiAware xmlns="http://schemas.microsoft.com/SMI/2005/WindowsSettings">true/pm</dpiAware>
      <dpiAwareness xmlns="http://schemas.microsoft.com/SMI/2016/WindowsSettings">PerMonitorV2, PerMonitor</dpiAwareness>
    </windowsSettings>
  </application>
  <compatibility xmlns="urn:schemas-microsoft-com:compatibility.v1">
    <application>
      <supportedOS Id="{8e0f7a12-bfb3-4fe8-b9a5-48fd50a15a9a}"/>
    </application>
  </compatibility>
</assembly>
"#;
