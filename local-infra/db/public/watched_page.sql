-- Listing pages a plan has been shown to draw its results from, and the
-- links each one carried when last read (thrift::watch).
--
-- A scheduled run reads these pages over plain HTTP first. A listing page
-- that gains a listing gains a link, so when none of them shows a link that is
-- not recorded here, there is nothing new for an agent to find and the run is
-- skipped. `links` is a JSON array of link keys (thrift::page::link_key), not
-- page content.
CREATE TABLE IF NOT EXISTS public.watched_page (
	plan_id     bigint NOT NULL,
	account_id  bigint NOT NULL,
	url_key     text NOT NULL,
	url         text NOT NULL,
	links       text NOT NULL DEFAULT '[]',
	-- How many of the plan's stored results this page linked to when it
	-- qualified: the evidence that it is a listing page for this plan.
	result_links integer NOT NULL DEFAULT 0,
	checked_at  timestamptz NOT NULL DEFAULT now(),
	PRIMARY KEY (plan_id, url_key),
	FOREIGN KEY (plan_id) REFERENCES public.plan(plan_id) ON DELETE CASCADE
);
