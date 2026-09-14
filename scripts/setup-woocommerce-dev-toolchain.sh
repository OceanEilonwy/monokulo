#!/usr/bin/env bash
# Minimal dev-toolchain setup for WBS Track 1.5 (the real WordPress/WooCommerce
# plugin) on an Arch-based system (written for CachyOS; plain Arch/Manjaro/etc.
# should work identically since it's all pacman + official-repo packages plus
# one upstream-documented .phar install).
#
# Installs: Docker (wp-env's own WordPress+MySQL sandbox runs in containers -
# nothing about WordPress/WooCommerce itself is installed on the host), Node.js
# + npm (to run @wordpress/env via npx - no global npm install here, see below),
# PHP CLI + Composer (for the plugin's own PHPUnit tests and dependency
# management, run directly on the host, outside Docker), and WP-CLI (not in
# Arch's official repos - installed via its own upstream-documented single-file
# .phar, not an AUR helper, to keep this script's own footprint small).
#
# Run as: sudo ./setup-woocommerce-dev-toolchain.sh
set -euo pipefail

if [[ $EUID -ne 0 ]]; then
    echo "run this with sudo (it installs packages and edits group membership): sudo $0" >&2
    exit 1
fi

echo "==> syncing package databases and installing docker, node, php, composer"
# `-Syu` (sync + full upgrade), not a bare `-Sy` - installing new packages
# against a stale local package database without upgrading everything else is
# Arch's classic "partial upgrade" foot-gun (a new package can pull in a
# library version newer than what's on the rest of the system already has).
#
# `--ignore lib32-libpcap`: multilib occasionally lags a few days behind core
# after a libpcap version bump, which makes a plain `-Syu` refuse outright
# ("breaks dependency 'libpcap=X' required by lib32-libpcap") - unrelated to
# anything this script installs. Skipping just that one package lets the rest
# of the upgrade proceed; a later plain `pacman -Syu` picks it back up
# automatically once multilib catches up, nothing to manually revert. Harmless
# to leave in even once that's resolved - `--ignore` on a package pacman has no
# actual reason to hold back is simply a no-op.
pacman -Syu --ignore libpcap --needed --noconfirm docker docker-compose nodejs npm php composer

echo "==> enabling and starting the docker service"
systemctl enable --now docker.service

# Let the user who invoked `sudo` run `docker`/`wp-env` without needing sudo for
# every single command afterward - the standard Arch docker post-install step.
target_user="${SUDO_USER:-}"
if [[ -n "$target_user" && "$target_user" != "root" ]]; then
    usermod -aG docker "$target_user"
    echo "==> added $target_user to the 'docker' group"
    echo "    log out and back in (or run 'newgrp docker' in this shell) for it to take effect"
else
    echo "==> skipped adding a user to the docker group (no SUDO_USER found - run this via 'sudo', not as root directly, if you want that step)"
fi

echo "==> installing wp-cli (not in Arch's official repos - upstream's own single-file .phar)"
curl -fsSL -o /usr/local/bin/wp https://raw.githubusercontent.com/wp-cli/builds/gh-pages/phar/wp-cli.phar
chmod +x /usr/local/bin/wp

echo
echo "==> done. installed versions:"
docker --version
php --version | head -1
composer --version
node --version
npm --version
wp --info | head -1

cat <<'EOF'

Next steps (no more sudo needed from here):
  - Log out/in (or `newgrp docker`) so your user's docker-group membership
    takes effect, then confirm with: docker run hello-world
  - wp-env itself needs no global install - from inside the plugin's own
    project directory, `npx @wordpress/env start` downloads and runs it
    on demand, spinning up WordPress + WooCommerce + MySQL in Docker.
EOF
