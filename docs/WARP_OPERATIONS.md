# WARP Egress Operations

## Topology

```
Claude Code
    │ POST /v1/messages
    ▼
lily2claude (Axum)
    │
    ├── HTTP provider ──► reqwest + Proxy ──────────────┐
    │                                                   │
    └── ACP provider ──► HTTP_PROXY / HTTPS_PROXY ──────┤
                                                        ▼
                                      127.0.0.1:40001 / 40002
                                                        │
                                                        ▼
                                      sing-box inside rootless Podman
                                      (mixed inbound :9091)
                                                        │
                                                        │ WireGuard UDP :2408
                                                        ▼
                                      Cloudflare WARP edge → Internet
```

Each node is a separate WARP registration with its own private key and
`reserved` bytes. In theory that gives each node a different egress IP; consumer
WARP uses shared NAT per datacenter, so two nodes can still share an IP. Use
`/health/warp` -> `distinct_egress_ips` to see the real capacity.

## Why host WARP breaks container WARP

Cloudflare's host client installs a route that captures the container's
WireGuard packets:

```
162.159.192.1 dev CloudflareWARP mtu 1280
```

The container sends packets with MTU 1408. Nested inside the host tunnel
(MTU 1280) the kernel returns `EMSGSIZE`, which surfaces as:

```
endpoint/wireguard[WARP]: failed to send data packets: sendmmsg: message too long
dns: lookup failed ... context deadline exceeded
```

The proxy port stays open, so a TCP-connect check passes while the tunnel is
dead. This is why the old health check produced false positives.

The fix is a split-tunnel exclusion on the host WARP client:

```bash
warp-cli --accept-tos tunnel ip add-range 162.159.192.0/24
warp-cli --accept-tos tunnel ip add-range 162.159.193.0/24
warp-cli --accept-tos tunnel ip add-range 162.159.195.0/24
warp-cli --accept-tos tunnel ip add-range 188.114.96.0/24
warp-cli --accept-tos tunnel ip add-range 188.114.97.0/24
warp-cli --accept-tos tunnel ip add-range 188.114.98.0/24
warp-cli --accept-tos tunnel ip add-range 188.114.99.0/24
warp-cli --accept-tos tunnel ip add-range 2606:4700:d0::/48
warp-cli --accept-tos tunnel ip add-range 2606:4700:d1::/48
```

Verify:

```bash
ip route get 162.159.192.1
# must NOT contain CloudflareWARP
l2c doctor
```

## Node lifecycle

```
Healthy ──(connect/timeout)──► Cooling(15s, exponential) ──► Rotating ──► Probe
   ▲                                │                              │
   └────────────(warp=on)───────────┴──────────(probe fails)───────┘
```

- Every 45 s the pool probes each node with
  `https://www.cloudflare.com/cdn-cgi/trace` through the proxy.
- A node only counts as healthy when the response contains `warp=on`.
- `429` puts the node in cooldown and schedules an IP rotation.
- Three consecutive network/probe failures also schedule a rotation.
- Rotation is guarded per node (`rotation_inflight`), so concurrent 429s do not
  spawn duplicate registrations.
- Failed rotations leave the node cooling and keep retrying on later requests.

## Rate-limit failover

A `429` is an egress-IP problem, so the correct response is to try the *other*
node, not to fail the user's turn:

- **HTTP providers**: `send_request_with_session()` tries up to `max_attempts`
  nodes. A 429 cools that node and immediately tries the next one. If both are
  cooling, it waits up to `acquire_wait_secs` for an in-flight rotation.
- **ACP providers**: a session is bound to one proxy at process spawn. On a
  rate-limit message the session is evicted, the node is cooled/rotated, and the
  next attempt spawns a new ACP process with the other proxy. Both the OpenCode
  and generic ACP adapters implement this retry loop.
- `acquire_proxy_wait()` is the piece that makes the retry useful when both
  nodes are cooling: it waits for a rotation to finish instead of immediately
  reusing the IP that just returned 429.

The daemon must be started with the pool enabled. The running process cannot
pick this up without a restart:

