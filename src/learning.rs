use crate::{
    engine::{self, Phase},
    policy, reference,
    storage::{LockedRun, State, hash, verified_blob},
    workflow,
    workspace::{Plan, Workspace, load_stages},
};
use anyhow::{Context, Result, ensure};
use std::path::Path;

pub struct Request<'a> {
    pub run: &'a str,
    pub stage: &'a str,
    pub workflow: &'a str,
    pub rule: &'a str,
    pub confirm: Option<&'a str>,
}

pub fn save(space: &Workspace, run: &Path, state: &mut State, request: Request<'_>) -> Result<()> {
    let rule = request.rule.trim();
    ensure!(
        !rule.is_empty() && rule.len() <= 1024 && !rule.chars().any(char::is_control),
        "provide one line of reusable guidance, up to 1024 UTF-8 bytes"
    );
    let plan = Plan::load(run)?;
    state.verify_archives(run, false)?;
    policy::require_current(state, &plan)?;
    let view = engine::evaluate(run, &plan, state, false)?;
    let stage = view
        .stages
        .iter()
        .find(|item| item.stage.id == request.stage)
        .context("unknown source stage")?;
    ensure!(
        stage.phase == Phase::Done && stage.stage.fields.review.is_some(),
        "the current handoff must be approved before it can teach a workflow"
    );
    let entry = &state.stages[request.stage];
    let (head, correction) = entry.current().context("missing source revision")?;
    ensure!(
        !correction.edited.is_empty(),
        "this revision contains no recorded edit to carry forward"
    );
    let order = entry.ordered_ids()?;
    let before = order
        .iter()
        .rev()
        .nth(1)
        .context("missing prior revision")?;
    for name in &correction.edited {
        if let Some(digest) = entry.revisions[before].outputs.get(name) {
            verified_blob(run, digest)?;
        }
    }

    let workflow = space.workflow(request.workflow)?;
    ensure!(workflow.try_exists()?, "save the target workflow first");
    // Serialize confirmed writes, even when the source runs are different.
    let _lock = request
        .confirm
        .map(|_| LockedRun::acquire(&workflow, true))
        .transpose()?;
    let definitions = load_stages(&workflow)?;
    let definition = definitions
        .iter()
        .find(|definition| definition.id == request.stage)
        .context("the workflow has no matching stage")?;
    ensure!(
        definition.contract == stage.stage.contract
            || state.workflow.as_deref() == Some(request.workflow),
        "the source stage does not match this saved workflow"
    );
    let target = workflow::guidance_path(request.stage);
    let existing = reference::bytes(&workflow, &target)?;
    let existing_text = std::str::from_utf8(existing.as_deref().unwrap_or_default())?;
    let authorization = state
        .review_token(request.stage, head)
        .context("missing source review identity")?;
    let correction_id = hash(&serde_json::to_vec(&(authorization, before, rule))?);
    let marker = format!("<!-- anydock-correction {correction_id} from {before} to {head} -->");
    let destination = format!("workflows/{}/{target}", request.workflow);
    if existing_text.lines().any(|line| line == marker) {
        println!("Correction already recorded in {destination}.");
        return Ok(());
    }
    let token = hash(&serde_json::to_vec(&(
        request.workflow,
        request.stage,
        hash(&definition.contract),
        correction_id,
        existing.as_deref().map(hash),
    ))?);
    if let Some(confirm) = request.confirm {
        ensure!(
            confirm == token,
            "the proposal or workflow changed. Preview the guidance again"
        );
        let content = if existing_text.trim().is_empty() {
            "# Review guidance".to_owned()
        } else {
            existing_text.trim_end().to_owned()
        };
        let content = format!("{content}\n\n{marker}\n- {rule}\n");
        reference::write(&workflow, &target, content.as_bytes(), existing.is_some())?;
        println!("Saved guidance to {destination}.");
        println!(
            "New runs of {} use it for {}.",
            request.workflow, request.stage
        );
        return Ok(());
    }

    println!(
        "Proposed guidance for {} | {}",
        request.workflow, request.stage
    );
    println!("Source: runs/{} | {before} -> {head}", request.run);
    for name in &correction.edited {
        println!("Edited file: runs/{}/{name}", request.run);
    }
    println!("Save to: {destination}");
    println!("Add: {rule}");
    println!("Only new runs receive this update.");
    println!("Confirmation: {token}");
    Ok(())
}
