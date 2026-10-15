#!/usr/bin/env bash
# Runs on the Pi as root. Safe to run more than once.
set -euo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"

apt-get update
apt-get install -y --no-install-recommends \
    libasound2t64 libinput10 libudev1 libxkbcommon0 libgbm1 libegl1 libgles2 libdrm2 libfontconfig1 \
    libegl-mesa0 libgl1-mesa-dri \
    bluez pipewire wireplumber libspa-0.2-bluetooth \
    curl bzip2 ca-certificates

# A player set up before the rename to Ziggy: stop its old service, then rename the service
# user, its home, the data and the config. Nothing is lost.
if [ -d /var/lib/muzak ] && [ ! -d /var/lib/ziggy ]; then
    systemctl disable --now muzak-player.service 2>/dev/null || true
    rm -f /etc/systemd/system/muzak-player.service /usr/local/bin/muzak-player \
        /etc/udev/rules.d/90-muzak-backlight.rules /etc/systemd/journald.conf.d/muzak.conf
    if id muzak >/dev/null 2>&1 && ! id ziggy-player >/dev/null 2>&1; then
        groupmod -n ziggy-player muzak
        usermod -l ziggy-player -d /home/ziggy-player -m muzak
    fi
    mv /var/lib/muzak /var/lib/ziggy
    if [ -d /etc/muzak ]; then
        mv /etc/muzak /etc/ziggy
        sed -i 's|/var/lib/muzak|/var/lib/ziggy|g; s|/etc/muzak|/etc/ziggy|g; s|Muzak device|Ziggy device|; s|`muzak |`ziggy |g' /etc/ziggy/config.toml
    fi
fi

id ziggy-player >/dev/null 2>&1 || useradd --system --create-home --shell /usr/sbin/nologin --groups video,input,audio,render,bluetooth ziggy-player
install -d -m 700 -o ziggy-player -g ziggy-player /var/lib/ziggy /var/lib/ziggy/models

# Sound: Spotify Soloist plays through PipeWire, which runs in the ziggy-player user's own session,
# started at boot. BlueALSA would claim Bluetooth speakers first, so it goes. WirePlumber normally
# offers Bluetooth only to someone logged in at the screen; this Pi has no such person.
if dpkg -s bluez-alsa-utils >/dev/null 2>&1; then
    systemctl disable --now bluealsa.service 2>/dev/null || true
    apt-get purge -y bluez-alsa-utils libasound2-plugin-bluez || true
fi
mkdir -p /etc/wireplumber/wireplumber.conf.d
rm -f /etc/wireplumber/wireplumber.conf.d/80-ziggy-bluetooth.conf
cat > /etc/wireplumber/wireplumber.conf.d/80-ziggy.conf <<'EOW'
# Ziggy: Bluetooth speakers work without anyone logged in.
wireplumber.profiles = {
  main = {
    monitor.bluez.seat-monitoring = disabled
  }
}
# Ziggy sets the volume itself; a speaker or the jack starts at full volume, not 40%.
wireplumber.settings = {
  device.routes.default-sink-volume = 1.0
}
# Music only: no phone-call (hands-free) mode, which is mono and tinny. A speaker with a
# microphone could otherwise fall back to it when music mode fails to connect once.
monitor.bluez.properties = {
  bluez5.roles = [ a2dp_sink a2dp_source ]
  bluez5.hfphsp-backend = "none"
}
EOW
# PipeWire runs for the player only: not for whoever logs in over SSH, which would start a second
# copy competing for the speaker.
systemctl --global disable pipewire.socket pipewire.service wireplumber.service \
    pipewire-pulse.socket 2>/dev/null || true
loginctl enable-linger ziggy-player
systemctl --user -M ziggy-player@ enable pipewire.socket wireplumber.service 2>/dev/null || true
# Already running from an earlier setup: pick up the settings.
systemctl --user -M ziggy-player@ restart wireplumber.service 2>/dev/null || true
install -d /etc/ziggy

