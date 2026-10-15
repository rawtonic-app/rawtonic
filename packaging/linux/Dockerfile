# Release build environment for the Linux AppImage: Ubuntu 22.04 (glibc 2.35),
# so the AppImage runs on Ubuntu 22.04+, Debian 12+, Fedora and other current
# distributions. Usage: packaging/linux/build-appimage.sh (see packaging/README.md).
FROM ubuntu:22.04
ENV DEBIAN_FRONTEND=noninteractive
RUN apt-get update && apt-get install -y --no-install-recommends \
    build-essential curl wget file git ca-certificates pkg-config xz-utils python3 \
    libwebkit2gtk-4.1-dev libgtk-3-dev libssl-dev librsvg2-dev libayatana-appindicator3-dev \
    libxdo-dev patchelf cmake nasm clang libclang-dev xdg-utils desktop-file-utils \
    gstreamer1.0-plugins-base gstreamer1.0-plugins-good libgstreamer-plugins-base1.0-dev \
    squashfs-tools fuse libfuse2 \
 && rm -rf /var/lib/apt/lists/*
RUN curl -fsSL https://nodejs.org/dist/v24.10.0/node-v24.10.0-linux-x64.tar.xz | tar xJ -C /opt \
 && ln -s /opt/node-v24.10.0-linux-x64/bin/* /usr/local/bin/
ARG UID=1000
RUN useradd -m -u $UID builder
USER builder
ENV PATH=/home/builder/.cargo/bin:$PATH
RUN curl -fsSL https://sh.rustup.rs | sh -s -- -y --profile minimal --default-toolchain 1.98.1
WORKDIR /src
