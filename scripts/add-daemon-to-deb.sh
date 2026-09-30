#!/usr/bin/env bash
# First-run E — put the `aivyx-pa` daemon into the desktop app's `.deb`.
#
# cargo-bundle packages only the `aivyx-desktop` shell, but the shell runs
# `aivyx-pa daemon run`: a user who installed just the `.deb` had no daemon.
# This adds the binary as /usr/bin/aivyx-pa (beside /usr/bin/aivyx-desktop,
# which the shell prefers, so their versions match), updates md5sums and
# Installed-Size, and rewrites the package in place. Uses only `ar` and
# `tar`, so it runs on the Ubuntu release runner and on any dev machine.
#
# usage: scripts/add-daemon-to-deb.sh <package.deb> <aivyx-pa binary>
set -euo pipefail

deb=$(realpath "$1")
bin=$(realpath "$2")
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
cd "$work"

ar x "$deb"
data=$(ls data.tar.*)
control=$(ls control.tar.*)
compress_flag() {
    case "$1" in
        *.gz) echo -z ;;
        *.xz) echo -J ;;
        *.zst) echo --zstd ;;
        *) echo "unsupported archive: $1" >&2; exit 1 ;;
    esac
}

mkdir data control
tar -xf "$data" -C data
tar -xf "$control" -C control

install -D -m 0755 "$bin" data/usr/bin/aivyx-pa

# md5sums lists every file; drop any old entry for the daemon first.
if [ -f control/md5sums ]; then
    grep -v '  usr/bin/aivyx-pa$' control/md5sums > control/md5sums.new || true
    (cd data && md5sum usr/bin/aivyx-pa) >> control/md5sums.new
    mv control/md5sums.new control/md5sums
fi
size=$(du -sk --apparent-size data | cut -f1)
sed -i "s/^Installed-Size: .*/Installed-Size: $size/" control/control

rm "$data" "$control"
tar -c "$(compress_flag "$data")" --owner=0 --group=0 -f "$data" -C data .
tar -c "$(compress_flag "$control")" --owner=0 --group=0 -f "$control" -C control .
# debian-binary must come first in the archive.
rm -f "$deb"
ar rc "$deb" debian-binary "$control" "$data"
echo "added aivyx-pa to $deb"
