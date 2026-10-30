# Development / build image for Perk.
#
# Contains everything needed to build, test, lint and package Perk for
# Linux (native) and Windows (cross-compiled with MinGW-w64).
# Nothing here is ever installed on the host machine.

ARG RUST_VERSION=1.88
FROM rust:${RUST_VERSION}-bookworm

ENV DEBIAN_FRONTEND=noninteractive

# Linux GUI/tray build deps (GTK3 + AppIndicator for the tray, libxdo for muda),
# MinGW-w64 for the Windows cross build, NSIS for the Windows installer.
RUN apt-get update && apt-get install -y --no-install-recommends \
        pkg-config \
        libgtk-3-dev \
        libxdo-dev \
        libayatana-appindicator3-dev \
        mingw-w64 \
        nsis \
        zip \
        file \
    && rm -rf /var/lib/apt/lists/*

RUN rustup target add x86_64-pc-windows-gnu \
    && rustup component add clippy rustfmt

# cargo-deb builds the Debian package.
RUN cargo install cargo-deb --locked --version 2.11.1 \
    && rm -rf /usr/local/cargo/registry

# Build output lives in a Docker volume, never in the (OneDrive-synced) host folder.
ENV CARGO_TARGET_DIR=/target \
    CARGO_TARGET_X86_64_PC_WINDOWS_GNU_LINKER=x86_64-w64-mingw32-gcc

WORKDIR /work
ENTRYPOINT ["/bin/bash", "/work/scripts/dev.sh"]
CMD ["help"]
