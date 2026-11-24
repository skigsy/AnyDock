use crate::{
    engine::{self, Phase},
    policy,
    storage::{LockedRun, State, verified_blob},
    workspace::{PROGRESS, Plan, file},
};
use anyhow::{Context, Result, ensure};
use serde::Serialize;
use std::{fs, io::Read, path::Path};

#[derive(Serialize)]
struct Handoff {
    schema: u32,
    run: String,
    mode: &'static str,
    instructions: &'static str,
    brief: String,
    progress: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    updated: Option<Update>,
    completed: Vec<Stage>,
    current: Option<Stage>,
    waiting: Vec<Stage>,
    changes: Vec<Change>,
    next: String,
}

#[derive(Serialize)]
struct Update {
    from: String,
    changed_inputs: Vec<String>,
}

#[derive(Serialize)]
struct Stage {
    id: String,
    state: &'static str,
    contract: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    reused_from: Option<String>,
    attempt: Option<u64>,
    checkpoint: Option<String>,
    inputs: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    input_commands: Vec<String>,
    references: Vec<String>,
    outputs: Vec<String>,
    reasons: Vec<String>,
    checks: Vec<String>,
    decision: Option<String>,
}

#[derive(Serialize)]
struct Change {
    file: String,
    excerpt: String,
    previous: Option<String>,
}

pub fn show(run: &Path, id: &str, json: bool) -> Result<()> {
    ensure!(run.try_exists()?, "run {id} does not exist");
    let prefix = |name: &str| format!("runs/{id}/{name}");
    let mut result = Handoff {
        schema: 1,
        run: id.into(),
        mode: "unregistered",
        instructions: "AGENTS.md",
        brief: prefix("brief.md"),
        progress: None,
        updated: None,
        completed: Vec::new(),
        current: None,
        waiting: Vec::new(),
        changes: Vec::new(),
        next: "Read the brief and stage definitions. Register only when execution is authorized."
            .into(),
    };
    if let Some(mut state) = LockedRun::inspect(run)? {
        result.mode = "managed";
        result.updated = state.update.as_ref().map(|update| Update {
            from: format!("runs/{}", update.from),
            changed_inputs: update.inputs.iter().map(|path| prefix(path)).collect(),
        });

        let plan = Plan::load(run)?;
        state.verify_archives(run, false)?;
        let changes = policy::changes(&state, &plan)?;
        if changes.is_empty() {
            let view = engine::evaluate(run, &plan, &mut state, false)?;
            let report = |item: &engine::StageView<'_>| Stage {
                id: item.stage.id.clone(),
                state: item.phase.label(),
                contract: prefix(&item.stage.contract_path()),
                reused_from: state.update.as_ref().and_then(|update| {
                    let revision = update.reused.get(&item.stage.id)?;
                    (item.revision.as_deref() == Some(&revision.revision))
                        .then(|| format!("runs/{} | {}", update.from, revision.from))
                }),
                attempt: item.attempt,
                checkpoint: state
                    .stages
                    .get(&item.stage.id)
                    .and_then(|entry| entry.attempt.as_ref())
                    .and_then(|attempt| attempt.checkpoint.clone()),
                inputs: item
                    .stage
                    .fields
                    .inputs
                    .iter()
                    .filter(|name| !item.stage.fields.input_fields.contains_key(*name))
                    .map(|name| prefix(name))
                    .collect(),
                input_commands: item
                    .stage
                    .fields
                    .input_fields
                    .keys()
                    .map(|name| format!("anydock input {id} {} {name}", item.stage.id))
                    .collect(),
                references: item
                    .stage
                    .fields
                    .references
                    .iter()
                    .map(|reference| prefix(&reference.label()))
                    .collect(),
                outputs: item
                    .stage
                    .fields
                    .outputs
                    .iter()
                    .map(|name| prefix(name))
                    .collect(),
                reasons: item.reasons.clone(),
                checks: item.checks.clone(),
                decision: item.stage.fields.review.clone(),
            };
            result.completed = view
                .stages
                .iter()
                .filter(|item| item.phase == Phase::Done)
                .map(report)
                .collect();
            if let Some(item) = view.action(state.batch_review) {
                result.current = Some(report(item));
                result.waiting = view
                    .stages
                    .iter()
                    .filter(|other| other.stage.id != item.stage.id && other.phase != Phase::Done)
                    .map(report)
                    .collect();
                result.next = match item.phase {
                    Phase::Ready | Phase::Running => {
                        format!("If execution is authorized, run anydock next {id}.")
                    }
                    Phase::Review => {
                        if let Some(presentation) = state
                            .update
                            .as_ref()
                            .and_then(|update| update.presentation.as_ref())
                        {
                            format!(
                                "If the human has seen the earlier presentation, use anydock review {id} --since {}. Otherwise use anydock review {id}.",
                                presentation.token
                            )
                        } else {
                            format!(
                                "Present the review decision. Run anydock status {id} to record live edits and show the current review revision."
                            )
                        }
                    }
                    Phase::Stale if item.basis.is_some() => format!(
                        "Inspect the changed files. If rework is authorized, run anydock next {id} --retry, then recheck review."
                    ),
                    _ => "Resolve the listed blockers.".into(),
                };
                for reason in &item.reasons {
                    let path = reason
                        .strip_prefix("file changed: ")
                        .filter(|path| path.starts_with("inputs/"))
                        .or_else(|| reason.strip_prefix("output edited: "));
                    if let Some(path) = path
                        && result.changes.len() < 3
                        && let Some(excerpt) = excerpt(run, path)?
                    {
                        let previous = state
                            .stages
                            .get(&item.stage.id)
                            .and_then(|entry| entry.current())
                            .and_then(|(_, revision)| revision.outputs.get(path))
                            .map(|digest| read_excerpt(&verified_blob(run, digest)?))
                            .transpose()?
                            .flatten();
                        result.changes.push(Change {
                            file: prefix(path),
                            excerpt,
                            previous,
                        });
                    }
                }
            } else {
                result.next = "Run complete. Review the handoffs.".into();
            }
        } else {
            result.next = format!(
                "Review requirement changed for {}. Restore it or ask the human to authorize the exact policy change.",
                changes
                    .iter()
                    .map(|c| c.stage)
                    .collect::<Vec<_>>()
                    .join(", ")
            );
        }
    } else if file(run, PROGRESS)?.try_exists()? {
        result.mode = "agent-only";
        result.progress = Some(prefix(PROGRESS));
        result.next = "Read the brief, progress record, and current stage. Verify the recorded digests and preserve the human review gate.".into();
    }

    if json {
        println!("{}", serde_json::to_string_pretty(&result)?);
    } else {
        println!("AnyDock handoff | {id} | {}", result.mode);
        println!("Read: {} and {}", result.instructions, result.brief);
        if let Some(progress) = &result.progress {
            println!("Progress: {progress}");
        }
        if let Some(update) = &result.updated {
            println!("Updated from: {}", update.from);
            for path in &update.changed_inputs {
                println!("Changed at update: {path}");
            }
        }
        for stage in &result.completed {
            println!("Done {} | {}", stage.id, stage.outputs.join(", "));
            if let Some(source) = &stage.reused_from {
                println!("  Reused from {source}");
            }
        }
        if let Some(stage) = &result.current {
            println!(
                "Current {} | {} | {}",
                stage.id, stage.state, stage.contract
            );
            if let Some(id) = stage.attempt {
                println!("Attempt: {id}");
            }
            if let Some(note) = &stage.checkpoint {
                println!("Progress note: {note}");
            }
            for input in &stage.inputs {
                println!("Input: {input}");
            }
            for command in &stage.input_commands {
                println!("Input view: {command}");
            }
            for reference in &stage.references {
                println!("Reference: {reference}");
            }
            for output in &stage.outputs {
                println!("Working file: {output}");
            }
            for reason in &stage.reasons {
                println!("  {reason}");
            }
            for check in &stage.checks {
                println!("  Check passed: {check}");
            }
            if let Some(decision) = &stage.decision {
                println!("Human review: {decision}");
            }
        }
        for stage in &result.waiting {
            println!("Waiting {} | {}", stage.id, stage.state);
            for reason in &stage.reasons {
                println!("  {reason}");
            }
        }
        for change in &result.changes {
            if let Some(previous) = &change.previous {
                println!("Recorded text, {}:", change.file);
                println!("{previous}");
            }
            println!("Current text, {}:", change.file);
            println!("{}", change.excerpt);
        }
        println!("Next: {}", result.next);
    }
    Ok(())
}

