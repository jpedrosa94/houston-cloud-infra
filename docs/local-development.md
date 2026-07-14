# Local Development

Run the Houston Cloud stack locally: a dockerized engine + the web frontend via Vite dev server.

## Prerequisites

- Docker 24+
- Node.js 22+ with pnpm
- The `houston` monorepo cloned as a sibling directory:
  ```
  ~/Projects/
  ├── houston/                  # engine + ui/ packages
  └── houston-cloud-infra/      # this repo
  ```

## 1. Start the engine

```bash
cd container/
cp .env.example .env
# Edit .env — set a token (or keep the default for dev)

docker compose up --build
```

Verify:

```bash
curl -H "Authorization: Bearer dev-token-replace-me" \
  http://localhost:7777/v1/health
```

Expected: `{"status":"ok","version":"...","protocol":1}`

The engine is now running on `http://localhost:7777`.

## 2. Link ui/ packages

The web frontend depends on `@houston-ai/*` packages from the houston monorepo. Add this repo's `web/` directory as a workspace in the monorepo:

```bash
cd ~/Projects/houston

# Add web/ to the pnpm workspace
echo '  - "../houston-cloud-infra/web"' >> pnpm-workspace.yaml

pnpm install
```

This resolves `workspace:*` dependencies to the local `ui/` packages.

## 3. Start the web frontend

```bash
cd ~/Projects/houston-cloud-infra/web
cp .env.example .env
# .env already points to http://localhost:7777

pnpm dev
```

Opens at `http://localhost:3000`. The frontend connects directly to the dockerized engine — no control plane, no auth required.

## How it works

Setting `VITE_ENGINE_URL` and `VITE_ENGINE_TOKEN` in `.env` activates **direct mode**:

- The `CloudEngineGate` connects straight to the engine at startup
- Auth is bypassed — no Supabase, no Google SSO
- The control plane is not needed

This is equivalent to what the desktop app does when connecting to a remote engine via Settings.

## Useful commands

| Command | What |
|---|---|
| `docker compose up --build` | Rebuild + start the engine |
| `docker compose up` | Start engine (no rebuild) |
| `docker compose down` | Stop engine |
| `docker compose down -v` | Stop engine + delete data volumes |
| `pnpm dev` | Start web frontend dev server |
| `pnpm build` | Build production SPA to `dist/` |
| `pnpm preview` | Serve the built `dist/` locally |

## Troubleshooting

**Engine fails to start**

Check if port 7777 is already in use:

```bash
lsof -i :7777
```

**"Engine not connected" in the browser**

- Verify the engine is healthy: `curl http://localhost:7777/v1/health`
- Check that `.env` has `VITE_ENGINE_URL=http://localhost:7777`
- Check that `VITE_ENGINE_TOKEN` matches `HOUSTON_ENGINE_TOKEN` in `container/.env`

**ui/ package changes not reflected**

The Vite dev server hot-reloads changes from linked workspace packages. If styles are missing, restart `pnpm dev` — Tailwind needs to re-scan `@source` paths on first load.

## Architecture (local dev)

```
Browser :3000          Engine container :7777
┌──────────────┐       ┌─────────────────────┐
│  Vite dev    │──────→│  houston-engine      │
│  server      │  HTTP │                      │
│              │──────→│  SQLite + flat files  │
│  @houston-ai │  WS   │  in /data/.houston   │
│  /* packages │       └─────────────────────┘
└──────────────┘
```

No control plane, no GKE, no Supabase — just the frontend talking to the engine.

## Architecture (cloud production)

```
CDN                    Control Plane :8080       Tenant Pod :7777
┌──────────────┐       ┌───────────────────┐    ┌──────────────────┐
│  Static SPA  │──────→│  houston-cloud-api │───→│  houston-engine  │
│  (dist/)     │  JWT  │                    │    │  (gVisor sandbox)│
│              │       │  Supabase auth     │    │  per-tenant PVC  │
└──────────────┘       └───────────────────┘    └──────────────────┘
```
