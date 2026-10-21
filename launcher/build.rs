use std::io::Error;

fn main() -> Result<(), Error> {
    windows_icon("../daemon/resources/goxlr-utility.ico", "goxlr-launcher")
}

/// Gives the Windows executable its icon. Decided by the target, not the system building it, so
/// a cross-compiled build (ci/local) gets it too; the resource file points at the icon by its
/// full path, which every resource compiler finds.
fn windows_icon(icon: &str, name: &str) -> Result<(), Error> {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return Ok(());
    }
    let manifest = std::env::var("CARGO_MANIFEST_DIR").map_err(Error::other)?;
    let out = std::env::var("OUT_DIR").map_err(Error::other)?;
    let icon = std::path::Path::new(&manifest).join(icon);
    println!("cargo:rerun-if-changed={}", icon.display());
    let rc = std::path::Path::new(&out).join(format!("{name}.rc"));
    let icon = icon.display().to_string().replace('\\', "/");
    std::fs::write(&rc, format!("id ICON \"{icon}\"\n"))?;
    embed_resource::compile(&rc, embed_resource::NONE)
        .manifest_optional()
        .map_err(Error::other)
}
