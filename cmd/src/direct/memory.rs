//! The plan's own records, reachable from the agent loop.
//!
//! [`crate::mcp::Server`] was built to be a child process talking stdio: it
//! owns a runtime and blocks on it. That is right for what it was for, and
//! wrong for calling from inside an async loop — `block_on` panics when the
//! thread it runs on already belongs to a runtime, and a blocking task still
//! belongs to one, so `spawn_blocking` does not save it either. That is not a
//! theory: it killed three runs, and then quietly disabled plan memory when it
//! was caught rather than fixed.
//!
//! So the server gets what it was designed for — a thread of its own, outside
//! any runtime — and the loop talks to it over a channel. No change to the
//! ported module, and no way for the two runtimes to meet.

use std::sync::mpsc;

use anyhow::{anyhow, Result};
use serde_json::Value;

type Job = (String, Value, tokio::sync::oneshot::Sender<Result<String>>);

/// A handle to the plan-memory server running on its own thread.
#[derive(Clone)]
pub struct Memory {
    jobs: mpsc::Sender<Job>,
}

impl Memory {
    /// Open the records for one plan. `None` when they cannot be opened — the
    /// tools are then simply not offered, and the run goes on without them.
    ///
    /// Safe to call from async: the thread this spawns is a plain OS thread,
    /// which is the one place `mcp::Server` can build its runtime.
    pub async fn start(database_url: String, plan_id: i64) -> Option<Self> {
        let (jobs, inbox) = mpsc::channel::<Job>();
        let (ready, opened) = tokio::sync::oneshot::channel::<bool>();

        std::thread::Builder::new()
            .name(format!("plan-memory-{plan_id}"))
            .spawn(move || {
                let server = match crate::mcp::Server::open(&database_url, plan_id) {
                    Ok(s) => {
                        let _ = ready.send(true);
                        s
                    }
                    Err(e) => {
                        tracing::warn!("plan memory is unavailable for this run: {e:#}");
                        let _ = ready.send(false);
                        return;
                    }
                };
                // Ends when the last handle is dropped, which is when the run
                // is over.
                while let Ok((name, args, reply)) = inbox.recv() {
                    let _ = reply.send(server.call_tool(&name, &args));
                }
            })
            .ok()?;

        match opened.await {
            Ok(true) => Some(Memory { jobs }),
            _ => None,
        }
    }

    /// Run one tool and wait for its answer.
    pub async fn call(&self, name: &str, args: &Value) -> Result<String> {
        let (reply, answer) = tokio::sync::oneshot::channel();
        self.jobs
            .send((name.to_string(), args.clone(), reply))
            .map_err(|_| anyhow!("the plan's records are no longer available"))?;
        answer.await.map_err(|_| anyhow!("the plan's records stopped answering"))?
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The invariant the whole module rests on: the thread `Memory` starts has
    /// no runtime of its own, so `mcp::Server` can build one there.
    ///
    /// Checked rather than assumed, because the obvious alternative —
    /// `spawn_blocking` — *looks* like a plain thread and is not: the runtime
    /// context follows it, `block_on` panics, and that killed three runs.
    #[tokio::test]
    async fn a_plain_thread_is_outside_the_runtime_but_a_blocking_task_is_not() {
        let plain = std::thread::spawn(|| tokio::runtime::Handle::try_current().is_ok()).join().unwrap();
        assert!(!plain, "a std thread must be free of the runtime — this is what makes Memory work");

        let blocking = tokio::task::spawn_blocking(|| tokio::runtime::Handle::try_current().is_ok()).await.unwrap();
        assert!(blocking, "and spawn_blocking is NOT — which is why it was the wrong fix");
    }

    /// A handle whose thread has gone answers rather than hanging.
    #[tokio::test]
    async fn a_memory_whose_thread_has_stopped_says_so() {
        let (jobs, inbox) = mpsc::channel::<Job>();
        drop(inbox);
        let memory = Memory { jobs };
        let e = memory.call("plan_status", &serde_json::json!({})).await.expect_err("the thread is gone");
        assert!(e.to_string().contains("no longer available"), "{e}");
    }

    /// Opening against a database that is not there gives `None` and leaves
    /// the run working without plan memory. Slow — it waits out the connect —
    /// so it is not in the ordinary suite.
    #[tokio::test]
    #[ignore]
    async fn opening_an_unreachable_database_yields_no_memory() {
        assert!(Memory::start("postgres://nobody@127.0.0.1:1/none".into(), 1).await.is_none());
    }
}
