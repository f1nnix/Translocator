# The Linux development environment for the GTK app (crates/translocator-gtk)
# and the rest of the workspace: Ubuntu 24.04 (GTK 4.14, libadwaita 1.5,
# GtkSourceView 5.12, GNOME 46), the packages CI installs, Rust stable, and
# what `dev.sh run` needs to start the app headless and take screenshots
# (Xvfb, D-Bus, xdotool, ImageMagick). Built and used by linux/dev.sh; the source is
# mounted at /src, the cargo registry and target dir live in named volumes.
FROM ubuntu:24.04

ENV DEBIAN_FRONTEND=noninteractive \
    LANG=C.UTF-8 \
    LC_ALL=C.UTF-8 \
    RUSTUP_HOME=/usr/local/rustup \
    CARGO_HOME=/usr/local/cargo \
    PATH=/usr/local/cargo/bin:$PATH

# The same packages CI installs, from the same script.
COPY scripts/linux-deps.sh /tmp/linux-deps.sh
RUN sh /tmp/linux-deps.sh build \
 && apt-get install -y --no-install-recommends \
        xvfb xauth xdotool imagemagick dbus dbus-x11 at-spi2-core \
        adwaita-icon-theme fonts-cantarell fonts-dejavu-core fonts-noto-core \
        openssh-client zip unzip procps \
 && rm -rf /var/lib/apt/lists/* /tmp/linux-deps.sh

RUN curl -sSf https://sh.rustup.rs | sh -s -- -y --no-modify-path --profile minimal \
        --default-toolchain stable --component clippy \
 && rustc --version && cargo --version \
 && rm -rf /usr/local/cargo/registry

WORKDIR /src
