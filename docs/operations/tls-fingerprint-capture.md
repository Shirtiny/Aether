# TLS Fingerprint Capture

Aether stores per-request TLS capture under `usage.request_metadata.tls_fingerprint`.

```json
{
  "tls_fingerprint": {
    "incoming": {
      "source": "forwarded_header",
      "ja3": "...",
      "ja3_hash": "...",
      "ja4": "...",
      "protocol": "TLSv1.3",
      "cipher": "TLS_AES_128_GCM_SHA256",
      "sni": "api.example.com",
      "alpn": "h2"
    },
    "outgoing": {
      "source": "aether_transport_config",
      "observed": false,
      "transport_path": "direct",
      "backend": "reqwest_default_tls",
      "http_mode": "auto",
      "tls_stack": "native_tls",
      "tls_versions_offered": ["native_tls_default"],
      "alpn_offered": []
    }
  }
}
```

`incoming` is the client-to-Aether TLS fingerprint. It can be populated by Aether native TLS capture in direct deployments or by trusted reverse-proxy headers when TLS terminates before Aether.

`outgoing` is the Aether-to-provider TLS transport record. The current gateway records the exact transport configuration it controls. It sets `observed: false` because the normal execution path does not expose emitted ClientHello bytes inline. Capture/probe results can reuse the same object shape with `observed: true` plus `ja3`, `ja3_hash`, and `ja4`.

## Strict Codex Comparison

Hard requirements when claiming Codex default TLS equivalence:

- Keep `native-tls-vendored` enabled. Host OpenSSL produced a different JA3 in the 2026-06-28 comparison.
- Use `reqwest_default_tls` for the Codex default TLS profile. Rustls profiles, including `codex-reqwest-rustls-auto`, are legacy/non-equivalent for strict official Codex matching.

Use `tools/tls-clienthello-capture.py` to compare normalized ClientHello structure, not raw TLS record bytes. Raw ClientHello bytes contain connection randomness and key-share material, so byte-for-byte equality is not a useful contract.

The comparison currently checks:

- TLS record and legacy ClientHello versions
- cipher suite list/order with GREASE removed
- extension list/order with GREASE removed
- supported TLS versions
- supported groups
- EC point formats
- signature algorithms
- ALPN
- JA3 string and hash

Capture official installed Codex CLI against the local listener:

```bash
python3 tools/tls-clienthello-capture.py capture \
  --timeout 40 \
  --out /tmp/official-codex-clienthello.json \
  -- \
  codex exec \
    --ignore-user-config \
    --skip-git-repo-check \
    --ephemeral \
    -c model_provider="capture" \
    -c model="gpt-5" \
    -c model_providers.capture.name="capture" \
    -c model_providers.capture.base_url='"{url}/v1"' \
    -c model_providers.capture.wire_api="responses" \
    -c model_providers.capture.requires_openai_auth=false \
    -c model_providers.capture.experimental_bearer_token="dummy" \
    -c model_providers.capture.request_max_retries=0 \
    -c model_providers.capture.stream_max_retries=0 \
    "Say hi"
```

Capture Aether's legacy explicit rustls profile:

```bash
cargo build --manifest-path tools/tls-profile-probe/Cargo.toml

python3 tools/tls-clienthello-capture.py capture \
  --timeout 30 \
  --out /tmp/aether-current-rustls-clienthello.json \
  -- \
  tools/tls-profile-probe/target/debug/tls-profile-probe \
    aether-current-rustls \
    {url}
```

Capture Aether's Codex default TLS profile:

```bash
python3 tools/tls-clienthello-capture.py capture \
  --timeout 30 \
  --out /tmp/aether-codex-default-tls-vendored-clienthello.json \
  -- \
  tools/tls-profile-probe/target/debug/tls-profile-probe \
    aether-codex-default-tls \
    {url}
```

Capture the tunnel native-TLS connector shape:

```bash
python3 tools/tls-clienthello-capture.py capture \
  --timeout 30 \
  --out /tmp/aether-tunnel-native-tls-clienthello.json \
  -- \
  tools/tls-profile-probe/target/debug/tls-profile-probe \
    aether-tunnel-native-tls \
    {url}
```

Compare the official capture with the legacy rustls capture:

```bash
python3 tools/tls-clienthello-capture.py compare \
  /tmp/official-codex-clienthello.json \
  /tmp/aether-current-rustls-clienthello.json
```

Observed result on 2026-06-28:

- official installed Codex CLI JA3 hash: `23211f2b48104c7030b93680a2efcfd0`
- Aether legacy explicit rustls profile JA3 hash: `15a7254eddf31f45dc492932457ebcef`
- comparison result: `MISMATCH`

