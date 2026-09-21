-- Signed Stripe webhook events already applied. Stripe retries delivery, so
-- its event id is the idempotency boundary around every wallet reversal.
CREATE TABLE IF NOT EXISTS public.billing_event (
	event_id     varchar(120) NOT NULL,
	account_id  bigint NOT NULL,
	event_type  varchar(80) NOT NULL,
	created_at  timestamptz NOT NULL DEFAULT now(),
	PRIMARY KEY (event_id)
);
CREATE INDEX IF NOT EXISTS billing_event_account_idx
	ON public.billing_event (account_id, created_at DESC);
