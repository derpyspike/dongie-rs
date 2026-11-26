# Development

```
src/main.rs      CLI (serve, user …), startup
src/web.rs       HTTP routes, auth, CSRF, actions, both listeners
src/status.rs    collector: modem, cells, hotspot, Tailscale, system, flash, services, LEDs
src/history.rs   24 h ring buffer, bucketed queries, band histogram
src/sms.rs       ModemManager D-Bus: ingest, de-dupe, send, delivery reports
src/wifi.rs      hotspot/client modes, scan, join, saved networks
src/net.rs       interfaces, routes, DNS, WireGuard, Tailscale peers, connection test
src/usage.rs     data usage from QMI counters
src/users.rs     users file, argon2id, sessions
src/cmd.rs       process + parsing helpers
src/update.rs    update check (GitHub releases), download, root helper (update-apply)
web/index.html   the whole UI (embedded at build time)
deploy/          systemd units, sudoers, polkit rule, default config, install.sh, release-key.gpg
contrib/         helper scripts for the stick
```

## Releases and the Update button

1. Bump `version` in `Cargo.toml`, commit, run `cargo fmt --check` and
   `cargo clippy --all-targets --locked -- -D warnings` (CI fails on either).
2. Tag (signed) and push the tag **on its own** (`git push github vX.Y.Z`), so
   the release workflow runs. It attaches the binary, the source archive and
   `SHA256SUMS`.
3. `scripts/sign-release.sh vX.Y.Z` signs that `SHA256SUMS` with the release key
   and checks the signature like a stick will; attach the resulting
   `SHA256SUMS.asc` to the release.

Sticks check `releases/latest` once a day (`update_check`) and show a badge.
Only releases with a valid `SHA256SUMS.asc` can be installed: the check uses the
key built into the *installed* binary (`deploy/release-key.gpg`, primary key
`6882 C1DB D9D7 0697 E99A 27A0 B1FE EB2B F584 65DC`). When the signing subkey
is renewed (it expires 2028-01-04), re-export it into `deploy/release-key.gpg`
in a release **before** the old expiry, or sticks can't verify newer releases:

```
gpg --export --export-options export-minimal \
    --export-filter keep-uid='uid =~ users.noreply.github.com' \
    6882C1DBD9D70697E99A27A0B1FEEB2BF58465DC > deploy/release-key.gpg
```

Parsers are unit-tested against real output captured on a UZ801 stick (`cargo test`). For UI work,
build, copy the binary, restart the service; the page needs no separate build step.

---
[← back to README](../README.md) · [docs index](README.md)
