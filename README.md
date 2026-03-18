# Survivatorium GitConfig Proxy

Lightweight Rust proxy that bridges DayZ's HTTP engine to the GitHub API. Required by the [Survivatorium-GitConfig](../Survivatorium-GitConfig/) DayZ mod.

---

## Why is a proxy needed?

DayZ's Enfusion engine has limited HTTP capabilities:

- **No custom headers** — `RestContext` only supports `SetHeader(string)` which sets `Content-Type`. GitHub's API requires `User-Agent` and `Authorization` headers, so direct requests return `Client Error`.
- **Text-only responses** — `GET_now()` returns a string, corrupting binary files (`.dze`, `.map`, `.bin`, etc.)
- **Timeout on large files** — the engine's HTTP client times out on files above ~10 MB

The proxy solves all three by running on localhost, accepting HTTP (or HTTPS with TLS enabled) from the mod, and forwarding requests to GitHub with proper headers. For binary/large files, the proxy downloads from GitHub and writes raw bytes directly to disk.

```
                   Plain HTTP (default)             TLS to GitHub
DayZ Server ── HTTP ──▶ Proxy (localhost:8470) ── HTTPS ──▶ GitHub API
                                   │
                   With TLS enabled                 │
DayZ Server ── HTTPS ─▶ Proxy (localhost:8470) ── HTTPS ──▶ GitHub API
                                   │
                                   ▼ (/write endpoint)
                              Disk write
                         (binary-safe, raw bytes)
```

---

## Endpoints

| Endpoint | Method | Purpose | Key Parameters |
|----------|--------|---------|----------------|
| `/tree` | GET | Fetch the full repository file tree (Git Trees API) | `owner`, `repo`, `branch`, `token`* |
| `/raw` | GET | Download a single file's content as text | `owner`, `repo`, `branch`, `path`, `token`* |
| `/write` | GET | Download from GitHub and write raw bytes to disk | `owner`, `repo`, `branch`, `path`, `dest`, `localpath`, `token`* |
| `/health` | GET | Health check — returns `OK` | — |

\* `token` is optional if `GITHUB_TOKEN` env var or `--token` CLI arg is set.

### `/write` endpoint

Used for binary and large files. The mod sends the GitHub path and a local destination, and the proxy:

1. Downloads raw bytes from GitHub over HTTPS
2. Validates the destination path stays within the configured base directory
3. Creates parent directories as needed
4. Writes the raw bytes to disk

Requires `--profile-path` and/or `--mission-path` to be set.

| Parameter | Description |
|-----------|-------------|
| `dest` | Target base: `"profile"` or `"mission"` |
| `localpath` | Relative path under the base directory (e.g., `EditorFiles/MyMap.dze`) |
| `path` | GitHub repo path (for downloading from raw.githubusercontent.com) |

---

## Quick Start

### Build from source

```powershell
# Install Rust: https://rustup.rs
cd Survivatorium-GitConfig-Proxy
cargo build --release
```

Binary: `target\release\svc-gitconfig-proxy.exe` (~4 MB, fully self-contained via rustls)

### Run

```powershell
$env:GITHUB_TOKEN = "github_pat_XXXX"
.\target\release\svc-gitconfig-proxy.exe `
    --profile-path "C:\DayZServer\profiles" `
    --mission-path "C:\DayZServer\mpmissions\dayzOffline.chernarusplus"
```

### Helper script

Edit `start_proxy.bat` with your values and run it:

```bat
@echo off
set GITHUB_TOKEN=github_pat_XXXX
set SVC_PROFILE_PATH=C:\DayZServer\profiles
set SVC_MISSION_PATH=C:\DayZServer\mpmissions\dayzOffline.chernarusplus

svc-gitconfig-proxy.exe --bind 127.0.0.1 --port 8470
```

---

## CLI Options