# Display and boot settings; they need a reboot, so say when anything changed.
CONFIG=/boot/firmware/config.txt
CMDLINE=/boot/firmware/cmdline.txt
BEFORE=$(cat "$CONFIG" "$CMDLINE" | md5sum)
grep -q '^dtoverlay=vc4-kms-dsi-7inch' "$CONFIG" || echo 'dtoverlay=vc4-kms-dsi-7inch' >> "$CONFIG"
grep -q '^dtparam=audio=on' "$CONFIG" || echo 'dtparam=audio=on' >> "$CONFIG"
# Boot faster: no rainbow splash, no boot delay.
grep -q '^disable_splash=1' "$CONFIG" || echo 'disable_splash=1' >> "$CONFIG"
grep -q '^boot_delay=0' "$CONFIG" || echo 'boot_delay=0' >> "$CONFIG"
grep -q 'vt.global_cursor_default=0' "$CMDLINE" || sed -i '1 s/$/ vt.global_cursor_default=0 consoleblank=0/' "$CMDLINE"
grep -q 'loglevel=3' "$CMDLINE" || sed -i '1 s/$/ quiet loglevel=3/' "$CMDLINE"
[ "$(cat "$CONFIG" "$CMDLINE" | md5sum)" = "$BEFORE" ] || echo ZIGGY_REBOOT_NEEDED

cat > /etc/udev/rules.d/90-ziggy-backlight.rules <<'EOR'
SUBSYSTEM=="backlight", ACTION=="add", RUN+="/bin/chgrp video /sys%p/brightness", RUN+="/bin/chmod g+w /sys%p/brightness"
EOR

# Keep the logs across reboots (capped), so a crash can still be read afterwards.
mkdir -p /etc/systemd/journald.conf.d /var/log/journal
printf '[Journal]\nStorage=persistent\nSystemMaxUse=50M\n' > /etc/systemd/journald.conf.d/ziggy.conf
systemctl restart systemd-journald 2>/dev/null || true
journalctl --flush 2>/dev/null || true

install -m 644 "$HERE/ziggy-player.service" /etc/systemd/system/ziggy-player.service
systemctl daemon-reload
# Voice: make a USB microphone the default recording device. The Pi's own default device
# (the headphone jack) can't record. Playback stays on the headphone jack by default; the
# player names its Bluetooth or headphone output itself. An asound.conf someone else wrote
# is left alone.
MIC=$(arecord -l 2>/dev/null | sed -n 's/^card [0-9]*: \([^ ]*\) \[.*USB.*/\1/p' | head -1)
if [ -n "$MIC" ] && { [ ! -e /etc/asound.conf ] || grep -q '^# Ziggy' /etc/asound.conf; }; then
    cat > /etc/asound.conf <<EOA
# Ziggy: play through the headphone jack by default, record from the USB microphone.
pcm.!default {
    type asym
    playback.pcm "plughw:CARD=Headphones"
    capture.pcm "plughw:CARD=$MIC"
}
ctl.!default {
    type hw
    card Headphones
}
EOA
fi
# Many USB microphones start with their gain at zero, so the wake word needs shouting. 75%
# (12 of 16 on a common one) hears ordinary speech across a room. Saved for the next boot.
if [ -n "$MIC" ]; then
    for control in Mic Capture; do
        amixer -q -c "$MIC" sset "$control" 75% cap 2>/dev/null && break
    done
    alsactl store 2>/dev/null || true
fi

# Raspberry Pi OS Lite starts with the Bluetooth radio blocked; the player needs it on.
rfkill unblock bluetooth 2>/dev/null || true
systemctl enable ziggy-player.service
systemctl disable getty@tty1.service || true
# Nothing waits for the network at boot, and package-list and manual-page jobs don't run
# while the player starts. (Updates come through `ziggy update`.)
for unit in NetworkManager-wait-online.service systemd-networkd-wait-online.service \
    apt-daily.timer apt-daily-upgrade.timer man-db.timer; do
    systemctl disable "$unit" 2>/dev/null || true
done

echo "Provisioned. Reboot to apply display settings: sudo reboot"