The main differences were cipher suites, extension ordering, supported groups, signature algorithms, and ALPN. Therefore the legacy `codex-reqwest-rustls-auto` profile is stable and observable, but it is not strict ordinary Codex CLI TLS equivalence.

Compare the official capture with the Codex default TLS capture:

```bash
python3 tools/tls-clienthello-capture.py compare \
  /tmp/official-codex-clienthello.json \
  /tmp/aether-codex-default-tls-vendored-clienthello.json
```

Observed result on 2026-06-28:

- Aether Codex default TLS profile JA3 hash: `23211f2b48104c7030b93680a2efcfd0`
- comparison result: `MATCH`

Compare the official capture with the tunnel native-TLS connector capture:

```bash
python3 tools/tls-clienthello-capture.py compare \
  /tmp/official-codex-clienthello.json \
  /tmp/aether-tunnel-native-tls-clienthello.json
```

Observed result on 2026-06-28:

- Aether tunnel native-TLS connector JA3 hash: `23211f2b48104c7030b93680a2efcfd0`
- comparison result: `MATCH`

Strict matching required reqwest 0.12 default/native TLS with vendored OpenSSL. The same reqwest default TLS path backed by the host system OpenSSL was close but still different: it produced JA3 hash `2617ff3a2d7f879546f0aac7afc5f15c`, with a different extension/EC point-format shape. Keep `native-tls-vendored` enabled for strict Codex CLI equivalence on this Linux build.

Aether also links `wreq` for browser-style transport profiles. Because `wreq` brings BoringSSL and Codex default TLS brings vendored OpenSSL, keep `wreq/prefix-symbols` enabled. Without it, test binaries that include both stacks can fail to link with duplicate crypto symbols.

Profile-level Codex transport records persist the expected normalized fingerprint under `fingerprint.transport_profile.extra.tls_fingerprint`. Codex account profiles also persist `codex_client_profile.transport_tls_fingerprint_hash`, and include it in `codex_client_profile.fingerprint_hash`. Usage metadata copies the expected transport fingerprint to `tls_fingerprint.outgoing.expected` when a plan carries that profile. Runtime request metadata is still marked `observed: false` unless a packet capture/probe supplied the actual ClientHello bytes for that request.

Keep the target shape fixed when comparing. An IP target normally has no SNI; a DNS target can add SNI. HTTP/2/ALPN configuration changes ClientHello. Proxy/tunnel routing must use the same selected transport backend; otherwise the route can replace the actual upstream TLS stack.

## Nginx TLS Termination

When nginx terminates HTTPS and proxies HTTP to Aether, Aether cannot see the original ClientHello. Configure nginx to forward the TLS fields it can observe:

```nginx
server {
    listen 443 ssl http2;
    server_name api.example.com;

    ssl_certificate     /etc/letsencrypt/live/api.example.com/fullchain.pem;
    ssl_certificate_key /etc/letsencrypt/live/api.example.com/privkey.pem;

    location / {
        proxy_pass http://127.0.0.1:3000;
        proxy_http_version 1.1;

        proxy_set_header Host $host;
        proxy_set_header X-Real-IP $remote_addr;
        proxy_set_header X-Forwarded-For $proxy_add_x_forwarded_for;
        proxy_set_header X-Forwarded-Proto $scheme;

        proxy_set_header X-Aether-TLS-Source nginx;
        proxy_set_header X-Aether-TLS-Protocol $ssl_protocol;
        proxy_set_header X-Aether-TLS-Cipher $ssl_cipher;
        proxy_set_header X-Aether-TLS-SNI $ssl_server_name;
    }
}
```

Stock nginx does not provide JA3/JA4 variables. The forwarded record is still useful, but it is not a complete TLS fingerprint. To forward JA3/JA4 through nginx, use an nginx build/module or edge layer that computes them and set:

```nginx
proxy_set_header X-Aether-TLS-JA3      $ja3;
proxy_set_header X-Aether-TLS-JA3-Hash $ja3_hash;
proxy_set_header X-Aether-TLS-JA4      $ja4;
```

Only accept these headers from trusted infrastructure. Do not expose Aether directly to public clients while also trusting client-supplied `X-Aether-TLS-*` headers.

## Nginx TCP Passthrough

If Aether terminates TLS itself, nginx can pass TCP through without decrypting:

```nginx
stream {
    map $ssl_preread_server_name $aether_backend {
        api.example.com 127.0.0.1:3443;
        default         127.0.0.1:3443;
    }

    server {
        listen 443;
        proxy_pass $aether_backend;
        ssl_preread on;
    }
}
```

In this mode nginx cannot inject HTTP headers because it never sees HTTP. Aether native TLS capture is responsible for populating `tls_fingerprint.incoming`.
