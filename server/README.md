# Neko Route Share Server (`nekoshare`)

A reverse HTTP tunnel (yamux over TLS) that lets a Neko Route user share their
local router over the public internet. A friend points an OpenAI-compatible
client at `https://share.neko.arm.moe/<id>/v1` with a share token; the request
is tunneled to that user's machine, validated (token + model scope), and routed
through their own providers.

```
friend ──HTTPS──▶ share.neko.arm.moe/<id>/v1/...  ┐
                                                   │  (path-routed by <id>)
tunnel client ──HTTPS upgrade──▶ server.neko.arm.moe  ┘
        │  GET /tunnel  X-Neko-Id/-Secret  → 101 → yamux session
        ▼
   nekoshare (one binary, :443 + :80)
        │  opens one yamux stream per inbound request
        ▼  X-Neko-Share:1 + Bearer <token>  (id prefix stripped)
   user's Neko Route 127.0.0.1:8787
```

One Go binary terminates TLS for both hosts on `:443` and obtains/renews
Let's Encrypt certificates automatically (`autocert`); `:80` serves the ACME
HTTP-01 challenges and redirects to HTTPS. **No Caddy/nginx, no wildcard cert.**

## Routing (by Host)
- **`server.neko.arm.moe`** — control plane. The tunnel client sends an
  HTTP/1.1 upgrade (`GET /tunnel`) carrying `X-Neko-Id` / `X-Neko-Secret`; the
  server authenticates, replies `101`, hijacks the connection, and runs yamux.
  The `id↔secret` binding is persisted so a subdomain can only be reclaimed by
  its owner.
- **`share.neko.arm.moe`** — data plane. `…/<id>/v1/...` is routed to the live
  tunnel for `<id>`; the `/<id>` prefix is stripped so the user's Neko Route
  sees its own `/v1/...`. Responses stream back with flushing (SSE works).

Friend auth (the share token) and model scoping happen on the **user's** Neko
Route, not here — this server is an identity-routed pipe.

## Build & test
```sh
cd server
go test ./...
GOOS=linux GOARCH=amd64 CGO_ENABLED=0 go build -ldflags="-s -w" -o nekoshare .
```

## Flags
```
-share-host   default share.neko.arm.moe   (path-routed friend host)
-server-host  default server.neko.arm.moe  (tunnel-client host)
-http         default :80                  (ACME HTTP-01 + redirect)
-https        default :443
-state        default /var/lib/nekoshare/state.json   (id->secret store)
-cert-cache   default /var/lib/nekoshare/certs        (autocert cache)
-email        optional ACME contact
```

## Deploy (systemd)
DNS for both hosts must point at the server, and `:80`/`:443` must be publicly
reachable (autocert validates via HTTP-01 on first TLS handshake per host).

```sh
scp nekoshare root@<server>:/usr/local/bin/nekoshare
ssh root@<server> 'chmod +x /usr/local/bin/nekoshare'
# /etc/systemd/system/nekoshare.service → ExecStart=/usr/local/bin/nekoshare
systemctl enable --now nekoshare
# trigger + verify certs (from the server, where the hosts resolve to it):
curl -sI https://share.neko.arm.moe/        # 404 + valid cert
curl -sI https://server.neko.arm.moe/tunnel # 400 + valid cert
```

## Notes / limits
- High concurrency: goroutine-per-request + yamux multiplexing over one client
  connection. No per-request rate limiting yet (per-id registration limits TBD).
