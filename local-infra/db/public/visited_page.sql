-- Non-search pages the agent opened, so a later run can go back to a
-- directory it skimmed once instead of only ever moving forward.
CREATE TABLE IF NOT EXISTS public.visited_page (
	plan_id      bigint NOT NULL,
	url_key      text NOT NULL,
	url         text NOT NULL,
	host        text NOT NULL,
	visits      integer NOT NULL DEFAULT 0,
	first_seen_at timestamptz NOT NULL DEFAULT now(),
	last_seen_at  timestamptz NOT NULL DEFAULT now(),
	PRIMARY KEY (plan_id, url_key),
	FOREIGN KEY (plan_id) REFERENCES public.plan(plan_id) ON DELETE CASCADE
);
CREATE INDEX IF NOT EXISTS visited_page_seen_idx ON public.visited_page (plan_id, last_seen_at);
