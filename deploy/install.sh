#!/bin/bash
# SPDX-License-Identifier: EUPL-1.2
# Install/upgrade dongie on the dongle. Run as root from a directory containing:
#   dongie (aarch64 binary), dongie.service, dongie-update.service, sudoers-dongie,
#   50-dongie.rules, config.toml
set -euo pipefail
cd "$(dirname "$0")"
[ "$(id -u)" -eq 0 ] || { echo "run as root"; exit 1; }

id dongie >/dev/null 2>&1 || useradd --system --no-create-home --home-dir /nonexistent --shell /usr/sbin/nologin dongie

install -m 0755 -o root -g root dongie /usr/local/bin/dongie.new
mv /usr/local/bin/dongie.new /usr/local/bin/dongie

# sudoers: validate before activating
install -m 0440 -o root -g root sudoers-dongie /etc/sudoers.d/dongie.new
visudo -cf /etc/sudoers.d/dongie.new
mv /etc/sudoers.d/dongie.new /etc/sudoers.d/dongie

install -m 0644 -o root -g root 50-dongie.rules /etc/polkit-1/rules.d/50-dongie.rules

install -d -m 0750 -o root -g dongie /etc/dongie
[ -e /etc/dongie/config.toml ] || install -m 0640 -o root -g dongie config.toml /etc/dongie/config.toml
[ -e /etc/dongie/users ] || install -m 0640 -o root -g dongie /dev/null /etc/dongie/users
chgrp dongie /etc/dongie/users && chmod 0640 /etc/dongie/users

install -m 0644 -o root -g root dongie.service /etc/systemd/system/dongie.service
install -m 0644 -o root -g root dongie-update.service /etc/systemd/system/dongie-update.service
# let the dongie user change Tailscale prefs (exit node, accept routes) from the dashboard
command -v tailscale >/dev/null && tailscale set --operator=dongie || true

systemctl daemon-reload
systemctl enable dongie.service
systemctl restart dongie.service
echo "installed. users: $(dongie user list | wc -l) — add one with: sudo dongie user add <name>"
