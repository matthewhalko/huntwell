-- Every literal search the agent typed into a search engine, read off the
-- browser navigations. Feeds the rotation block that stops a plan from
-- opening with the same query every run.
CREATE TABLE IF NOT EXISTS public.search_query (
	plan_id       bigint NOT NULL,
	query_key     text NOT NULL,
	query        text NOT NULL,
	engine       varchar(40) NOT NULL DEFAULT '',
	hits         integer NOT NULL DEFAULT 0,
	max_depth     integer NOT NULL DEFAULT 1,
	-- Approximate: an iteration's new prospects are split across the queries
	-- it used. Enough to rank angles, not an audit trail.
	new_prospects integer NOT NULL DEFAULT 0,
	-- What this angle cost, in tokens, summed over the iterations that used
	-- it — attributed the same approximate way as new_prospects: enough to
	-- see that an angle is expensive and barren, not enough to bill anyone by.
	tokens        bigint NOT NULL DEFAULT 0,
	first_used_at  timestamptz NOT NULL DEFAULT now(),
	last_used_at   timestamptz NOT NULL DEFAULT now(),
	PRIMARY KEY (plan_id, query_key),
	FOREIGN KEY (plan_id) REFERENCES public.plan(plan_id) ON DELETE CASCADE
);
CREATE INDEX IF NOT EXISTS search_query_hits_idx ON public.search_query (plan_id, hits DESC);

