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

# Chaotic-AUR repo (pre-built waydroid, gamescope from AUR)
RUN pacman-key --recv-key 3056513887B78AEB --keyserver keyserver.ubuntu.com && \
    pacman-key --lsign-key 3056513887B78AEB && \
    pacman -U --noconfirm \
        'https://cdn-mirror.chaotic.cx/chaotic-aur/chaotic-keyring.pkg.tar.zst' \
        'https://cdn-mirror.chaotic.cx/chaotic-aur/chaotic-mirrorlist.pkg.tar.zst' && \
    printf '\n[chaotic-aur]\nInclude = /etc/pacman.d/chaotic-mirrorlist\n' >> /etc/pacman.conf && \
    pacman -Sy --noconfirm

# Overlay configs BEFORE kernel install — mkinitcpio and UKI hook must exist before pacman -S linux-cachyos
COPY rootfs/ /
RUN chmod +x /usr/local/bin/andler-build-uki

# Enable en_US.UTF-8 locale (archlinux:base has none by default)
RUN sed -i 's/^#en_US.UTF-8 UTF-8/en_US.UTF-8 UTF-8/' /etc/locale.gen && \
    locale-gen && \
    echo "LANG=en_US.UTF-8" > /etc/locale.conf

# Pre-seed everything systemd-firstboot checks for (timezone, hostname — locale is
# already set above); systemd-firstboot.service itself is masked further down, once
# systemd is installed. This is a prebaked appliance image: it must never block boot
# on an interactive first-boot wizard (Initial Setup), which would otherwise sit on
# the serial console waiting for input nobody sends, forever, with the graphical
# session (gamescope/waydroid) never even starting since multi-user.target is never
# reached.
RUN ln -sf /usr/share/zoneinfo/UTC /etc/localtime && \
    echo "cachyos" > /etc/hostname

RUN pacman -S --noconfirm --needed \
        base linux-cachyos \
        systemd systemd-ukify \
        sudo nano less python \
        openssh \
        mesa vulkan-icd-loader vulkan-virtio \
        iproute2 iputils \
        gamescope waydroid \
    && yes | pacman -Scc --noconfirm

RUN python3 /usr/local/lib/andler/fetch-waydroid-images.py \
        --android-major="${ANDROID_MAJOR}" \
        --variant="${ANDROID_VARIANT}"

# Dedicated system user for gamescope session (gamescope/wlroots misbehave under root)
RUN groupadd -f video && groupadd -f render && groupadd -f input && groupadd -f seat && \
    useradd --system --create-home \
        --groups video,render,input,seat \
        --shell /usr/bin/nologin \
        user

RUN ln -sf /usr/lib/systemd/system/multi-user.target /etc/systemd/system/default.target && \
    systemctl enable systemd-networkd systemd-resolved && \
    systemctl enable waydroid-container.service && \
    systemctl enable gamescope-waydroid.service && \
    systemctl mask systemd-firstboot.service
    # sshd intentionally disabled — see autologin.conf for console access model.

# Verify andler-build-uki runs successfully during docker build (catches errors early)
RUN /usr/local/bin/andler-build-uki

LABEL org.andler.image.kind="linux-waydroid-base" \
      org.andler.image.android_major="${ANDROID_MAJOR}" \
      org.andler.image.android_variant="${ANDROID_VARIANT}" \
      org.andler.image.kernel="linux-cachyos"

CMD ["/usr/bin/true"]
