#!/bin/bash
# Runs inside the container started by test-in-container.sh.
set -uo pipefail
log() { echo "== $*" | tee -a /out/summary.txt; }
: > /out/summary.txt
pacman -Syu --noconfirm --needed flatpak flatpak-builder xorg-server-xvfb xorg-xauth xorg-xwd dbus sudo \
  mesa vulkan-swrast >/out/pacman.log 2>&1 || { log "FAIL packages"; exit 1; }
useradd -m tester
mkdir -p /cache/flatpak /home/tester/.local/share
ln -s /cache/flatpak /home/tester/.local/share/flatpak
chown -R tester /cache/flatpak /home/tester
cp -r /pkg /home/tester/pkg; chown -R tester /home/tester/pkg
as() { sudo -u tester env HOME=/home/tester XDG_RUNTIME_DIR=/tmp/xdg dbus-run-session -- "$@"; }
mkdir -p /tmp/xdg && chown tester /tmp/xdg && chmod 700 /tmp/xdg
cd /home/tester/pkg/flatpak
log "flatpak-builder"
as bash -c 'flatpak remote-add --user --if-not-exists flathub https://dl.flathub.org/repo/flathub.flatpakrepo &&
  flatpak-builder --user --install-deps-from=flathub --force-clean --install --disable-rofiles-fuse -y /home/tester/build cc.rawtonic.editor.yml' \
  >/out/builder.log 2>&1 || { log "FAIL builder"; tail -30 /out/builder.log; exit 1; }
tail -3 /out/builder.log | tee -a /out/summary.txt
as flatpak info --user cc.rawtonic.editor | tee -a /out/summary.txt
as flatpak info --user --show-permissions cc.rawtonic.editor | tee /out/permissions.txt >>/out/summary.txt
photo=$(ls /in/* | grep -v SHA256SUMS | head -1)
(cd /in && sha256sum "$(basename "$photo")") > /out/in.sha256
mkdir -p /home/tester/o && chown tester /home/tester/o
log "headless export of $(basename "$photo") through the sandbox (the file chooser portal is not involved: --filesystem grants it)"
as xvfb-run -a flatpak run --user --filesystem=/in:ro --filesystem=/home/tester/o cc.rawtonic.editor export "$photo" --output /home/tester/o/out.jpg --format jpeg >/out/export.log 2>&1
code=$?
size=$(stat -c %s /home/tester/o/out.jpg 2>/dev/null || echo 0)
magic=$(head -c 2 /home/tester/o/out.jpg 2>/dev/null | od -An -tx1 | tr -d ' ')
log "export exit $code, $size bytes, magic $magic"
cp /home/tester/o/out.jpg /out/ 2>/dev/null
(cd /in && sha256sum -c --quiet /out/in.sha256) && log "original unchanged"
tail -4 /out/export.log | tee -a /out/summary.txt
log "GUI start (30 s under Xvfb)"
cat > /home/tester/gui.sh <<'GUI'
flatpak run --user --env=WEBKIT_DISABLE_COMPOSITING_MODE=1 cc.rawtonic.editor >/home/tester/gui.log 2>&1 &
sleep 30
echo "flatpak instances: $(flatpak ps --columns=application | grep -c cc.rawtonic.editor)"
ps -eo args | grep -c '[W]ebKitWebProcess' | sed 's/^/webkit web processes: /'
flatpak kill cc.rawtonic.editor
sleep 3
echo "instances after kill: $(flatpak ps --columns=application | grep -c cc.rawtonic.editor)"
GUI
as xvfb-run -a bash /home/tester/gui.sh 2>&1 | tee -a /out/summary.txt
cp /home/tester/gui.log /out/ 2>/dev/null
log "uninstall"
as flatpak uninstall --user -y cc.rawtonic.editor >/out/uninstall.log 2>&1 || log "FAIL uninstall"
as flatpak list --user --app | tee -a /out/summary.txt
