-- Keep engine tokens server-side only. Authenticated users can read their
-- own tenant metadata through RLS, but not the engine_token column.

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
