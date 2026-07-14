# Houston Cloud — Containerization + GKE Deployment Plan

## Context

Houston Engine is a standalone Rust HTTP+WS binary that already runs containerized for the self-hosted "Always On" product. The goal is to create **Houston Cloud** — a multi-tenant cloud deployment on GKE where each user gets an isolated engine instance, plus a **standalone web frontend** that provides the full agent experience (chat, board, routines, skills) in the browser.

**Full architecture spec:** `../houston-cloud-proposal/README.md`
**Engine source:** `../houston/engine/`

---

## What was accomplished

### 1. Engine containerization (working)

- Hardened Dockerfile: multi-stage build, non-root UID 10001, STOPSIGNAL SIGTERM, OCI labels, health check
- docker-compose for local dev with security hardening (cap_drop ALL, no-new-privileges)
- `HOUSTON_NO_PARENT_WATCHDOG=1` to prevent stdin-EOF exit in containers
- `HOUSTON_DISABLE_COMPOSIO=1` env var added to engine to skip CLI auto-install
- Engine starts, serves HTTP+WS on port 7777, creates SQLite DB, responds to health checks

### 2. Frontend platform abstraction (working)

`app/src/lib/platform.ts` — runtime adapter that abstracts all `@tauri-apps/*` APIs. Detects Tauri at runtime via `window.__TAURI_INTERNALS__`. When Tauri is absent (web browser), provides browser-native equivalents or no-ops.

**11 files refactored** to import from `platform.ts` instead of `@tauri-apps/*`:
- `lib/os-bridge.ts` — invoke, listen, emit
- `lib/engine.ts` — listen, invoke
- `lib/auth.ts` — listen
- `lib/supabase.ts` — invoke
- `hooks/session-notifications.ts` — getCurrentWindow, notifications
- `hooks/use-session-events.ts` — onNotificationAction
- `hooks/use-update-checker.ts` — checkForUpdate
- `hooks/use-legal-acceptance.ts` — getCurrentWindow
- `components/portable/export-wizard.tsx` — invoke
- `components/portable/import-wizard.tsx` — invoke
- `lib/events.ts` — already used os-bridge (no direct Tauri imports)

**Zero `@tauri-apps` imports remain outside `platform.ts`.** Typecheck passes.

### 3. Cloud-native Vite config (working)

`app/vite.config.ts` updated:
- When `VITE_HOUSTON_ENGINE_BASE` is set, adds a `/v1/*` proxy to the engine (avoids CORS)
- Switches port to 3000 (cloud dev) vs 1420 (Tauri dev)
- `engine.ts` updated: when `VITE_HOUSTON_ENGINE_TOKEN` is set, connects directly without waiting for Tauri handshake

**Test command:**
```bash
# Terminal 1: engine container
cd houston-cloud-infra/container && docker compose up

# Terminal 2: frontend
cd houston/app
cp .env.cloud .env.local
pnpm vite
# Opens http://localhost:3000 — full Houston UI in browser
```

### 4. Infrastructure scaffolds (ready for implementation)

- Terraform modules: GKE cluster (gVisor node pool), VPC (Cloud NAT), Artifact Registry
- K8s manifests: namespaces, network policies, tenant pod template
- docker-compose with health checks, security hardening

---

## Key blocker: CLI subprocess architecture

### The problem

Houston Engine does not call LLM provider APIs (Anthropic, OpenAI) directly. Instead, it **spawns CLI subprocesses**:

```
User message -> Engine -> spawns `claude` CLI -> CLI calls api.anthropic.com
                       -> spawns `codex` CLI  -> CLI calls api.openai.com
```

These CLIs:
- **Claude Code CLI** — proprietary, macOS/Windows only, no official Linux builds
- **Codex CLI** — Apache-2.0, has Linux builds but designed for interactive use
- **Composio CLI** — MIT, macOS only in production, dev installs via `curl | bash`

### Why this blocks cloud deployment

| Issue | Impact |
|---|---|
| No Linux builds for Claude Code CLI | Cannot run Anthropic provider in a Linux container |
| CLIs expect interactive OAuth | No headless auth flow for containers |
| Each session spawns a full CLI process | ~100MB RAM per process, poor density |
| API keys stored on disk by CLIs | Security concern in shared container environment |
| CLIs auto-update independently | Moving target inside a "stable" container image |
| Proprietary license (Claude Code) | Cannot distribute inside container images |

