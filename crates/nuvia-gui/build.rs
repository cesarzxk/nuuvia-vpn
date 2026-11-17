use std::path::PathBuf;
use std::process::Command;

const MANIFEST: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<assembly xmlns="urn:schemas-microsoft-com:asm.v1" manifestVersion="1.0">
  <trustInfo xmlns="urn:schemas-microsoft-com:asm.v3">
    <security>
      <requestedPrivileges>
        <requestedExecutionLevel level="requireAdministrator" uiAccess="false"/>
      </requestedPrivileges>
    </security>
  </trustInfo>
  <compatibility xmlns="urn:schemas-microsoft-com:compatibility.v1">
    <application>
      <supportedOS Id="{8e0f7a12-bfb3-4fe8-b9a5-48fd50a15a9a}"/>
    </application>
  </compatibility>
  <dependency>
    <dependentAssembly>
      <assemblyIdentity type="win32"
        name="Microsoft.Windows.Common-Controls"
        version="6.0.0.0"
        processorArchitecture="*"
        publicKeyToken="6595b64144ccf1df"
        language="*"/>
    </dependentAssembly>
  </dependency>
</assembly>"#;

fn main() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }

    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let icon = manifest_dir.join("../../deploy/windows/nuvia.ico");
    let out = PathBuf::from(std::env::var("OUT_DIR").expect("OUT_DIR"));

    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed={}", icon.display());

    let manifest = out.join("nuvia.manifest");
    if let Err(error) = std::fs::write(&manifest, MANIFEST) {
        println!("cargo:warning=manifest: {error}");
        return;
    }

    let icon_path = icon.to_string_lossy().replace('\\', "/");
    let manifest_path = manifest.to_string_lossy().replace('\\', "/");
    let rc = out.join("nuvia.rc");
    let rc_text = format!("1 24 \"{manifest_path}\"\nIDI_ICON1 ICON \"{icon_path}\"\n");
    if let Err(error) = std::fs::write(&rc, rc_text) {
        println!("cargo:warning=resource script: {error}");
        return;
    }

    let object = out.join("nuvia-resource.o");
    let windres = if std::env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("gnu") {
        "x86_64-w64-mingw32-windres"
    } else {
        "windres"
    };
    match Command::new(windres)
        .args(["-O", "coff", "-i"])
        .arg(&rc)
        .arg("-o")
        .arg(&object)
        .status()
    {
        Ok(status) if status.success() => {
            println!("cargo:rustc-link-arg={}", object.display());
        }
        Ok(status) => println!("cargo:warning={windres} exited with {status}"),
        Err(error) => println!("cargo:warning=could not run {windres}: {error}"),
    }
}
