# Aether MicroSocks framing fix

This directory tracks the host-side MicroSocks fix used by the manual
`netcup-ipv6` SOCKS5 proxy. It is intentionally separate from the Aether
application: the fault was in the proxy server's SOCKS5 stream framing.

## Problem

Debian MicroSocks `1.0.5-1` parsed each SOCKS5 phase from a single `recv()`.
TCP does not preserve application write boundaries, so a greeting split as
`05 01` followed by `00` could be parsed before the method byte arrived. The
server then returned `05 ff` (`no acceptable authentication method`).

The patch reads the complete field-sized frames for:

- method negotiation;
- username/password authentication;
- IPv4, IPv6, and domain-name CONNECT requests.

It also handles partial writes for the small SOCKS5 responses.

## Files

- `patches/microsocks-1.0.5-aether-framing.patch`: source change against the
  Debian `1.0.5-1` source package.
- `build.sh`: pinned, build-only reproduction script. It does not install or
  restart a service.
- `tests/framing_regression.py`: loopback-only fragmented framing and
  concurrency regression test.
- `LICENSE-MIT`: upstream MicroSocks license.

Source archives and compiled binaries are not committed. `build.sh` downloads
the pinned Debian source package and verifies these inputs:

```text
939d1851a18a4c03f3cc5c92ff7a50eaf045da7814764b4cb9e26921db15abc8  microsocks_1.0.5.orig.tar.gz
f9d49b78cc483cd9287b7cffbd5cffe083d29d916736531df19f5684b717130d  microsocks_1.0.5-1.debian.tar.xz
```

## Build and test

The host needs `deb-src` configured plus `cc`, `make`, `patch`,
`dpkg-buildflags`, and `strip`.

```bash
work_dir=$(mktemp -d)
./ops/microsocks/build.sh "$work_dir" /tmp/aether-microsocks-1.0.5-aether1
python3 ./ops/microsocks/tests/framing_regression.py \
  /usr/bin/microsocks \
  /tmp/aether-microsocks-1.0.5-aether1
```

The regression test only binds loopback sockets. It does not call ChatGPT or
use an account credential.

The artifact built for the 2026-08-31 production repair had SHA256:

```text
087fdf19221feaee85252dcb169ce6743ab255ed4762a4258f7f71f798657b87
```

## Production record

The production artifact and persistent build record are outside the Git
working tree:

```text
/usr/local/libexec/aether-microsocks-1.0.5-aether1
/usr/local/share/aether-ipv6-proxy/BUILD-20260831.txt
```

The package-owned `/usr/bin/microsocks` remains unchanged for rollback. Any
future production install or service restart still requires explicit
authorization and a health check.