```bash
l2c server stop
L2C_ENABLE_WARP=1 l2c serve --port 4001
```

Starting the gateway also serves the current Claude Code session, so restart it
from another terminal (or exit Claude Code first).

Check the live process:

```bash
tr '\0' '\n' < /proc/$(cat /tmp/lily2claude_4001.pid)/environ | grep L2C_ENABLE_WARP
```

If that prints nothing, no ACP child receives `HTTP_PROXY`/`HTTPS_PROXY`, and no
proxy failover can happen regardless of the pool's health.

## Configuration

```toml
[warp]
probe_url = "https://www.cloudflare.com/cdn-cgi/trace"
probe_interval_secs = 45
probe_timeout_secs = 6
cooldown_429_secs = 60
cooldown_network_secs = 15
max_attempts = 2
rotation_enabled = true
require_warp = true
require_distinct_egress = false
acquire_wait_secs = 8

[[warp.nodes]]
url = "http://127.0.0.1:40001"
container = "test-warp-1"
runtime = "podman"       # auto | docker | podman

[[warp.nodes]]
url = "http://127.0.0.1:40002"
container = "test-warp-2"
runtime = "podman"
```

`runtime = "auto"` tries rootless Podman first, then Docker with an explicit
`unix:///var/run/docker.sock`. This avoids inheriting a `DOCKER_HOST` that points
at the wrong daemon.

The ACP path uses this pool when `L2C_ENABLE_WARP` is set. HTTP providers use
`warp_proxies` in their provider config for backwards compatibility.

## Health endpoint

```bash
curl -s http://127.0.0.1:4001/health/warp | jq
```

```json
{
  "status": "ok",
  "pools": 1,
  "nodes_total": 2,
  "nodes_healthy": 2,
  "distinct_egress_ips": 2,
  "nodes": [
    {
      "proxy_url": "http://127.0.0.1:40001",
      "container": "test-warp-1",
      "state": "healthy",
      "healthy": true,
      "cooldown_remaining_secs": 0,
      "egress_ip": "2a09:bac5:...",
      "last_ok_secs_ago": 3,
      "failures": 0,
      "rotation_count": 1,
      "last_error": null
    }
  ]
}
```

## Boot persistence (Quadlet)

Ad-hoc `podman run --restart always` is not restored on boot by itself.
`podman-restart.service` only starts containers once; it does not supervise them.
Use Quadlet:

```bash
scripts/setup-warp.sh --migrate
```

This preserves the `test-warp-*-config` volumes, installs the units from
`deploy/quadlet/`, enables linger, and adds the host WARP exclusions.

Bind the proxy to loopback only. Publishing `0.0.0.0:40001` exposes an open
proxy to the LAN.

## Runbook

| Symptom | Check | Fix |
|---|---|---|
| `sendmmsg: message too long` | `ip route get 162.159.192.1` | add split-tunnel exclusions |
| `warp=off` in trace | probe through the node | rotate / restart node |
| port closed after reboot | `systemctl --user status test-warp-1` | use `scripts/setup-warp.sh` |
| `429` on both nodes | `/health/warp` | wait for cooldown; rotation is automatic |
| `429` returned with no failover attempt | check daemon env for `L2C_ENABLE_WARP` | restart `l2c serve` with `L2C_ENABLE_WARP=1` |
| rotation loop | `podman logs test-warp-1` | check container runtime / registration API |
| `docker exec` wrong daemon | `echo $DOCKER_HOST` | set `runtime` explicitly in `[warp.nodes]` |

Useful commands:

```bash
l2c doctor
curl -s http://127.0.0.1:4001/health/warp | jq
podman logs --tail 50 test-warp-1
podman exec test-warp-1 cat /etc/sing-box/config.json | jq '.endpoints[0].address'
systemctl --user restart test-warp-1.service
```

## Security

- Publish only on `127.0.0.1`.
- Pin the image by digest in production; `latest` can change the config schema.
- WARP private keys live in the `test-warp-*-config` volume; keep the volume
  private to the user.
- Do not log request bodies or authorization headers.
