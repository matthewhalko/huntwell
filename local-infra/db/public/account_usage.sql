-- Per-account meter and prepaid wallet.
--
-- This is the guaranteed-margin control: a run is refused once the prepaid
-- credit wallet is empty, and a running job is stopped the moment a debit
-- would take it below zero. account_id is a soft reference (no cross-service
-- FK); ownership is enforced in application code as everywhere else.
--
-- All money is in micro-USD. Tokens are the unit Cursor reports on every
-- call; billed spend is the account's tokens valued at the sell rate
-- (HUNTWELL_SELL_USD_PER_MTOKEN), cost_usd_micros the summed Cursor COGS, so
-- margin = billed − cost. New accounts start with no complimentary budget:
-- they buy prepaid credits and spend those.
CREATE TABLE IF NOT EXISTS public.account_usage (
	account_id             bigint NOT NULL,
	-- The plan's monthly base allowance, reset at each period roll.
	token_allowance        bigint NOT NULL DEFAULT 50000000,
	-- Consumed this period; incremented live as runs report usage.
	tokens_used            bigint NOT NULL DEFAULT 0,
	-- Extra tokens bought this period on top of the allowance; reset on roll.
	token_topups           bigint NOT NULL DEFAULT 0,
	-- Start of the current period; a roll happens when a month has elapsed.
	period_start           timestamptz NOT NULL DEFAULT now(),
	created_at             timestamptz NOT NULL DEFAULT now(),
	updated_at             timestamptz NOT NULL DEFAULT now(),
	budget_usd_micros      bigint NOT NULL DEFAULT 0,
	topup_usd_micros       bigint NOT NULL DEFAULT 0,
	cost_usd_micros        bigint NOT NULL DEFAULT 0,
	-- The payment method on file. A run is refused without one (see
	-- web::runner::start), so this is a gate, not decoration. Card numbers
	-- never reach this database: Stripe collects them and we keep only what
	-- is safe to show and to charge against — the customer handle (cus_…, or
	-- a mock handle in local development), the brand and last four Stripe
	-- itself returns, and the payment-method id (pm_…) so a saved card can be
	-- charged for credit purchases without asking for the number again.
	payment_ref            varchar(120) NOT NULL DEFAULT '',
	card_brand             varchar(24)  NOT NULL DEFAULT '',
	card_last4             varchar(4)   NOT NULL DEFAULT '',
	card_added_at          timestamptz,
	card_pm                varchar(120) NOT NULL DEFAULT '',
	-- Prepaid credit wallet. Purchased via Stripe against the card on file,
	-- decremented atomically as tokens are booked, never reset with the
	-- billing period. The wallet itself never goes negative — the CHECK
	-- below is the defence beneath the atomic debit query — which is how an
	-- account cannot overspend.
	credit_usd_micros      bigint NOT NULL DEFAULT 0,
	-- What was actually attributed to the customer this period. Differs from
	-- tokens_used at the hard cutoff: Cursor can report one final in-flight
	-- slice after it was incurred, but billed spend is capped at credits held.
	billed_usd_micros      bigint NOT NULL DEFAULT 0,
	-- Refunded/disputed credits that were already spent. Future purchases
	-- repay this before becoming spendable, so a chargeback cannot mint free
	-- work.
	credit_debt_usd_micros bigint NOT NULL DEFAULT 0,
	PRIMARY KEY (account_id),
	CONSTRAINT account_usage_credit_nonnegative CHECK (credit_usd_micros >= 0),
	CONSTRAINT account_usage_credit_debt_nonnegative CHECK (credit_debt_usd_micros >= 0),
	CONSTRAINT account_usage_billed_nonnegative CHECK (billed_usd_micros >= 0)
);

-- Auto-reload (2026-10-06): when spendable credit falls below
-- auto_reload_below_micros, the card on file is charged
-- auto_reload_amount_micros, off-session, by the website's reload loop
-- (billing::spawn_auto_reload). Only with the payer's recorded agreement:
-- who agreed, when, and to which wording (auto_reload_terms) — a changed
-- amount or threshold asks again. A declined card turns it off.
ALTER TABLE public.account_usage ADD COLUMN IF NOT EXISTS auto_reload boolean NOT NULL DEFAULT false;
ALTER TABLE public.account_usage ADD COLUMN IF NOT EXISTS auto_reload_below_micros bigint NOT NULL DEFAULT 0;
ALTER TABLE public.account_usage ADD COLUMN IF NOT EXISTS auto_reload_amount_micros bigint NOT NULL DEFAULT 0;
ALTER TABLE public.account_usage ADD COLUMN IF NOT EXISTS auto_reload_agreed_at timestamptz;
ALTER TABLE public.account_usage ADD COLUMN IF NOT EXISTS auto_reload_agreed_by bigint;
ALTER TABLE public.account_usage ADD COLUMN IF NOT EXISTS auto_reload_terms varchar(40) NOT NULL DEFAULT '';
-- A charge in flight: set when the loop claims the account, cleared when it
-- finishes. A stale one (a crash mid-charge) is reclaimed after a lease.
ALTER TABLE public.account_usage ADD COLUMN IF NOT EXISTS auto_reload_started_at timestamptz;
ALTER TABLE public.account_usage ADD COLUMN IF NOT EXISTS auto_reload_last_at timestamptz;
ALTER TABLE public.account_usage ADD COLUMN IF NOT EXISTS auto_reload_last_error text NOT NULL DEFAULT '';
