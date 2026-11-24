use crate::{
    engine::{self, Phase},
    handoff, policy,
    storage::{LockedRun, State, hash},
    workspace::Plan,
};
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use std::{
    ffi::{OsStr, OsString},
    path::Path,
    process::Command,
    time::{SystemTime, UNIX_EPOCH},
};

const LIFETIME: u64 = 30 * 60;

#[derive(Deserialize, Serialize)]
pub struct Offer {
    digest: String,
    basis: String,
    attempt: Option<u64>,
    expires: u64,
}

pub fn issue(
    path: &Path,
    lock: &LockedRun,
    state: &mut State,
    attempt: Option<u64>,
) -> Result<String> {
    let plan = Plan::load(path)?;
    verify(path, state, &plan, attempt)?;
    let mut random = [0; 32];
    getrandom::fill(&mut random)
        .map_err(|err| anyhow::anyhow!("generate handoff ticket: {err}"))?;
    let ticket = hash(&random);
    state.switch_offer = None;
    state.switch_offer = Some(Offer {
        digest: hash(ticket.as_bytes()),
        basis: basis(state, &plan)?,
        attempt,
        expires: seconds()? + LIFETIME,
    });
    lock.save(state)?;
    Ok(ticket)
}

pub fn claim(
    path: &Path,
    lock: &LockedRun,
    state: &mut State,
    ticket: &str,
) -> Result<Option<u64>> {
    let offer = state
        .switch_offer
        .take()
        .context("no pending handoff. Ask the sending harness for a new offer")?;
    ensure!(
        offer.digest == hash(ticket.as_bytes()),
        "ticket does not match the current handoff offer"
    );
    ensure!(
        seconds()? < offer.expires,
        "handoff ticket expired. Issue a new offer"
    );
    let plan = Plan::load(path)?;
    ensure!(
        offer.basis == basis(state, &plan)?,
        "run state or plan changed since the handoff offer. Issue a new offer"
    );
    verify(path, state, &plan, offer.attempt)?;
    let attempt = offer
        .attempt
        .map(|attempt| handoff::takeover(state, attempt))
        .transpose()?;
    state.switch_receipt = Some(offer.digest);
    lock.save(state)?;
    Ok(attempt)
}

fn verify(path: &Path, state: &mut State, plan: &Plan, attempt: Option<u64>) -> Result<()> {
    state.verify_archives(path, false)?;
    policy::require_current(state, plan)?;
    let view = engine::evaluate(path, plan, state, false)?;
    let work = view.action(state.batch_review);
    let active = work.and_then(|stage| stage.attempt);
    ensure!(
        active == attempt,
        "handoff attempt does not match the run. Inspect handoff and specify its active attempt"
    );
    if attempt.is_some() {
        ensure!(
            work.is_some_and(|stage| stage.phase == Phase::Running),
            "active attempt is no longer current. Inspect handoff before retrying"
        );
    }
    Ok(())
}

fn basis(state: &State, plan: &Plan) -> Result<String> {
    let contracts: Vec<_> = plan
        .stages
        .iter()
        .map(|stage| (&stage.id, hash(&stage.contract)))
        .collect();
    Ok(hash(&serde_json::to_vec(&(
        state,
        hash(&plan.brief),
        contracts,
    ))?))
}

fn seconds() -> Result<u64> {
    Ok(SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs())
}

pub fn was_claimed(state: &State, ticket: &str) -> bool {
    state.switch_receipt.as_deref() == Some(&hash(ticket.as_bytes()))
}

pub fn bootstrap(run: &str, ticket: &str, executable: &Path) -> String {
    format!(
        "Continue the authorized AnyDock run {run} in this workspace. Read AGENTS.md. Call the AnyDock executable at \"{}\" with claim {run} --ticket {ticket}. Read its handoff before editing, then honor the saved stage and human review instructions.",
        executable.display()
    )
}

pub struct Launcher {
    program: OsString,
    args: Vec<OsString>,
}

impl Launcher {
    pub fn parse(command: Vec<OsString>) -> Result<Self> {
        let mut parts = command.into_iter();
        let program = parts
            .next()
            .context("provide a local harness executable after --")?;
        let args: Vec<_> = parts.collect();
        ensure!(
            args.iter().filter(|arg| *arg == "{handoff}").count() == 1,
            "include exactly one separate {{handoff}} argument for the receiving harness's prompt"
        );
        if cfg!(windows) {
            let extension = Path::new(&program).extension().and_then(OsStr::to_str);
            ensure!(
                extension.is_none_or(|ext| ext.eq_ignore_ascii_case("exe")),
                "use a native executable or pass the harness entry file to its runtime"
            );
        }
        Ok(Self { program, args })
    }

    pub fn run(self, root: &Path, prompt: &str) -> Result<()> {
        let status = Command::new(&self.program)
            .current_dir(root)
            .args(self.args.iter().map(|arg| {
                if arg == "{handoff}" {
                    OsStr::new(prompt)
                } else {
                    arg.as_os_str()
                }
            }))
            .status()
            .with_context(|| {
                format!(
                    "launch {:?}. The offer only changes the attempt after a claim",
                    self.program
                )
            })?;
        ensure!(
            status.success(),
            "harness exited with {status}. Inspect handoff to see if it claimed the run"
        );
        Ok(())
    }
}
