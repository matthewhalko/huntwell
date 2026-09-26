-- What a workspace tells the outreach drafter: what it sells and the rules a
-- draft must follow. One row per workspace, shared by the team. Each person's
-- own sign-off lives on `account.outreach_footer`, not here.
CREATE TABLE IF NOT EXISTS public.outreach_profile (
	account_id  bigint NOT NULL,
	-- Free text: the product, what it does, who it is for.
	product     text NOT NULL DEFAULT '',
	-- Free text: how drafts should read ("under 120 words, no pricing").
	rules       text NOT NULL DEFAULT '',
	updated_at  timestamptz NOT NULL DEFAULT now(),
	updated_by  bigint,
	PRIMARY KEY (account_id),
	FOREIGN KEY (account_id) REFERENCES public.account(account_id) ON DELETE CASCADE
);
