# Linux build, tests and windowed runs of gpui-kit-grid (`docker/linux/*.sh`).
# On Apple Silicon the image is arm64 (aarch64-unknown-linux-gnu), no
# emulation. There is no GPU: Mesa draws on the CPU (llvmpipe, lavapipe).
#
# The lines up to `WORKDIR /src` are a generic GPUI build image. Keep them
# unchanged, so Docker reuses their layers for other GPUI projects built
# from the same lines; everything specific to this repository comes after.
FROM rust:1.95-bookworm

RUN apt-get update && DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends \
      pkg-config clang libclang-dev cmake ninja-build python3 curl ca-certificates git \
      libxkbcommon-dev libxkbcommon-x11-dev libwayland-dev libx11-dev libx11-xcb-dev \
      libxcb1-dev libxcb-xkb-dev libxcb-randr0-dev libxcb-xfixes0-dev libxcb-shape0-dev \
      libxcursor-dev libxi-dev libxrandr-dev libxext-dev libxfixes-dev \
      libvulkan-dev libfontconfig-dev libfreetype-dev libssl-dev libasound2-dev \
      libzstd-dev libglib2.0-dev libdbus-1-dev libpipewire-0.3-dev libgbm-dev libdrm-dev \
      libegl-dev libgl-dev libgles-dev libegl1-mesa-dev libgl1-mesa-dri \
      mesa-vulkan-drivers mesa-utils vulkan-tools \
      xvfb x11-utils xdotool imagemagick fonts-dejavu-core \
    && rm -rf /var/lib/apt/lists/*

# The version of rust-toolchain.toml.
RUN rustup toolchain install 1.95.0 --profile minimal && rustup default 1.95.0
RUN rustup component add --toolchain 1.95.0 clippy

ENV CARGO_TARGET_DIR=/target \
    LIBGL_ALWAYS_SOFTWARE=1
WORKDIR /src

# gpui-kit-grid only from here on.
# rustfmt: rust-toolchain.toml lists it, so rustup would fetch it on first use.
RUN rustup component add --toolchain 1.95.0 rustfmt
# xclip reads the X clipboard for the copy scenario (script/e2e).
# mold replaces the system `ld`, as `rui314/setup-mold` does in CI: GNU ld
# linking a dozen test binaries at once runs the Docker VM out of memory
# (`ld terminated with signal 9`). The rustc flags stay the same, so a
# switch does not rebuild anything.
RUN apt-get update && DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends \
      xclip mold \
    && rm -rf /var/lib/apt/lists/* \
    && ln -sf /usr/bin/mold "$(realpath /usr/bin/ld)"

WORKDIR /work/gpui-kit-grid
