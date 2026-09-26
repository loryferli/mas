//! The model, reached through a sidecar: one line of JSON in, one line of JSON out.
//!
//! The engine gains no HTTP client, no async runtime and no TLS stack. A long-lived child
//! process holds the Anthropic SDK - which stays in Python, where the analysis layer already
//! lives - and speaks line-delimited JSON over stdin and stdout, the same way `build-cache`
//! shells out to `curl` rather than taking a request crate. The sidecar can be replaced by a
//! stub in a test without touching a line of Rust, which is what the suite does.
//!
//! ponytail: line-delimited JSON over a pipe. Move it in-process only if the per-decision round
//! trip ever shows up next to the model's own latency.
//!
//! Three properties this file exists to hold, each of which is a way a result like this is
//! usually fabricated:
//!
//! - **Nothing per tick and nothing per agent per tick.** A policy that costs a network call
//!   every tick is not a policy, it is a bill. Every answer is memoised on the exact bytes of the
//!   context it answered, and no context carries the clock, so a driver waiting at home with
//!   unchanged demand is one call rather than one call a second. [`DecisionCost::calls`] against
//!   [`DecisionCost::decisions`] is what makes the ratio visible instead of assumed.
//! - **A dead sidecar fails the run.** Every failure - a process that will not spawn, a closed
//!   pipe, a line that is not JSON, an `{"error": …}` from the sidecar itself - comes back as
//!   `Err` and ends the run. Falling back to the heuristic would report the heuristic's numbers
//!   under the model's name, which is the single worst thing this code could do.
//! - **An invalid action is counted and refused, never retried.** The refusal lives in
//!   [`crate::policy`], where the action space is; this file only counts it. The rate at which
//!   the model proposes something the world cannot do is one of the more interesting numbers
//!   this repository produces, so it is a metric column rather than a retry loop.

use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value as Json};
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, ChildStdout, Stdio};
use std::time::Instant;

/// The sidecar the scenarios reach for unless they say otherwise. Run through the shell so a
/// scenario names it the way a person would type it.
pub const DEFAULT_SIDECAR: &str = "uv run scripts/sidecar.py";

/// What the model cost, beside what it bought.
///
/// **Cost is a first-class metric here and never a footnote.** `usd`, `input_tokens` and
/// `output_tokens` are what the sidecar reports for the calls it actually made; `latency_ms` is
/// measured on this side of the pipe, so it includes the round trip rather than only the model's
/// own time. `decisions` counts every decision the model was responsible for and `calls` counts
/// the round trips that answered them, so the gap between the two is the memo's doing and a
/// reader can compute either rate.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize)]
pub struct DecisionCost {
    /// Decisions handed to the model, memo hits included.
    pub decisions: u64,
    /// Round trips to the sidecar. Never more than `decisions`, and usually far fewer.
    pub calls: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub usd: f64,
    /// Wall-clock time spent inside the round trips, summed.
    pub latency_ms: f64,
    /// Decisions the model failed to answer usably. Counted and refused, never retried - and
    /// counted per decision rather than per call, because a refused answer is memoised like any
    /// other (re-asking is the retry this refuses to do) and every decision that reads it really
    /// was made by the fallback rule.
    pub invalid: u64,
}

/// One answer the sidecar sent back: what it decided, and what that cost.
#[derive(Debug, Deserialize)]
struct Reply {
    /// The action, in whatever shape the request's schema asked for. Absent when `error` is set.
    #[serde(default)]
    action: Option<Json>,
    /// The sidecar's own account of what the call cost. Absent on an error.
    #[serde(default)]
    usage: Option<Usage>,
    /// Set when the sidecar could not answer. Ends the run.
    #[serde(default)]
    error: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
struct Usage {
    #[serde(default)]
    input_tokens: u64,
    #[serde(default)]
    output_tokens: u64,
    #[serde(default)]
    usd: f64,
}

/// A child process holding the model, spawned on the first decision and killed with the run.
///
/// Spawned lazily on purpose: a policy that never has an exception to hand over never starts a
/// process, which is what lets `hybrid` cost nothing on a scenario where the two fixed rules
/// happen to agree everywhere, and what makes "`heuristic` makes zero calls" a fact about the
/// process table rather than a claim about a counter.
#[derive(Debug)]
pub struct Sidecar {
    /// The command, as a person would type it. Run through `sh -c`.
    command: String,
    process: Option<Process>,
    /// The answer to every distinct context already asked about. **This is what keeps the phase
    /// affordable**, and it is only sound because no context carries the clock: two contexts that
    /// serialise to the same bytes are the same question.
    memo: HashMap<String, Json>,
    cost: DecisionCost,
}

#[derive(Debug)]
struct Process {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
}

impl Sidecar {
    pub fn new(command: &str) -> Sidecar {
        Sidecar {
            command: command.to_string(),
            process: None,
            memo: HashMap::new(),
            cost: DecisionCost::default(),
        }
    }

