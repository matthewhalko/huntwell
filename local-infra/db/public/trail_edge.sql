-- What led to what: the edges of a plan's knowledge graph.
--
-- search_query and visited_page already record *what* a plan has searched and
-- opened; neither records that this page came from that search. Without the
-- edge the record is two lists, and the interesting question — which angles
-- actually lead anywhere — cannot be answered.
--
-- Kinds are 'query' (a search_query.query_key), 'page' (a
-- visited_page.url_key) and 'result' (a prospect/artifact.source_key).
-- Edges observed repeatedly accumulate weight rather than duplicating, so the
-- graph stays the size of the plan's world and not of its history.
CREATE TABLE IF NOT EXISTS public.trail_edge (
	plan_id    bigint NOT NULL,
	from_kind  varchar(8) NOT NULL,
	from_key   text NOT NULL,
	to_kind    varchar(8) NOT NULL,
	to_key     text NOT NULL,
	weight    integer NOT NULL DEFAULT 1,
	-- The run that first drew this edge, so the graph can be read as history.
	execution_id     bigint,
	created_at timestamptz NOT NULL DEFAULT now(),
	last_seen_at timestamptz NOT NULL DEFAULT now(),
	PRIMARY KEY (plan_id, from_kind, from_key, to_kind, to_key),
	FOREIGN KEY (plan_id) REFERENCES public.plan(plan_id) ON DELETE CASCADE
);
CREATE INDEX IF NOT EXISTS trail_edge_plan_idx ON public.trail_edge (plan_id, last_seen_at DESC);

-- Where this edge falls in the plan's traversal. The graph is walked in order —
-- search, then the pages it opened, then what those produced — and without a
-- sequence the picture is a web with no direction. Assigned per plan on insert;
-- an edge seen again keeps its original place, since that is when the plan
-- first went that way.
ALTER TABLE public.trail_edge ADD COLUMN IF NOT EXISTS seq bigint NOT NULL DEFAULT 0;
