use std::io::Error;

fn main() -> Result<(), Error> {
    windows_icon(
        "../daemon/resources/goxlr-desk.ico",
        "goxlr-launcher",
        "GoXLR Desk Launcher",
    )
}

include!("../ci/windows_resource.rs");
