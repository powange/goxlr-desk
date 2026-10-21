# Builds the Windows installer (and the Linux packages) without GitHub Actions:
# see ci/local/build.sh. Windows binaries are cross-compiled for MSVC with cargo-xwin
# (clang + the MSVC CRT and Windows SDK it downloads), the installer is made with NSIS.
# Debian trixie: clang 19, which the current MSVC C++ headers require
FROM rust:1.98-trixie

RUN apt-get update && apt-get install -y --no-install-recommends \
        clang lld llvm \
        nsis \
        jq curl git unzip ca-certificates \
        pkg-config libdbus-1-dev libspeechd-dev libpulse-dev libclang-dev \
    && rm -rf /var/lib/apt/lists/*

RUN rustup target add x86_64-pc-windows-msvc aarch64-pc-windows-msvc \
    && cargo install --locked -j 4 cargo-xwin@0.23.1 cargo-deb@3.8.0 cargo-generate-rpm@0.21.1 \
    && rm -rf /usr/local/cargo/registry

# The MSVC CRT and Windows SDK are downloaded on first use, accepting their licence
ENV XWIN_ACCEPT_LICENSE=1 \
    XWIN_CACHE_DIR=/cache/xwin \
    CARGO_HOME=/cache/cargo

WORKDIR /src
