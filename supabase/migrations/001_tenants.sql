-- Tenant registry: maps Supabase user → engine container.
-- Each user gets exactly one tenant (1:1).

CREATE TABLE public.tenants (
  id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
  user_id UUID NOT NULL REFERENCES auth.users(id) ON DELETE CASCADE,
  tenant_id TEXT NOT NULL UNIQUE,
  engine_token TEXT NOT NULL,
  status TEXT NOT NULL DEFAULT 'pending'
    CHECK (status IN ('pending', 'provisioning', 'ready', 'error', 'suspended')),
  created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
  last_active_at TIMESTAMPTZ NOT NULL DEFAULT now(),
  UNIQUE(user_id)
);

CREATE INDEX idx_tenants_user_id ON public.tenants(user_id);
CREATE INDEX idx_tenants_status ON public.tenants(status);

-- RLS: users can only read their own tenant row.
ALTER TABLE public.tenants ENABLE ROW LEVEL SECURITY;

CREATE POLICY tenant_select ON public.tenants
  FOR SELECT USING (auth.uid() = user_id);

-- Grant access to roles.
GRANT ALL ON public.tenants TO service_role;
REVOKE ALL ON public.tenants FROM anon;
REVOKE ALL ON public.tenants FROM authenticated;
GRANT SELECT (
  id,
  user_id,
  tenant_id,
  status,
  created_at,
  last_active_at
) ON public.tenants TO authenticated;
