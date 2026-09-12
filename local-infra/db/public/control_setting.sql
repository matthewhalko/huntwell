-- Control-plane key/value settings (operator-scoped, no account_id).
-- Keys in use: routing_strategy (random | round_robin | pinned),
-- pinned_host_id, pinned_pod, rr_cursor (round-robin position, "host:pod").
CREATE TABLE IF NOT EXISTS public.control_setting (
	key       varchar(60) NOT NULL,
	value     text NOT NULL DEFAULT '',
	updated_at timestamptz NOT NULL DEFAULT now(),
	PRIMARY KEY (key)
);
