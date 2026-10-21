use std::io::Error;

fn main() -> Result<(), Error> {
    windows_icon(
        "../daemon/resources/goxlr-utility.ico",
        "goxlr-launcher",
        "GoXLR Utility Launcher",
    )
}

/// Gives the Windows executable its icon and version information. Decided by the target, not the system building it, so
/// a cross-compiled build (ci/local) gets it too; the resource file points at the icon by its
/// full path, which every resource compiler finds.
fn windows_icon(icon: &str, name: &str, description: &str) -> Result<(), Error> {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return Ok(());
    }
    let manifest = std::env::var("CARGO_MANIFEST_DIR").map_err(Error::other)?;
    let out = std::env::var("OUT_DIR").map_err(Error::other)?;
    let icon = std::path::Path::new(&manifest).join(icon);
    println!("cargo:rerun-if-changed={}", icon.display());
    let rc = std::path::Path::new(&out).join(format!("{name}.rc"));
    let icon = icon.display().to_string().replace('\\', "/");
    // The icon, and who made the program (its properties in Windows): an executable without
    // them looks suspicious to virus scanners' heuristics
    let version = std::env::var("CARGO_PKG_VERSION").map_err(Error::other)?;
    let mut numbers: Vec<String> = version
        .split(['.', '-'])
        .take(3)
        .map(|part| part.parse::<u16>().unwrap_or(0).to_string())
        .collect();
    numbers.resize(4, String::from("0"));
    let numbers = numbers.join(",");
    let file = format!("{name}.exe");
    std::fs::write(
        &rc,
        format!(
            "id ICON \"{icon}\"\n\
             1 VERSIONINFO\n\
             FILEVERSION {numbers}\n\
             PRODUCTVERSION {numbers}\n\
             FILEOS 0x40004\n\
             FILETYPE 0x1\n\
             BEGIN\n\
             BLOCK \"StringFileInfo\"\n\
             BEGIN\n\
             BLOCK \"040904b0\"\n\
             BEGIN\n\
             VALUE \"CompanyName\", \"The GoXLR on Linux Team\"\n\
             VALUE \"FileDescription\", \"{description}\"\n\
             VALUE \"FileVersion\", \"{version}\"\n\
             VALUE \"InternalName\", \"{name}\"\n\
             VALUE \"LegalCopyright\", \"MIT License\"\n\
             VALUE \"OriginalFilename\", \"{file}\"\n\
             VALUE \"ProductName\", \"GoXLR Utility\"\n\
             VALUE \"ProductVersion\", \"{version}\"\n\
             END\n\
             END\n\
             BLOCK \"VarFileInfo\"\n\
             BEGIN\n\
             VALUE \"Translation\", 0x409, 1200\n\
             END\n\
             END\n"
        ),
    )?;
    embed_resource::compile(&rc, embed_resource::NONE)
        .manifest_optional()
        .map_err(Error::other)
}
