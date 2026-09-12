-- The planner's seed queue and history. A seed the planner proposes is written
-- here as 'pending' immediately, so a cancelled run leaves its work for the
-- next one; it flips to 'explored' once it has run and is never demoted.
CREATE TABLE IF NOT EXISTS public.search_frontier (
	plan_id       bigint NOT NULL,
	seed_key      char(40) NOT NULL,
	seed_json     text NOT NULL,
	iteration    integer,
	new_prospects integer,
	status       varchar(10) NOT NULL DEFAULT 'explored',
	queued_at     timestamptz,
	explored_at   timestamptz DEFAULT now(),
	PRIMARY KEY (plan_id, seed_key),
	FOREIGN KEY (plan_id) REFERENCES public.plan(plan_id) ON DELETE CASCADE
);