| Argument | Default | Env Var | Description |
|----------|---------|---------|-------------|
| `--port` | `8470` | — | Port to listen on |
| `--bind` | `127.0.0.1` | — | Bind address |
| `--token` | — | `GITHUB_TOKEN` | GitHub PAT (overrides token from mod query params) |
| `--profile-path` | — | `SVC_PROFILE_PATH` | Local path to DayZ `$profile:` directory. Required for `/write` profile files. |
| `--mission-path` | — | `SVC_MISSION_PATH` | Local path to DayZ `$mission:` directory. Required for `/write` mission files. |
| `--timeout` | `300` | — | HTTP timeout (seconds) for GitHub downloads |
| `--allowed-ips` | `127.0.0.1` | `SVC_ALLOWED_IPS` | Comma-separated list of allowed client IPs. See [IP Allowlist](#ip-allowlist). |
| `--tls-cert` | — | `SVC_TLS_CERT` | Path to TLS certificate (PEM). Enables HTTPS when set with `--tls-key`. |
| `--tls-key` | — | `SVC_TLS_KEY` | Path to TLS private key (PEM). Enables HTTPS when set with `--tls-cert`. |

---

## TLS (HTTPS)

The proxy supports optional TLS, encrypting traffic between DayZ and the proxy. This is especially important when the proxy runs on a different machine — without TLS, your GitHub token and file contents travel in plaintext.

### When to use TLS

| Setup | TLS needed? |
|-------|-------------|
| Proxy on same machine as DayZ (localhost) | Optional — no network to sniff |
| Proxy on a different machine (LAN/cloud) | **Strongly recommended** |
| Proxy exposed to internet | **Required** (plus firewall + IP allowlist) |

### Generate certificates with mkcert (recommended)

[mkcert](https://github.com/FiloSottile/mkcert) creates locally-trusted certificates. DayZ's engine uses the Windows certificate store, so certificates signed by mkcert's auto-installed CA are trusted automatically.

```powershell
# Install mkcert (requires Go or use the pre-built binary)
# One-time: install the local CA into the system trust store
mkcert -install

# Generate cert for localhost (or your proxy's IP/hostname)
mkcert -cert-file proxy-cert.pem -key-file proxy-key.pem 127.0.0.1 localhost
```

This creates:
- `proxy-cert.pem` — the certificate
- `proxy-key.pem` — the private key

### Generate certificates with OpenSSL (alternative)

If you can't use mkcert, create a self-signed cert and manually import the CA:

```powershell
# Generate a self-signed certificate (valid for 365 days)
openssl req -x509 -newkey rsa:2048 -nodes \
    -keyout proxy-key.pem -out proxy-cert.pem \
    -days 365 -subj "/CN=127.0.0.1" \
    -addext "subjectAltName=IP:127.0.0.1"
```

Then import into Windows trust store so DayZ accepts it:
```powershell
# Run as Administrator
Import-Certificate -FilePath proxy-cert.pem -CertStoreLocation Cert:\LocalMachine\Root
```

### Run with TLS

```powershell
$env:GITHUB_TOKEN = "github_pat_XXXX"
.\svc-gitconfig-proxy.exe `
    --tls-cert proxy-cert.pem `
    --tls-key proxy-key.pem `
    --profile-path "C:\DayZServer\profiles" `
    --mission-path "C:\DayZServer\mpmissions\dayzOffline.chernarusplus"
```

The proxy will log `Listening on https://127.0.0.1:8470` when TLS is active.

### Update the mod config

Change `proxyUrl` in your `config.json` from `http://` to `https://`:

```json
"proxyUrl": "https://127.0.0.1:8470"
```

### Cross-machine example

Proxy on `192.168.1.50`, DayZ server on `192.168.1.100`:

```powershell
# On the proxy machine:
# Generate cert for the proxy's IP
mkcert -cert-file proxy-cert.pem -key-file proxy-key.pem 192.168.1.50

# Copy the mkcert CA cert to the DayZ server machine and install it:
# mkcert -CAROOT   (shows where the CA cert is)
# Copy rootCA.pem to the DayZ server, then:
# Import-Certificate -FilePath rootCA.pem -CertStoreLocation Cert:\LocalMachine\Root

# Start the proxy
.\svc-gitconfig-proxy.exe `
    --bind 0.0.0.0 `
    --tls-cert proxy-cert.pem `
    --tls-key proxy-key.pem `
    --allowed-ips 192.168.1.100 `
    --profile-path "D:\DayZServer\profiles" `
    --mission-path "D:\DayZServer\mpmissions\dayzOffline.chernarusplus"
```

On the DayZ server, set:
```json
"proxyUrl": "https://192.168.1.50:8470"
```

---

## Docker

```bash
docker build -t svc-gitconfig-proxy .
docker run -d \
    -e GITHUB_TOKEN=github_pat_XXXX \
    -e SVC_PROFILE_PATH=/data/profiles \
    -e SVC_MISSION_PATH=/data/mission \
    -e SVC_ALLOWED_IPS=172.17.0.1 \
    -v /path/to/profiles:/data/profiles \
    -v /path/to/mission:/data/mission \
    -p 8470:8470 \
    svc-gitconfig-proxy --bind 0.0.0.0
```

> **Note:** In Docker, the DayZ server connects through Docker's bridge network. The source IP is typically the gateway IP (e.g., `172.17.0.1`). Set `--allowed-ips` to that IP, **not** `127.0.0.1`. Run `docker network inspect bridge` to find the gateway address.

---

## Security

### IP Allowlist

The proxy only accepts requests from IP addresses in the allowlist. Default: `127.0.0.1` (localhost only).

```powershell
# Single IP (default — localhost only)
--allowed-ips 127.0.0.1

# Multiple IPs (comma-separated)
--allowed-ips 127.0.0.1,192.168.1.100,10.0.0.5

# Allow all IPs (0.0.0.0 = wildcard)
--allowed-ips 0.0.0.0
```

Or via environment variable:
```
SVC_ALLOWED_IPS=127.0.0.1,192.168.1.100
```

Requests from unlisted IPs receive `403 Forbidden` and are logged as warnings.

> **IPv6 note:** If your DayZ server connects via IPv6 loopback (`::1`), add it to the allowlist: `--allowed-ips 127.0.0.1,::1`

### ⚠️ DANGER: Binding to 0.0.0.0

Setting `--bind 0.0.0.0` makes the proxy accessible from **any network interface** — your LAN, the internet, or a VPN. This introduces serious risks:

| Risk | Impact | Mitigation |
|------|--------|------------|
| **Token theft** | An attacker on the network can send a request and the proxy will forward it with your GitHub token attached. They can read your entire private repo. | Always use `--allowed-ips` to restrict accepted client IPs. |
| **Plaintext HTTP** | Traffic between the DayZ server and proxy is unencrypted HTTP (unless TLS is enabled). Anyone on the network path can sniff the token and all file contents. | **Enable TLS** (`--tls-cert` / `--tls-key`). Or use a VPN/SSH tunnel. |
| **Arbitrary file read** | The `/raw` endpoint lets callers read any file in your private GitHub repo (within the token's scope). | Scope your GitHub PAT to a single repository with read-only access. |
| **Arbitrary disk write** | The `/write` endpoint writes files to the `--profile-path` and `--mission-path` directories. A malicious client could overwrite server configs. | `--allowed-ips` blocks unauthorized sources. The proxy also validates paths to prevent directory traversal. |
| **GitHub API abuse** | An attacker could spam requests to exhaust your GitHub API rate limit (5,000 requests/hour for authenticated users). | `--allowed-ips` prevents unauthorized clients from reaching the proxy. |

**Recommended setup for cross-machine deployments:**

1. Set `--bind 0.0.0.0` (necessary to accept remote connections)
2. Set `--allowed-ips` to **only** the DayZ server's IP
3. Scope your GitHub PAT to a single repo with **read-only** access
4. Firewall port 8470 to only the DayZ server's IP
5. Use a VPN or private network — do **not** expose port 8470 to the internet

### Token Security

- **Env var preferred**: `GITHUB_TOKEN` environment variable is the recommended method. The token is loaded once at startup and never appears in HTTP requests or logs.
- **Query param fallback**: If no env var is set, the DayZ mod sends the token in the URL query string. This is acceptable on localhost but means the token is in plaintext if TLS is not enabled and the proxy is network-exposed.
- **Never logged**: The proxy never prints the token value to logs. Error messages reference parameter names, not values.
- **Minimal scope**: The proxy only needs a GitHub PAT with **Contents: Read-only** on a single repository.

### SSRF Prevention

All URL-building parameters (`owner`, `repo`, `branch`) are validated by `validate_segment()` which blocks:
- Path traversal (`..`)
- Slashes (`/`, `\`)
- URL injection (`?`, `&`, `#`, `@`, `:`)
- Spaces

The proxy constructs URLs exclusively to `api.github.com` and `raw.githubusercontent.com`. User input cannot redirect requests to other hosts.

### Path Traversal Prevention (`/write`)

The `/write` endpoint protects against directory traversal with two layers:

1. **Input validation**: `validate_path()` rejects paths containing `..` or backslashes
2. **Canonical containment check**: After resolving the full path via `canonicalize()`, the proxy verifies it starts with the configured base directory. Symlink tricks are caught because `canonicalize()` resolves all symlinks before the check.

If the resolved path escapes the base directory → `403 Forbidden`.

### What the proxy does NOT do

- **No auto-TLS** — TLS is opt-in via `--tls-cert` and `--tls-key`. Without it, the proxy speaks plain HTTP.
- **No rate limiting** — if exposed on a network, authorized clients can make unlimited requests.
- **No authentication beyond IP** — any client from an allowed IP can use any endpoint. The proxy trusts the DayZ mod to send valid parameters.
- **No response body size limit** — the proxy loads full responses into memory. For very large repos (100k+ files), the `/tree` response could be large.

---

## Platform Support

| Platform | Notes |
|----------|-------|
| Windows Server 2016/2019/2022 | Pre-built binary or build from source |
| Windows 10/11 | Pre-built binary or build from source |
| Linux (x86_64) | Build from source or use the Dockerfile |

The binary is fully self-contained — no runtime dependencies. TLS is bundled via rustls (no OpenSSL needed).

---

## Troubleshooting

| Issue | Fix |
|-------|-----|
| `Failed to bind address` | Port 8470 is already in use. Stop the old proxy or change `--port`. |
| `IP X not in allowlist` | Add the client's IP to `--allowed-ips`. Check if IPv6 (`::1`) is being used. |
| `No GitHub token` | Set `GITHUB_TOKEN` env var or pass `--token`. |
| `/write` returns "unconfigured dest" | Start the proxy with `--profile-path` and/or `--mission-path`. |
| `Base path does not exist` | The `--profile-path` or `--mission-path` directory doesn't exist on disk. |
| `Resolved path escapes base directory` | Possible path traversal attempt or symlink issue. Check the `localpath` parameter. |
| `GitHub 401` | Token is invalid, expired, or doesn't have access to the repo. |
| `GitHub 404` | File or branch doesn't exist. Check `owner`, `repo`, `branch`, `path`. |
| `Download failed: timeout` | Increase `--timeout` (default 300s). Large files may take longer. |
| `Failed to load TLS certificate/key` | Check that `--tls-cert` and `--tls-key` point to valid PEM files. |
| DayZ can't connect over HTTPS | The proxy's cert isn't trusted. Install the CA in the Windows cert store (see [TLS section](#tls-https)). |

---

## License

MIT — See [LICENSE](LICENSE).
