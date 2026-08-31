# Aether MicroSocks framing fix

This directory is the source of truth for the host-side MicroSocks service
used by the manual `netcup-ipv6` SOCKS5 proxy.

The checked-in Linux amd64 artifact is only about 26 KB. The systemd unit runs
that file directly from the fixed Aether checkout path, so replacing a host
does not require downloading source or compiling C code.

## Problem and fix

Debian MicroSocks `1.0.5-1` parsed each SOCKS5 phase from a single `recv()`.
TCP does not preserve application write boundaries, so a greeting split as
`05 01` followed by `00` could be parsed before the method byte arrived. The
server then returned `05 ff` (`no acceptable authentication method`).

The repair reads complete field-sized frames for method negotiation,
username/password authentication, and IPv4, IPv6, or domain-name CONNECT
requests. It also handles partial writes for the small SOCKS5 responses.

## Repository contents

- `bin/linux-amd64/aether-microsocks-1.0.5-aether1`: tested runtime artifact.
- `systemd/aether-ipv6-proxy.service`: service definition that runs the
  repository artifact directly.
- `systemd/aether-ipv6-proxy.default.example`: host-specific configuration
  template.
- `tests/framing_regression.py`: loopback-only fragmentation, authentication,
  and concurrency regression test.
- `patches/microsocks-1.0.5-aether-framing.patch`: auditable source patch
  against Debian MicroSocks `1.0.5-1`.
- `build.sh`: optional maintainer script for rebuilding the artifact. A normal
  replacement host does not need to run it.
- `LICENSE-MIT`: upstream license.

Runtime artifact SHA256:

```text
087fdf19221feaee85252dcb169ce6743ab255ed4762a4258f7f71f798657b87
```

The artifact targets Linux x86-64 and requires glibc `2.38` or newer. For an
older libc or another CPU architecture, rebuild from the retained patch
instead of using this binary.

## Replace or rebuild a host

These instructions assume the repository path remains `/opt/stacks/aether`
and the host uses Debian or Ubuntu with systemd.

### 1. Clone the repository

```bash
git clone --branch custom git@github.com:Shirtiny/Aether.git /opt/stacks/aether
cd /opt/stacks/aether
```

### 2. Prepare networking

1. Configure the intended outbound IPv6 address and verify the default IPv6
   route.
2. Create the Docker network used by Aether and identify its host gateway.
   The current deployment uses `172.30.0.1` and port `1082`.
3. Restrict the listener to the required Docker subnet. Do not expose this
   unauthenticated SOCKS service publicly.

### 3. Test the repository artifact

```bash
sha256sum ./ops/microsocks/bin/linux-amd64/aether-microsocks-1.0.5-aether1
python3 ./ops/microsocks/tests/framing_regression.py \
  ./ops/microsocks/bin/linux-amd64/aether-microsocks-1.0.5-aether1
```

The test only binds loopback sockets. It does not call ChatGPT or use an
account credential.

### 4. Install host configuration and the unit

```bash
sudo install -D -o root -g root -m 0644 \
  ./ops/microsocks/systemd/aether-ipv6-proxy.service \
  /etc/systemd/system/aether-ipv6-proxy.service

if [ ! -e /etc/default/aether-ipv6-proxy ]; then
  sudo install -D -o root -g root -m 0644 \
    ./ops/microsocks/systemd/aether-ipv6-proxy.default.example \
    /etc/default/aether-ipv6-proxy
fi
sudoedit /etc/default/aether-ipv6-proxy
```

Set these host-specific values in `/etc/default/aether-ipv6-proxy`:

- `AETHER_MICROSOCKS_LISTEN_IP`: Docker gateway listened on by MicroSocks.
- `AETHER_MICROSOCKS_LISTEN_PORT`: normally `1082`.
- `AETHER_MICROSOCKS_BIND_ADDRESS`: outbound IPv6 address on the new host.

### 5. Enable and start

Starting or restarting the service closes existing proxy streams. Perform
these lifecycle commands only when the production action is authorized.

```bash
sudo systemd-analyze verify aether-ipv6-proxy.service
sudo systemctl daemon-reload
sudo systemctl enable --now aether-ipv6-proxy.service

systemctl is-enabled aether-ipv6-proxy.service
systemctl is-active aether-ipv6-proxy.service
systemctl show aether-ipv6-proxy.service -p ExecStart -p MainPID -p NRestarts
ss -ltnp '( sport = :1082 )'
```

`Restart=always` retries startup if the Docker gateway or IPv6 address is
briefly unavailable during boot. A normal reboot continues to execute the
binary under the repository path.

After the listener is healthy, configure Aether's manual proxy URL as
`socks5h://<docker-gateway>:<listen-port>`.

## Optional rebuild

Rebuilding is only needed for a different architecture or a future source
change. `build.sh` downloads the pinned Debian source package, verifies its
hashes, applies the retained patch, and produces a new artifact. Rebuilding
does not install or restart the service.

## Production record

The package-owned `/usr/bin/microsocks` remains unchanged for rollback. The
host-specific network values are intentionally kept in
`/etc/default/aether-ipv6-proxy`, not Git. Future repository binary changes
still require explicit production authorization before restarting the service.
