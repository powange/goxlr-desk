use std::io::Error;

fn main() -> Result<(), Error> {
    windows_icon(
        "../daemon/resources/goxlr-desk.ico",
        "goxlr-defaults",
        "GoXLR Desk default files",
    )
}

include!("../ci/windows_resource.rs");
