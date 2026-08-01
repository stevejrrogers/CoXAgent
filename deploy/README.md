# Deploying CoXAgent (chat + calls + file storage) over HTTPS

One `docker compose` brings up the full stack on a Linux host with a public IP:

- **app** — the CoXAgent server (dashboard, chat, API, WebSocket).
- **MinIO** — S3-compatible file storage (chat + ticket uploads).
- **MongoDB** — server-side documentation store (the living docs system of record).
- **coturn** — TURN/STUN relay so 1:1 voice/video calls traverse NATs.
- **Caddy** — automatic HTTPS (Let's Encrypt) reverse proxy.

## Steps
1. Point a domain's DNS **A record** at the server's public IP.
2. Open the firewall: `80,443/tcp` (Caddy), `3478/tcp+udp` and
   `49160-49200/udp` (coturn).
3. `cp .env.example .env` and fill in `DOMAIN`, `PUBLIC_IP`, and the secrets.
   Generate `ADMIN_PASSWORD` rather than inventing one:
   `../scripts/rotate-admin-password.sh`.
4. `docker compose up -d --build`

Then open `https://<your-domain>` and sign in as the admin from `.env`.
HTTPS is required for the browser to grant camera/mic (calls) — Caddy handles it.

## Credentials
`.env` is **gitignored and stays that way** — it is the filled-in copy holding
the live super-admin password and every backing-service secret. `.env.example`
is the committed template; its `change-me-…` values are placeholders, not
defaults to ship.

COX-B030: a working `ADMIN_PASSWORD` was once committed in `.env`, so it is
public in this repo's history. Any deployment that ever used it must rotate:

```sh
../scripts/rotate-admin-password.sh --restart   # new password + recreate the app
```

The hub reads `COXAGENT_ADMIN_PASSWORD` once, at boot, and stores only a hash —
rotating the file without recreating the container changes nothing, and there
is no way to read the current password back out.

## Notes
- MinIO isn't exposed publicly; the app proxies file bytes, so browsers never
  talk to it directly.
- coturn runs with host networking (needs the public IP + a UDP relay range).
- For agent runs, the `claude`/`opencode` CLI must be available in the app
  container (mount it in, or bake it into the image) — chat/calls/files work
  without it.