### Required engine refactor

The engine needs a **direct API client** path — calling `api.anthropic.com` and `api.openai.com` via HTTP directly, without CLI subprocesses. This is the single largest piece of work for cloud readiness.

**Scope:** Add a `Provider` trait to `houston-engine-core` with two implementations:
1. `CliProvider` — current behavior (spawn claude/codex subprocess)
2. `HttpProvider` — new, calls APIs directly via `reqwest`, streams responses over WS

The `HttpProvider` would:
- Accept API keys via env vars or engine preferences (no interactive OAuth)
- Stream responses using SSE (Anthropic) or streaming (OpenAI) natively
- Eliminate the CLI process overhead entirely
- Run on any platform (Linux, macOS, Windows) without external binaries

**Estimated effort:** 2-3 weeks for a single-provider MVP (Anthropic first).

---

## Revised execution timeline

```
Phase 0 (done):   Engine containerization + UI platform abstraction
Phase 0.5 (NEW):  Direct API provider in engine (HttpProvider) — 2-3 weeks
Phase 1:          Container image with working LLM provider — 1 week after Phase 0.5
Phase 2:          Cloud web frontend (same app, CDN deploy) — 2 weeks
Phase 3:          GKE infrastructure (Terraform + K8s) — 2 weeks (parallel)
Phase 4:          Control plane (tenant lifecycle) — 3 weeks
Phase 5:          Networking + security — 2 weeks
Phase 6:          Hibernation + observability — 2 weeks

MVP: ~8-10 weeks (was 6, +2-3 for HttpProvider)
```

---

## What works today

| Component | Status | How to test |
|---|---|---|
| Engine in Docker | Running | `cd container && docker compose up` |
| Health endpoint | Working | `curl -H "Authorization: Bearer dev-token-replace-me" http://localhost:7777/v1/health` |
| Full Houston UI in browser | Working | `cd app && cp .env.cloud .env.local && pnpm vite` -> http://localhost:3000 |
| Workspace/agent CRUD | Working | Create workspaces and agents through the UI |
| Platform abstraction | Working | `app/src/lib/platform.ts` — zero Tauri imports outside this file |
| Chat / LLM interaction | Blocked | Needs HttpProvider (CLIs not available in container) |
| Composio integrations | Blocked | Needs HttpProvider or Linux CLI builds |

---

## Files changed in houston monorepo

| File | Change |
|---|---|
| `app/src/lib/platform.ts` | NEW — runtime Tauri abstraction |
| `app/src/lib/os-bridge.ts` | Import from platform.ts |
| `app/src/lib/engine.ts` | Import from platform.ts + env var direct connect |
| `app/src/lib/auth.ts` | Import from platform.ts |
| `app/src/lib/supabase.ts` | Import from platform.ts |
| `app/src/hooks/session-notifications.ts` | Import from platform.ts |
| `app/src/hooks/use-session-events.ts` | Import from platform.ts |
| `app/src/hooks/use-update-checker.ts` | Import from platform.ts |
| `app/src/hooks/use-legal-acceptance.ts` | Import from platform.ts |
| `app/src/components/portable/export-wizard.tsx` | Import from platform.ts |
| `app/src/components/portable/import-wizard.tsx` | Import from platform.ts |
| `app/vite.config.ts` | Vite proxy for cloud dev |
| `app/.env.cloud` | NEW — env template for cloud dev |
| `engine/houston-engine-server/src/main.rs` | HOUSTON_DISABLE_COMPOSIO support |

## Files in houston-cloud-infra repo

| File | Purpose |
|---|---|
| `PLAN.md` | This document |
| `container/Dockerfile` | Hardened engine image |
| `container/docker-compose.yml` | Local dev stack |
| `container/.dockerignore` | Build context filter |
| `container/.env.example` | Engine token config |
| `k8s/base/namespaces.yaml` | houston-system + houston-tenants |
| `k8s/base/network-policies.yaml` | Tenant isolation |
| `k8s/base/tenant-pod-template.yaml` | Per-tenant pod spec |
| `terraform/modules/gke-cluster/main.tf` | GKE + gVisor node pool |
| `terraform/modules/vpc/main.tf` | VPC + Cloud NAT |
| `terraform/modules/artifact-registry/main.tf` | Container registry |
| `terraform/envs/dev/main.tf` | Dev environment |
| `docs/local-development.md` | Dev setup guide |
