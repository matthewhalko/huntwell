-- A plan sending its results to a connected CRM (2026-10-13). Results first
-- seen at or after `since` are pushed; turning it on with "send the results
-- already found" sets `since` to the epoch.
CREATE TABLE IF NOT EXISTS public.plan_crm (
	plan_id         bigint NOT NULL,
	connection_id   bigint NOT NULL,
	account_id      bigint NOT NULL,
	enabled         boolean NOT NULL DEFAULT true,
	since           timestamptz NOT NULL DEFAULT now(),
	-- The sync loop's claim, so one pass at a time works a link. A stale one
	-- (a crash) is taken again after a lease.
	sync_started_at timestamptz,
	last_sync_at    timestamptz,
	last_error      text NOT NULL DEFAULT '',
	PRIMARY KEY (plan_id, connection_id),
	FOREIGN KEY (plan_id) REFERENCES public.plan(plan_id) ON DELETE CASCADE,
	FOREIGN KEY (connection_id) REFERENCES public.crm_connection(connection_id) ON DELETE CASCADE
);
CREATE INDEX IF NOT EXISTS plan_crm_enabled_idx ON public.plan_crm (connection_id) WHERE enabled;
