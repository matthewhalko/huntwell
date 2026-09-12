-- Who may work inside whose workspace.
--
-- An account is both a person (email, password) and the workspace that owns
-- their data: every plan, run and prospect keys on account_id. A team is
-- therefore not a new tenant — it is other people granted access to an existing
-- account's workspace, which is why nothing else in the schema changes.
--
-- The workspace's own account is the owner and has no row here; a row is a
-- guest. role is 'member' (works with the data) or 'admin' (also invites and
-- removes) — the owner alone can delete the workspace or be the last one out.
CREATE TABLE IF NOT EXISTS public.membership (
	-- The workspace being shared: an account.account_id.
	workspace_id bigint NOT NULL,
	-- The person given access: also an account.account_id.
	member_id    bigint NOT NULL,
	role        varchar(12) NOT NULL DEFAULT 'member',
	-- Who let them in, for the audit trail in the team list.
	invited_by_id bigint,
	created_at   timestamptz NOT NULL DEFAULT now(),
	PRIMARY KEY (workspace_id, member_id)
);
CREATE INDEX IF NOT EXISTS membership_member_idx ON public.membership (member_id);
