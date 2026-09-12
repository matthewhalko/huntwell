-- Browser sessions. The cookie carries a random token; only its SHA-256 is
-- stored, so a copy of this table signs nobody in.
CREATE TABLE IF NOT EXISTS public.session (
	token_hash  char(64) NOT NULL,
	account_id  bigint NOT NULL,
	created_at  timestamptz NOT NULL DEFAULT now(),
	expires_at  timestamptz NOT NULL,
	last_seen_at timestamptz NOT NULL DEFAULT now(),
	user_agent  varchar(400) NOT NULL DEFAULT '',
	PRIMARY KEY (token_hash),
	FOREIGN KEY (account_id) REFERENCES public.account(account_id) ON DELETE CASCADE
);
CREATE INDEX IF NOT EXISTS session_account_idx ON public.session (account_id);
