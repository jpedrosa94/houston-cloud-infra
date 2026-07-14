# CLAUDE.md

This file gives future agent sessions the project spec, constraints, and safe operating rules for `houston-cloud-infra`.

## Project Spec

Houston Cloud Infra supports a multi-tenant Houston Cloud deployment:

- Users authenticate with Supabase JWTs.
- The Rust control plane verifies JWTs, maps each Supabase user to one tenant, and provisions tenant runtime resources.
- Each tenant receives an isolated Houston Engine pod in the `houston-tenants` namespace.
- Browser API calls go to `/api/*` on the control-plane API through Gateway routing.
- Tenant engine traffic goes through Gateway ext-authz. The control plane validates the browser JWT and returns headers for the Gateway to route to the tenant engine and inject the server-side engine bearer token.
- Tenant engine tokens must not be exposed to browser clients when `GATEWAY_DOMAIN` is configured.

Key components:

- `control-plane/`: Rust Axum service.
- `control-plane/src/auth.rs`: Supabase JWT verification.
- `control-plane/src/routes.rs`: API and ext-authz routes.
- `control-plane/src/session.rs`: tenant session/provisioning flow.
- `control-plane/src/tenant.rs`: Supabase PostgREST tenant store.
- `control-plane/src/k8s/`: Kubernetes tenant resource builders and client calls.
- `container/`: Dockerfiles and local engine compose stack.
- `k8s/base/`: Kubernetes base manifests.
- `terraform/`: GCP infrastructure.
- `supabase/migrations/`: database schema and grants.

## Security Invariants

Do not weaken these without explicit user approval:

- `/ext-authz` must stay cluster-internal. Do not expose it through a public LoadBalancer or a broad Gateway route.
- Public control-plane access should be limited to `/api/*`.
- `response_token` must return `None` when `GATEWAY_DOMAIN` is set.
- JWT validation must check signature, audience, issuer, expiry, non-empty `sub`, and `role == "authenticated"`.
- Client-facing auth errors should be generic. Detailed verification errors should be logged, not returned.
- `CORS_ALLOWED_ORIGINS` must be explicit for browser access. Avoid permissive wildcard CORS in production paths.
- Supabase `authenticated` users must not have access to `tenants.engine_token`.
- Tenant pods should run non-root, drop capabilities, disable privilege escalation, use `RuntimeDefault` seccomp, and avoid writable root filesystems.
- Kubernetes probe specs must not inline raw engine tokens.
- Do not add public services for the control plane unless the route surface is restricted and reviewed.

## Files That Must Not Be Committed

The repo `.gitignore` should exclude these. Never force-add them:

- `control-plane/.env`
- `container/.env`
- `terraform/**/*.tfvars`
- `terraform/**/*.tfstate`
- `**/.terraform/`
- `.claude/settings.local.json`
- `node_modules/`
- `control-plane/target/`
- private SSH keys, provider API keys, service-role keys, JWT secrets, kubeconfigs, or cloud credentials

Use placeholder files:

- `control-plane/.env.example`
- `container/.env.example`
- `terraform/envs/dev/terraform.tfvars.example`

## Validation Commands

Run these after changing Rust control-plane code:

```bash
cd control-plane
cargo fmt
cargo test
cargo clippy --all-targets -- -D warnings
```

Run this after changing Kubernetes YAML:

```bash
ruby -e 'require "yaml"; ARGV.each { |f| YAML.load_stream(File.read(f)); puts "ok #{f}" }' \
  k8s/base/*.yaml
```

Run a targeted secret scan before commit:

```bash
rg --hidden -n --no-heading \
  '(-----BEGIN (RSA |DSA |EC |OPENSSH |PGP |)PRIVATE KEY-----|gh[pousr]_[A-Za-z0-9_]{20,}|github_pat_[A-Za-z0-9_]{40,}|sk-[A-Za-z0-9]{32,}|xox[baprs]-[A-Za-z0-9-]{20,}|AKIA[0-9A-Z]{16}|ASIA[0-9A-Z]{16}|AIza[0-9A-Za-z_-]{35}|eyJ[A-Za-z0-9_-]+\.[A-Za-z0-9_-]+\.[A-Za-z0-9_-]+|SUPABASE_SERVICE_ROLE_KEY=eyJ|SUPABASE_JWT_SECRET=[A-Za-z0-9+/=]{32,})' \
  -g '!.git' -g '!**/*.lock' -g '!pnpm-lock.yaml'
```

If `gitleaks` is installed, also run it before pushing.

## Git And Identity

This repo is intended to commit as:

```text
jpedrosa94 <julio.alexandre.pedrosa@gmail.com>
```

The configured remote is:

```text
git@github.com:jpedrosa94/houston-cloud-infra.git
```

SSH authentication is separate from commit author identity. This repo may use a repo-local `core.sshCommand` pointing at `/Users/julio/.ssh/id_ed25519_jpedrosa94`.

## Current Deployment Shape

Control plane:

- Runs in `houston-system`.
- Exposes a `ClusterIP` service on port 3001.
- Has an `HTTPRoute` for `/api/*`.
- Uses Supabase secrets from `control-plane-secrets`.
- Uses `CORS_ALLOWED_ORIGINS` and `GATEWAY_SCHEME`.

Tenant runtime:

- Runs in `houston-tenants`.
- Uses per-tenant PVC, Secret, Pod, Service, and optional HTTPRoute.
- Uses gVisor when enabled.
- Stores engine tokens in Kubernetes Secrets and Supabase.

Database:

- `public.tenants` has RLS enabled.
- `service_role` has full access.
- `authenticated` only receives column-level SELECT excluding `engine_token`.

## Known Cloud Readiness Blocker

The broader Houston product still depends on CLI subprocesses for some LLM provider paths. Cloud production needs direct HTTP provider support in the Houston Engine so tenant containers do not rely on interactive CLI auth or platform-specific CLI binaries. See `PLAN.md` for details.

## Agent Working Rules

- Prefer small, focused changes.
- Do not rewrite unrelated infrastructure.
- Preserve existing security hardening.
- Do not remove ignored secret rules to make a commit easier.
- Use `apply_patch` for manual edits.
- Use `rg` for searches.
- Keep docs and manifests ASCII unless editing an existing file that already uses non-ASCII.
- When changing deployment exposure, review both Kubernetes routes and application handlers together.
