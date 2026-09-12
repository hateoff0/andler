# syntax=docker/dockerfile:1

ARG ANDROID_MAJOR=13
ARG ANDROID_VARIANT=VANILLA

FROM archlinux:base

ARG ANDROID_MAJOR
ARG ANDROID_VARIANT

RUN sed -i '/^\[options\]/a DisableSandboxNetwork\nParallelDownloads = 5' /etc/pacman.conf

RUN pacman-key --init && \
    pacman -Sy --noconfirm --needed archlinux-keyring && \
    pacman-key --populate archlinux && \
    pacman -Syu --noconfirm

# curl/tar/gnupg needed for repo setup; left installed (useful for debugging)
RUN pacman -S --noconfirm --needed curl tar gnupg

# CachyOS repo (linux-cachyos kernel, BORE scheduler)
RUN curl -fsSL --retry 3 --retry-delay 5 https://mirror.cachyos.org/cachyos-repo.tar.xz -o /tmp/cachyos-repo.tar.xz && \
    tar -xf /tmp/cachyos-repo.tar.xz -C /tmp && \
    cd /tmp/cachyos-repo && yes | ./cachyos-repo.sh && \
    cd / && rm -rf /tmp/cachyos-repo /tmp/cachyos-repo.tar.xz && \
    yes | pacman -Scc --noconfirm

# Chaotic-AUR repo (pre-built waydroid from AUR)
#
# Two failure modes are handled here, because anything that fails at this point
# wastes the kernel + waydroid install that follows:
#   * the CDN (cdn-mirror.chaotic.cx) answers 503 while its regional mirrors
#     serve the same files, so the keyring/mirrorlist come from the first
#     mirror that answers instead of one hardcoded host;
#   * if no mirror answers at all, the build continues without the repo. Every
#     package installed below resolves from the official and cachyos repos
#     (`waydroid` itself is in `extra` now), so an outage degrades the image
#     rather than aborting a 20-minute build — and a package that later needs
#     chaotic-aur still fails loudly with "target not found".
# `${base}` is a shell variable, not a build ARG: there is no matching ARG/ENV,
# so buildx passes it through to the shell untouched (checked against buildx,
# which build.sh requires). Escaping it as \${base} would hand the shell the
# literal string and every URL would be wrong.
RUN set -e; \
    for base in \
        https://geo-mirror.chaotic.cx \
        https://cdn-mirror.chaotic.cx \
        https://us-ca-mirror.chaotic.cx \
        https://sg-mirror.chaotic.cx \
        https://de-mirror.chaotic.cx ; do \
      echo "==> chaotic-aur bootstrap from ${base}"; \
      if curl -fsSL --connect-timeout 15 --retry 3 --retry-delay 5 \
             -o /tmp/chaotic-keyring.pkg.tar.zst "${base}/chaotic-aur/chaotic-keyring.pkg.tar.zst" && \
         curl -fsSL --connect-timeout 15 --retry 3 --retry-delay 5 \
             -o /tmp/chaotic-mirrorlist.pkg.tar.zst "${base}/chaotic-aur/chaotic-mirrorlist.pkg.tar.zst"; then \
        break; \
      fi; \
    done; \
    if [ -s /tmp/chaotic-keyring.pkg.tar.zst ] && [ -s /tmp/chaotic-mirrorlist.pkg.tar.zst ]; then \
      pacman-key --recv-key 3056513887B78AEB --keyserver keyserver.ubuntu.com && \
      pacman-key --lsign-key 3056513887B78AEB && \
      pacman -U --noconfirm /tmp/chaotic-keyring.pkg.tar.zst /tmp/chaotic-mirrorlist.pkg.tar.zst && \
      printf '\n[chaotic-aur]\nInclude = /etc/pacman.d/chaotic-mirrorlist\n' >> /etc/pacman.conf; \
    else \
      echo "WARNING: no chaotic-aur mirror answered; building without chaotic-aur"; \
    fi; \
    rm -f /tmp/chaotic-*.pkg.tar.zst; \
    pacman -Sy --noconfirm

# Overlay configs BEFORE kernel install — mkinitcpio and UKI hook must exist before pacman -S linux-cachyos
COPY rootfs/ /
RUN chmod +x /usr/local/bin/andler-build-uki /usr/local/bin/andler-waydroid-compositor /usr/local/bin/andler-waydroid-session

# Enable en_US.UTF-8 locale (archlinux:base has none by default)
RUN sed -i 's/^#en_US.UTF-8 UTF-8/en_US.UTF-8 UTF-8/' /etc/locale.gen && \
    locale-gen && \
    echo "LANG=en_US.UTF-8" > /etc/locale.conf

# Pre-seed everything systemd-firstboot checks for (timezone, hostname — locale is
# already set above); systemd-firstboot.service itself is masked further down, once
# systemd is installed. This is a prebaked appliance image: it must never block boot
# on an interactive first-boot wizard (Initial Setup), which would otherwise sit on
# the serial console waiting for input nobody sends, forever, with the graphical
# session (weston/waydroid) never even starting since multi-user.target is never
# reached.
RUN ln -sf /usr/share/zoneinfo/UTC /etc/localtime && \
    echo "cachyos" > /etc/hostname

# -Syyu (force refresh, not just -Syu) right before the actual install: lines 13-35
# above sync package databases too, but those RUN layers get cached indefinitely by
# Docker — on a rolling-release repo like cachyos-extra-v3/chaotic-aur, the CDN
# regularly prunes old package builds, so a stale cached sync here can hand out
# filenames that no longer exist upstream (seen as random 404s on unrelated-looking
# packages). Refreshing right before downloading avoids relying on how old the
# earlier cached layers happen to be.
RUN pacman -Syyu --noconfirm && \
    pacman -S --noconfirm --needed \
        base linux-cachyos \
        systemd systemd-ukify \
        sudo nano less python \
        openssh \
        dbus \
        mesa vulkan-icd-loader vulkan-virtio \
        iproute2 iputils \
        pipewire pipewire-pulse pipewire-alsa rtkit \
        weston waydroid qemu-guest-agent \
    && yes | pacman -Scc --noconfirm

RUN --mount=type=cache,target=/var/cache/andler-fetch \
    python3 /usr/local/lib/andler/fetch-waydroid-images.py \
        --android-major="${ANDROID_MAJOR}" \
        --variant="${ANDROID_VARIANT}" \
        --cache-dir=/var/cache/andler-fetch

# Dedicated system user for the compositor session (Wayland compositors
# misbehave/refuse to run as root)
RUN groupadd -f video && groupadd -f render && groupadd -f input && groupadd -f seat && \
    useradd --system --create-home \
        --groups video,render,input,seat \
        --shell /usr/bin/nologin \
        user

RUN ln -sf /usr/lib/systemd/system/multi-user.target /etc/systemd/system/default.target && \
    systemctl enable systemd-networkd systemd-resolved && \
    systemctl enable waydroid-container.service && \
    systemctl enable andler-pipewire.service && \
    systemctl enable andler-pipewire-pulse.service && \
    systemctl enable waydroid-compositor.service && \
    systemctl mask systemd-firstboot.service
    # sshd intentionally disabled — see autologin.conf for console access model.

# Verify andler-build-uki runs successfully during docker build (catches errors early)
RUN /usr/local/bin/andler-build-uki

LABEL org.andler.image.kind="linux-waydroid-base" \
      org.andler.image.android_major="${ANDROID_MAJOR}" \
      org.andler.image.android_variant="${ANDROID_VARIANT}" \
      org.andler.image.kernel="linux-cachyos"

CMD ["/usr/bin/true"]
