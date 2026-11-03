#!/usr/bin/env bash
# Builds a BIOS-bootable ISO of the x86_64 kernel with GRUB as the
# Multiboot loader. Needs grub-mkrescue (grub-pc-bin), xorriso and mtools,
# which are Linux packages: CI runs this on Ubuntu and attaches the ISO as
# an artifact; on Windows run it from WSL.
#
#   tools/mkiso.sh [--release] [OUT.iso]
#
# The ISO boots in QEMU (-cdrom), VirtualBox, Hyper-V Generation 1 and on
# a BIOS-booted PC. Output goes to the VGA text screen and COM1.
set -euo pipefail

cd "$(dirname "$0")/.."

release=""
out="target/sprout-x86_64.iso"
for arg in "$@"; do
    case "$arg" in
        --release) release="--release" ;;
        *) out="$arg" ;;
    esac
done

kernel=$(cargo xtask build --arch x86_64 $release | tail -n 1)
stage=$(mktemp -d)
trap 'rm -rf "$stage"' EXIT

mkdir -p "$stage/boot/grub"
# Debug info is most of the file and GRUB loads none of it.
strip --strip-debug -o "$stage/boot/sprout-kernel" "$kernel"
cat > "$stage/boot/grub/grub.cfg" <<'EOF'
set timeout=0
set default=0
menuentry "Sprout" {
    multiboot /boot/sprout-kernel
    boot
}
EOF

grub-mkrescue -o "$out" "$stage" >/dev/null 2>&1
ls -l "$out"
