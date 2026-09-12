-- Per-account token budget and consumption for the current billing period.
--
-- This is the guaranteed-margin control: a run is refused once tokens_used
-- reaches the available budget (token_allowance + token_topups). It lives in
-- the operational (core) database, not with account (which the auth service
-- owns) — usage and billing belong with the runs that generate them, and
-- account_id is a soft reference (no cross-service FK), account ownership
-- being enforced in application code as everywhere else.
--
-- Tokens are the unit because Cursor reports per-call token usage on every
-- plan; it does not always report a dollar cost. Convert to dollars with a
-- rate table if/when Cursor billing is usage-based.
CREATE TABLE IF NOT EXISTS public.account_usage (
	account_id      bigint NOT NULL,
	-- The plan's monthly base allowance, reset at each period roll.
	token_allowance bigint NOT NULL DEFAULT 50000000,
	-- Consumed this period; incremented live as runs report usage.
	tokens_used     bigint NOT NULL DEFAULT 0,
	-- Extra tokens bought this period on top of the allowance; reset on roll.
	token_topups    bigint NOT NULL DEFAULT 0,
	-- Start of the current period; a roll happens when a month has elapsed.
	period_start    timestamptz NOT NULL DEFAULT now(),
	created_at      timestamptz NOT NULL DEFAULT now(),
	updated_at      timestamptz NOT NULL DEFAULT now(),
	PRIMARY KEY (account_id)
);
-- Usage-based (dollar) billing, layered on the token meter above. The cap is on
-- dollars: a run is refused once billed spend reaches
-- budget_usd_micros + topup_usd_micros. Billed spend is the account's tokens
-- valued at the sell rate (HUNTWELL_SELL_USD_PER_MTOKEN); cost_usd_micros is
-- the summed Cursor COGS, so margin = billed − CostUsdMicros. All in micro-USD.
ALTER TABLE public.account_usage ADD COLUMN IF NOT EXISTS budget_usd_micros bigint NOT NULL DEFAULT 50000000;  -- $50
ALTER TABLE public.account_usage ADD COLUMN IF NOT EXISTS topup_usd_micros  bigint NOT NULL DEFAULT 0;
ALTER TABLE public.account_usage ADD COLUMN IF NOT EXISTS cost_usd_micros   bigint NOT NULL DEFAULT 0;

-- The payment method on file. A run is refused without one (see
-- web::runner::start), so this is a gate, not decoration.
--
-- Card numbers never reach this database: Stripe Checkout collects them and we
-- keep only what is safe to show and to charge against — the customer handle
-- and the brand/last4 Stripe itself returns. payment_ref is the Stripe
-- customer id (cus_…), or a mock handle when no Stripe key is configured, which
-- is how local development exercises the same flow (see web::billing).
ALTER TABLE public.account_usage ADD COLUMN IF NOT EXISTS payment_ref  varchar(120) NOT NULL DEFAULT '';
ALTER TABLE public.account_usage ADD COLUMN IF NOT EXISTS card_brand   varchar(24)  NOT NULL DEFAULT '';
ALTER TABLE public.account_usage ADD COLUMN IF NOT EXISTS card_last4   varchar(4)   NOT NULL DEFAULT '';
ALTER TABLE public.account_usage ADD COLUMN IF NOT EXISTS card_added_at timestamptz;
