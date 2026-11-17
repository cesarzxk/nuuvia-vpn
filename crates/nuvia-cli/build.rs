use std::path::PathBuf;
use std::process::Command;

fn version_info(description: &str, internal: &str, original: &str) -> String {
    let version = std::env::var("CARGO_PKG_VERSION").unwrap_or_else(|_| "0.0.0".into());
    let mut parts = version.split(['.', '-', '+']);
    let mut numbers = [0u16; 4];
    for number in &mut numbers {
        if let Some(part) = parts.next() {
            *number = part.parse().unwrap_or(0);
        }
    }
    format!(
        r#"
1 VERSIONINFO
FILEVERSION {},{},{},{}
PRODUCTVERSION {},{},{},{}
FILEFLAGSMASK 0x3fL
FILEFLAGS 0x0L
FILEOS 0x40004L
FILETYPE 0x1L
FILESUBTYPE 0x0L
BEGIN
  BLOCK "StringFileInfo"
  BEGIN
    BLOCK "040904b0"
    BEGIN
      VALUE "CompanyName", "Nuuvia contributors\0"
      VALUE "FileDescription", "{}\0"
      VALUE "FileVersion", "{}\0"
      VALUE "InternalName", "{}\0"
      VALUE "LegalCopyright", "MIT License\0"
      VALUE "OriginalFilename", "{}\0"
      VALUE "ProductName", "Nuuvia\0"
      VALUE "ProductVersion", "{}\0"
    END
  END
  BLOCK "VarFileInfo"
  BEGIN
    VALUE "Translation", 0x409, 1200
  END
END
"#,
        numbers[0],
        numbers[1],
        numbers[2],
        numbers[3],
        numbers[0],
        numbers[1],
        numbers[2],
        numbers[3],
        description,
        version,
        internal,
        original,
        version
    )
}

fn main() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }

    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let icon = manifest_dir.join("../../deploy/windows/nuvia.ico");
    let out = PathBuf::from(std::env::var("OUT_DIR").expect("OUT_DIR"));

    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed={}", icon.display());

    let icon_path = icon.to_string_lossy().replace('\\', "/");
    let rc = out.join("nuvia.rc");
    let rc_text = format!(
        "IDI_ICON1 ICON \"{icon_path}\"\n{}",
        version_info("Nuuvia CLI", "nuvia", "nuvia.exe")
    );
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
