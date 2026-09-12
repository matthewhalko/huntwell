-- Scrape prompts that have actually run for a plan, with their yield. The
-- stored ScrapePrompt is 'config'; free-agent proposals are 'agent'. The
-- stored prompt is never overwritten — promotion is a deliberate click.
CREATE TABLE IF NOT EXISTS public.prompt_variant (
	plan_id       bigint NOT NULL,
	prompt_hash   char(40) NOT NULL,
	prompt       text NOT NULL,
	origin       varchar(10) NOT NULL DEFAULT 'agent',
	created_at    timestamptz NOT NULL DEFAULT now(),
	executions         integer NOT NULL DEFAULT 0,
	new_prospects integer NOT NULL DEFAULT 0,
	PRIMARY KEY (plan_id, prompt_hash),
	FOREIGN KEY (plan_id) REFERENCES public.plan(plan_id) ON DELETE CASCADE
);
