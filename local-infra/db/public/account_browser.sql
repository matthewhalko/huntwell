-- Per-account browser identity for authenticated scraping. context_id is the
-- account's persistent Browserbase Context (a cloud browser profile that keeps
-- cookies/logins across sessions); every run for the account attaches it so it
-- scrapes logged in. The user logs in once, by hand, through Browserbase's Live
-- View — we never see or store credentials, only the Context id and which sites
-- were connected. Lives in the operational (core) database; account_id is a
-- soft reference, ownership enforced in application code.
CREATE TABLE IF NOT EXISTS public.account_browser (
	account_id       bigint NOT NULL,
	-- The Browserbase Context id (created lazily on first connect).
	context_id       text NOT NULL DEFAULT '',
	-- JSON array of {site, url, connected_at} for display.
	connections_json text NOT NULL DEFAULT '[]',
	created_at       timestamptz NOT NULL DEFAULT now(),
	updated_at       timestamptz NOT NULL DEFAULT now(),
	PRIMARY KEY (account_id)
);
