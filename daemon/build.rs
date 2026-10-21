use clap::CommandFactory;
use clap_complete::{Shell, generate_to};
use std::env;
use std::fs::File;
use std::io::Error;
use std::path::Path;

include!("src/cli.rs");

fn main() -> Result<(), Error> {
    windows_icon("resources/goxlr-utility.ico", "goxlr-daemon")?;

    let outdir = match env::var_os("OUT_DIR") {
        None => return Ok(()),
        Some(outdir) => outdir,
    };

    let mut app = Cli::command();
    for shell in Shell::value_variants() {
        let _ = generate_to(*shell, &mut app, "goxlr-daemon", &outdir)?;
    }

    let stamp_path = Path::new(&outdir).join("daemon-stamp");
    if let Err(err) = File::create(&stamp_path) {
        panic!("failed to write {}: {}", stamp_path.display(), err);
    }

    Ok(())
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