    pub fn cost(&self) -> DecisionCost {
        self.cost
    }

    /// Count an action the world cannot do. The refusal itself belongs to the caller, which is
    /// the only place that knows the action space.
    pub fn count_invalid(&mut self) {
        self.cost.invalid += 1;
    }

    /// Ask the model for one decision, and get back whatever the schema described.
    ///
    /// `decision` names the kind of decision, so the sidecar can put the right words around it.
    /// `context` is the serialised, leakage-audited view - nothing the deciding party could not
    /// know at decision time, and no clock, so the answer can be memoised. `schema` is the action
    /// space as JSON Schema, sent with every request so it lives in one place: here, beside the
    /// validation, rather than copied into the sidecar where the two could drift.
    pub fn ask(&mut self, decision: &str, context: &Json, schema: &Json) -> Result<Json> {
        self.cost.decisions += 1;
        let request = json!({"decision": decision, "context": context, "schema": schema});
        let key = request.to_string();
        if let Some(cached) = self.memo.get(&key) {
            return Ok(cached.clone());
        }

        let started = Instant::now();
        let line = self.round_trip(&key)?;
        self.cost.latency_ms += started.elapsed().as_secs_f64() * 1000.0;
        self.cost.calls += 1;

        let reply: Reply = serde_json::from_str(&line)
            .with_context(|| format!("the sidecar answered something that is not JSON: {line}"))?;
        if let Some(error) = reply.error {
            bail!("the sidecar failed the {decision} decision: {error}");
        }
        let usage = reply.usage.unwrap_or_default();
        self.cost.input_tokens += usage.input_tokens;
        self.cost.output_tokens += usage.output_tokens;
        self.cost.usd += usage.usd;

        let action = reply.action.ok_or_else(|| {
            anyhow!("the sidecar answered the {decision} decision with no action")
        })?;
        self.memo.insert(key, action.clone());
        Ok(action)
    }

    /// One line out, one line in. Every way this can fail ends the run: a sidecar that has died
    /// must never look like a policy that decided something.
    fn round_trip(&mut self, request: &str) -> Result<String> {
        if self.process.is_none() {
            self.process = Some(spawn(&self.command)?);
        }
        let process = self.process.as_mut().expect("just spawned");
        writeln!(process.stdin, "{request}")
            .and_then(|()| process.stdin.flush())
            .with_context(|| format!("writing to the sidecar `{}`", self.command))?;

        let mut line = String::new();
        let read = process
            .stdout
            .read_line(&mut line)
            .with_context(|| format!("reading from the sidecar `{}`", self.command))?;
        if read == 0 {
            bail!(
                "the sidecar `{}` closed its output mid-run; a run under a model policy fails \
                 rather than quietly finishing under another one",
                self.command
            );
        }
        Ok(line)
    }
}

impl Drop for Sidecar {
    fn drop(&mut self) {
        if let Some(process) = &mut self.process {
            // Dropping stdin is what a well-behaved sidecar exits on; the kill is for one that is
            // not, so a sweep of a hundred runs does not leave a hundred processes behind.
            let _ = process.child.kill();
            let _ = process.child.wait();
        }
    }
}

fn spawn(command: &str) -> Result<Process> {
    let mut child = std::process::Command::new("sh")
        .arg("-c")
        .arg(command)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .with_context(|| format!("spawning the sidecar `{command}`"))?;
    let stdin = child.stdin.take().expect("stdin was piped");
    let stdout = child.stdout.take().expect("stdout was piped");
    Ok(Process {
        child,
        stdin,
        stdout: BufReader::new(stdout),
    })
}
