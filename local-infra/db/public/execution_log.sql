-- The child's stdout/stderr, one row per line, streamed to the browser over
-- SSE by polling Seq.
CREATE TABLE IF NOT EXISTS public.execution_log (
	execution_id  bigint NOT NULL,
	seq    integer NOT NULL,
	ts     timestamptz NOT NULL DEFAULT now(),
	stream varchar(6) NOT NULL DEFAULT 'stdout',
	line   text NOT NULL,
	PRIMARY KEY (execution_id, seq),
	FOREIGN KEY (execution_id) REFERENCES public.execution(execution_id) ON DELETE CASCADE
);
