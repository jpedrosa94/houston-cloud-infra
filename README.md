# Houston Cloud Infra

Infrastructure and control-plane code for Houston Cloud, a multi-tenant GKE deployment where each authenticated user receives an isolated Houston Engine instance.

The repository contains:

- A Rust control-plane API for Supabase auth, tenant lifecycle, and Kubernetes provisioning.
- Hardened container build files for Houston Engine and the control plane.
- Kubernetes base manifests for namespaces, Gateway routing, tenant isolation, and control-plane deployment.
- Terraform modules for GKE, VPC/NAT, and Artifact Registry.
- Supabase migrations for the tenant registry.

## Architecture

Production traffic is designed to flow through the Gateway:

```text
Browser
  -> /api/* on api.engine.houston.cloud
  -> houston-cloud-api
  -> Supabase JWT verification
  -> per-user tenant provisioning/status

Browser
  -> tenant subdomain
  -> Gateway ext-authz
  -> houston-cloud-api validates JWT
  -> Gateway injects the tenant engine token
  -> tenant Houston Engine pod
```

Important security property: browser clients must not receive tenant engine tokens when `GATEWAY_DOMAIN` is enabled. Engine tokens are server-side only and are injected by the Gateway auth flow.

## Repository Layout

```text
container/                 Container builds and local engine compose stack
control-plane/             Rust Axum API for auth and tenant lifecycle
docs/                      Local development docs
k8s/base/                  Kubernetes base manifests
supabase/migrations/       Tenant registry schema and permission migrations
terraform/                 GCP infrastructure modules and dev environment
PLAN.md                    Longer implementation plan and cloud-readiness notes
```

## Local Development

Start the local engine:

```bash
cd container
cp .env.example .env
docker compose up --build
```

Check health:

```bash
curl -H "Authorization: Bearer dev-token-replace-me" \
  http://localhost:7777/v1/health
```

See [docs/local-development.md](docs/local-development.md) for the full local workflow.

## Control Plane

Required environment:

```text
SUPABASE_URL
SUPABASE_SERVICE_ROLE_KEY
SUPABASE_JWT_SECRET
```

Optional environment:

```text
ENGINE_IMAGE               Enables Kubernetes tenant provisioning
ENGINE_URL_TEMPLATE        Defaults to http://localhost:7777
GATEWAY_DOMAIN             Enables Gateway public tenant URLs and hides engine tokens from API responses
GATEWAY_SCHEME             Defaults to https
GVISOR                     Set to 1 or true for gVisor runtimeClassName
STORAGE_CLASS              Defaults to standard
CORS_ALLOWED_ORIGINS       Comma-separated browser origins; CORS is disabled if unset
DEV_ENGINE_TOKEN           Local development only
BIND                       Defaults to 0.0.0.0:3001
```

Run checks:

```bash
cd control-plane
cargo test
cargo clippy --all-targets -- -D warnings
```

## Kubernetes Notes

- `houston-cloud-api` is exposed as a `ClusterIP`.
- Public browser API access is only through the Gateway `HTTPRoute` for `/api/*`.
- `/ext-authz` must remain cluster-internal.
- Tenant pods run non-root, drop Linux capabilities, disable privilege escalation, use `RuntimeDefault` seccomp, and mount writable data only where needed.
- Tenant engine tokens are stored in Kubernetes Secrets and Supabase, not in client-visible API responses when Gateway mode is enabled.

## Supabase Notes

The `tenants` table stores one tenant row per Supabase user. Row Level Security allows users to read only their own metadata. The `engine_token` column is intentionally not granted to the `authenticated` role.

Apply all migrations in order:

```text
001_tenants.sql
002_restrict_tenant_token.sql
```

## Terraform Notes

Dev variables are documented in:

```text
terraform/envs/dev/terraform.tfvars.example
```

Local Terraform state, `.terraform/`, and real `*.tfvars` files are ignored and must not be committed.

## Secrets

Never commit:

- `control-plane/.env`
- `container/.env`
- `terraform/**/*.tfvars`
- `terraform/**/*.tfstate`
- `**/.terraform/`
- private keys or provider tokens

Use placeholder example files instead.

## Current Cloud Readiness

The infrastructure and control-plane scaffolding are in place. The main application-level blocker is still the Houston Engine provider model described in `PLAN.md`: cloud production needs direct HTTP provider support for LLM APIs instead of relying on interactive CLI subprocesses.