fn excerpt(run: &Path, path: &str) -> Result<Option<String>> {
    read_excerpt(&file(run, path)?).with_context(|| format!("read changed file {path}"))
}

fn read_excerpt(path: &Path) -> Result<Option<String>> {
    let file = fs::File::open(path)?;
    let mut bytes = Vec::new();
    file.take(1200).read_to_end(&mut bytes)?;
    if bytes.contains(&0) {
        return Ok(None);
    }
    // A partial final UTF-8 character may fall just beyond the preview limit.
    match std::str::from_utf8(&bytes) {
        Ok(text) => Ok(Some(text.into())),
        Err(error) if error.error_len().is_none() => Ok(Some(String::from_utf8(
            bytes[..error.valid_up_to()].to_vec(),
        )?)),
        Err(_) => Ok(None),
    }
}

pub fn takeover(state: &mut State, attempt: u64) -> Result<u64> {
    let active = state
        .stages
        .values_mut()
        .filter_map(|entry| entry.attempt.as_mut())
        .find(|active| active.id == attempt)
        .context("attempt is no longer active; inspect handoff")?;
    state.sequence = state
        .sequence
        .checked_add(1)
        .context("attempt counter exhausted")?;
    active.id = state.sequence;
    Ok(active.id)
}

pub fn checkpoint(state: &mut State, attempt: u64, note: &str) -> Result<()> {
    ensure!(
        !note.trim().is_empty() && note.len() <= 2048,
        "checkpoint must contain 1 to 2048 UTF-8 bytes"
    );
    let active = state
        .stages
        .values_mut()
        .filter_map(|entry| entry.attempt.as_mut())
        .find(|active| active.id == attempt)
        .context("attempt is no longer active; inspect handoff")?;
    active.checkpoint = Some(note.to_owned());
    Ok(())
}
