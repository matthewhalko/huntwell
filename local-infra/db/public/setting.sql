-- Operator-set configuration — the same table, and the same place in the
-- chain, as Park River's.
--
-- Everything a deployment needs EXCEPT the bootstrap chain, which cannot live
-- here because it is what makes this database reachable in the first place:
--
--   genesis key      a file beside the executable
--   global           sealed; KEY and SECRET only
--   Secrets Manager  the database connection, and every credential
--   -> setting       everything else: addresses, domains, limits, switches
--
-- Resolution at runtime is: process environment, then this table, then Secrets
-- Manager, then the global file. Set with `admin config set NAME VALUE`.
--
-- Never a credential. Every process that can reach the database can read this
-- table, and that includes every worker VM — which is exactly who must not see
-- the identity pool's key or the session secret. `admin config set` refuses
-- credential-shaped names and says to use Secrets Manager.
CREATE TABLE IF NOT EXISTS public.setting (
	key        varchar(120) NOT NULL,
	value      text NOT NULL DEFAULT '',
	updated_at timestamptz NOT NULL DEFAULT now(),
	PRIMARY KEY (key)
);
