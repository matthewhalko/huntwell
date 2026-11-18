-- Signing the Huntwell Chrome extension in (2026-10-20): OAuth's authorization
-- code with PKCE. The signed-in person approves on /extension/authorize; that
-- makes one of these, and the extension trades the code (with the PKCE
-- verifier, at /ext/v1/token) for its own key (api_key.kind='ext').
-- Only the code's SHA-256 is kept; it is good for five minutes, once.
CREATE TABLE IF NOT EXISTS public.extension_grant (
	code_hash      char(64) NOT NULL,
	-- The workspace the key will be for, and who approved it (its maker).
	account_id     bigint NOT NULL REFERENCES public.account(account_id) ON DELETE CASCADE,
	person_id      bigint NOT NULL,
	-- PKCE: BASE64URL(SHA-256(verifier)). Only the extension that started the
	-- sign-in holds the verifier, so a code seen in transit is worth nothing.
	code_challenge varchar(128) NOT NULL,
	-- Must come back unchanged with the code.
	redirect_uri   text NOT NULL,
	created_at     timestamptz NOT NULL DEFAULT now(),
	expires_at     timestamptz NOT NULL,
	used_at        timestamptz,
	PRIMARY KEY (code_hash)
);
CREATE INDEX IF NOT EXISTS extension_grant_account_idx ON public.extension_grant (account_id);
