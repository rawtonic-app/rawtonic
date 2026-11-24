#!/bin/bash
# Runs inside the container started by test-in-container.sh.
set -uo pipefail
log() { echo "== $*" | tee -a /out/summary.txt; }
: > /out/summary.txt
failed=0
fail() { log "FAIL $*"; failed=1; }
pacman -Syu --noconfirm --needed base-devel sudo namcap >/out/pacman-base.log 2>&1 || { log "FAIL base packages"; exit 1; }
useradd -m builder
echo 'builder ALL=(ALL) NOPASSWD: ALL' > /etc/sudoers.d/builder
mkdir /home/builder/pkg && cp /pkg/PKGBUILD /home/builder/pkg/
# The AppImage comes from the cache when it is there (makepkg skips the download).
src=$(sed -n 's/^source=("\([^:]*\)::.*/\1/p' /pkg/PKGBUILD); src=$(bash -c "$(sed -n '/^pkgver=/p;/^_tag=/p' /pkg/PKGBUILD); echo Rawtonic_\${_tag#v}_amd64.AppImage")
[ -f "/cache/$src" ] && cp "/cache/$src" /home/builder/pkg/
chown -R builder /home/builder/pkg
cd /home/builder/pkg
log "vercmp (expect -1, -1, 1)"
vercmp 0.1.0alpha.19 0.1.0 | tee -a /out/summary.txt
vercmp 0.1.0alpha.19 0.1.0alpha.20 | tee -a /out/summary.txt
vercmp 0.1.0alpha.19 0.1.0alpha.17.1 | tee -a /out/summary.txt
log "makepkg -s (declared dependencies only)"
sudo -u builder makepkg -s --noconfirm >/out/makepkg.log 2>&1 || { log "FAIL makepkg"; tail -30 /out/makepkg.log; exit 1; }
cp -n "$src" /cache/ 2>/dev/null || true
pkg=$(ls /home/builder/pkg/*.pkg.tar.zst)
ls -la "$pkg" | tee -a /out/summary.txt
cp "$pkg" /out/
log "namcap"; namcap PKGBUILD "$pkg" > /out/namcap.log 2>&1; grep -v 'lacks FULL RELRO' /out/namcap.log | head -40 | tee -a /out/summary.txt; echo "(plus $(grep -c 'lacks FULL RELRO' /out/namcap.log) RELRO notes for prebuilt libraries)" | tee -a /out/summary.txt
log "pacman -U"
pacman -U --noconfirm "$pkg" >/out/install.log 2>&1 || { log "FAIL install"; tail /out/install.log; exit 1; }
pacman -Qi rawtonic-bin | grep -E "^(Name|Version|Installed Size|Depends On)" | tee -a /out/summary.txt
log "missing shared libraries with only the declared dependencies installed"
export LD_LIBRARY_PATH=/opt/rawtonic/usr/lib
missing=0
while IFS= read -r f; do
  if file -b "$f" | grep -q '^ELF'; then
    ldd "$f" 2>/dev/null | grep 'not found' | sed "s|^|$f: |"
  fi
done < <(find /opt/rawtonic -type f \( -name '*.so*' -o -path '*/bin/*' -o -name AppRun.wrapped \)) | sort -u | tee /out/missing-libs.txt | head -40 | tee -a /out/summary.txt
unset LD_LIBRARY_PATH
log "test tools (a stand-in for a desktop: Xvfb, software Vulkan)"
pacman -S --noconfirm --needed xorg-server-xvfb xorg-xauth vulkan-swrast xorg-xwd >/out/pacman-tools.log 2>&1 || log "FAIL tools"
useradd -m tester
photo=$(ls /in/* | grep -v SHA256SUMS | head -1)
(cd /in && sha256sum "$(basename "$photo")") > /out/in.sha256
log "headless export of $(basename "$photo")"
sudo -u tester env HOME=/home/tester timeout 600 xvfb-run -a rawtonic export "$photo" --output /home/tester/out.jpg --format jpeg >/out/export.log 2>&1
code=$?
size=$(stat -c %s /home/tester/out.jpg 2>/dev/null || echo 0)
magic=$(head -c 2 /home/tester/out.jpg 2>/dev/null | od -An -tx1 | tr -d ' ')
log "export exit $code, $size bytes, magic $magic"
[ "$code" -eq 0 ] && [ "$size" -gt 100000 ] && [ "$magic" = ffd8 ] || fail "export"
cp /home/tester/out.jpg /out/ 2>/dev/null
log "headless export with relative paths from the home folder (informational for releases up to alpha.19)"
cp "$photo" /home/tester/rel.raw; chown tester /home/tester/rel.raw
(cd /home/tester && sudo -u tester env HOME=/home/tester timeout 600 xvfb-run -a rawtonic export rel.raw --output rel-out.jpg --format jpeg >/out/export-relative.log 2>&1; echo "exit $?, $(stat -c %s rel-out.jpg 2>/dev/null || echo 0) bytes") | tee -a /out/summary.txt
(cd /in && sha256sum -c --quiet /out/in.sha256) && log "original unchanged" || fail "original changed"
tail -5 /out/export.log | tee -a /out/summary.txt
log "GUI start (30 s under Xvfb, then screenshot, then close)"
cat > /home/tester/gui.sh <<'GUI'
# Xvfb has no DRI3: WebKit paints in software, as in packaging/README.md's earlier Xvfb checks.
WEBKIT_DISABLE_COMPOSITING_MODE=1 rawtonic >/home/tester/gui.log 2>&1 &
app=$!
sleep 30
kill -0 $app 2>/dev/null && echo "app alive: yes" || echo "FAIL app alive: no"
ps -eo args | grep -c -E '[W]ebKit(Web|Network)Process' | sed 's/^/webkit helper processes: /'
xwd -root -silent -out /home/tester/gui.xwd
kill $app 2>/dev/null; wait $app 2>/dev/null
echo "exit $?"
GUI
sudo -u tester env HOME=/home/tester xvfb-run -a -s "-screen 0 1440x900x24" bash /home/tester/gui.sh 2>&1 | tee -a /out/summary.txt
cp /home/tester/gui.log /home/tester/gui.xwd /out/ 2>/dev/null
log "remove the package"
pacman -Rns --noconfirm rawtonic-bin >/out/remove.log 2>&1 || log "FAIL remove"
left=$(find /opt/rawtonic /usr/bin/rawtonic /usr/share/applications/rawtonic.desktop /usr/share/licenses/rawtonic-bin 2>/dev/null | wc -l)
log "files left from the package after removal: $left"
[ "$left" -eq 0 ] || fail "files left"
if pacman -Q rawtonic-bin >/dev/null 2>&1; then fail "package still installed"; fi
grep -q "^FAIL\|^== FAIL" /out/summary.txt && failed=1
log "result: $([ $failed -eq 0 ] && echo PASS || echo FAILED)"
exit $failed
