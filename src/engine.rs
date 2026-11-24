use crate::{
    input_scope, quality, reference,
    storage::{
        Attempt, Basis, Hashes, Input, ReusedRevision, Revision, RunUpdate, State, archive,
        archive_staged, blob_path, copy_exact, hash, live_hash,
    },
    workspace::{BRIEF, Plan, Stage, file},
};
use anyhow::{Context, Result, bail, ensure};
use std::{collections::BTreeMap, fs, path::Path};

#[derive(Clone, Copy, Eq, PartialEq)]
pub enum Phase {
    Ready,
    Blocked,
    Running,
    Stale,
    Review,
    Done,
}

impl Phase {
    pub fn label(self) -> &'static str {
        match self {
            Self::Ready => "ready",
            Self::Blocked => "blocked",
            Self::Running => "in progress",
            Self::Stale => "stale",
            Self::Review => "awaiting review",
            Self::Done => "done",
        }
    }
}

pub struct StageView<'a> {
    pub stage: &'a Stage,
    pub phase: Phase,
    pub reasons: Vec<String>,
    pub checks: Vec<String>,
    pub basis: Option<Basis>,
    pub attempt: Option<u64>,
    pub revision: Option<String>,
}

pub struct View<'a> {
    pub stages: Vec<StageView<'a>>,
    pub changed: bool,
}

impl<'a> View<'a> {
    pub fn action(&self, batch_review: bool) -> Option<&StageView<'_>> {
        if batch_review
            && let Some(work) = self
                .stages
                .iter()
                .find(|item| item.attempt.is_some())
                .or_else(|| {
                    self.stages.iter().find(|item| {
                        matches!(item.phase, Phase::Ready | Phase::Stale) && item.basis.is_some()
                    })
                })
        {
            return Some(work);
        }
        self.front()
    }

    pub fn first_gated_by(&self, stage: &Stage, batch: bool) -> Option<&str> {
        self.stages
            .iter()
            .find(|later| {
                later.stage.id > stage.id
                    && later.phase != Phase::Done
                    && (!batch
                        || later
                            .stage
                            .fields
                            .inputs
                            .iter()
                            .any(|path| stage.fields.outputs.contains(path)))
            })
            .map(|later| later.stage.id.as_str())
    }

    pub fn front(&self) -> Option<&StageView<'a>> {
        self.stages.iter().find(|item| item.phase != Phase::Done)
    }
}

fn basis_diff(previous: &Basis, current: &Basis) -> Vec<String> {
    previous
        .keys()
        .chain(current.keys())
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .filter_map(|name| match (previous.get(name), current.get(name)) {
            (Some(a), Some(b)) if a == b => None,
            (None, _) => Some(format!("input added: {name}")),
            (_, None) => Some(format!("input removed: {name}")),
            (Some(a), Some(b)) if a.producer_revision != b.producer_revision => {
                Some(format!("upstream revision changed: {name}"))
            }
            _ => Some(format!("file changed: {name}")),
        })
        .collect()
}

fn read_outputs(run: &Path, stage: &Stage) -> Result<(Hashes, Vec<String>)> {
    let mut outputs = Hashes::new();
    let mut missing = Vec::new();
    for name in &stage.fields.outputs {
        match live_hash(run, name)? {
            Some(hash) => {
                outputs.insert(name.clone(), hash);
            }
            None => missing.push(format!("missing output: {name}")),
        }
    }
    Ok((outputs, missing))
}

struct Available {
    phase: Phase,
    revision: Option<String>,
    outputs: Hashes,
}

pub fn evaluate<'a>(
    run: &Path,
    plan: &'a Plan,
    state: &mut State,
    reconcile: bool,
) -> Result<View<'a>> {
    let mut view = View {
        stages: Vec::new(),
        changed: false,
    };
    let mut available: BTreeMap<&str, Available> = BTreeMap::new();
    let brief_hash = hash(&plan.brief);
    for stage in &plan.stages {
        let (basis, missing) = capture_basis(run, stage, plan, &brief_hash, &available)?;
        let mut item = StageView {
            stage,
            phase: Phase::Ready,
            reasons: missing,
            checks: Vec::new(),
            basis,
            attempt: None,
            revision: None,
        };
        let mut published = Hashes::new();
        if let Some(entry) = state.stages.get_mut(&stage.id) {
            if let Some(attempt) = &entry.attempt {
                item.attempt = Some(attempt.id);
                item.phase = if compare(&mut item, &attempt.basis) {
                    Phase::Running
                } else {
                    Phase::Stale
                };
            } else if let Some((id, revision)) = entry.current() {
                item.revision = Some(id.to_owned());
                item.phase = Phase::Stale;
                if compare(&mut item, &revision.basis) {
                    let (outputs, missing) = read_outputs(run, stage)?;
                    item.reasons.extend(missing);
                    if item.reasons.is_empty() {
                        let (passed, errors) = quality::run(run, stage, &outputs);
                        item.checks = passed;
                        item.reasons.extend(errors);
                    }
                    if item.reasons.is_empty() {
                        if outputs != revision.outputs {
                            let edited: Vec<String> = outputs
                                .iter()
                                .filter(|(name, hash)| revision.outputs.get(*name) != Some(*hash))
                                .map(|(name, _)| name.clone())
                                .collect();
                            if !reconcile {
                                item.reasons.extend(
                                    edited.iter().map(|name| format!("output edited: {name}")),
                                );
                                item.reasons.push(
                                    "live edit is unrecorded; use status to reconcile".into(),
                                );
                                item.revision = None;
                                if stage.fields.review.is_some() {
                                    item.phase = Phase::Review;
                                }
                                available.insert(
                                    &stage.id,
                                    Available {
                                        phase: item.phase,
                                        revision: None,
                                        outputs: Hashes::new(),
                                    },
                                );
                                view.stages.push(item);
                                continue;
                            }
                            archive(run, &outputs)?;
                            let id = entry.record(Revision {
                                basis: item.basis.clone().context("stage basis is missing")?,
                                outputs,
                                decision: stage.fields.review.clone(),
                                edited,
                                checks: item.checks.clone(),
                            })?;
                            item.revision = Some(id);
                            view.changed = true;
                        }
                        let (_, current) = entry.current().context("missing current revision")?;
                        item.reasons.extend(
                            current
                                .edited
                                .iter()
                                .map(|name| format!("output edited: {name}")),
                        );
                        published.clone_from(&current.outputs);
                        let approved = item
                            .revision
                            .as_ref()
                            .is_some_and(|id| entry.approvals.contains(id));
                        item.phase = if stage.fields.review.is_some() && !approved {
                            Phase::Review
                        } else {
                            Phase::Done
                        };
                    }
                }
            }
        }
        if item.phase == Phase::Ready && item.basis.is_none() {
            item.phase = Phase::Blocked;
        }
        available.insert(
            &stage.id,
            Available {
                phase: item.phase,
                revision: item.revision.clone(),
                outputs: published,
            },
        );
        view.stages.push(item);
    }
    Ok(view)
}

fn compare(item: &mut StageView<'_>, expected: &Basis) -> bool {
    let Some(basis) = &item.basis else {
        return false;
    };
    item.reasons.extend(basis_diff(expected, basis));
    item.reasons.is_empty()
}

fn capture_basis(
    run: &Path,
    stage: &Stage,
    plan: &Plan,
    brief_hash: &str,
    available: &BTreeMap<&str, Available>,
) -> Result<(Option<Basis>, Vec<String>)> {
    let mut basis = Basis::from([
        (
            BRIEF.into(),
            Input {
                hash: brief_hash.into(),
                producer_revision: None,
            },
        ),
        (
            stage.contract_path(),
            Input {
                hash: hash(&stage.contract),
                producer_revision: None,
            },
        ),
    ]);
    let mut missing = Vec::new();
    for name in &stage.fields.inputs {
        let mut producer_revision = None;
        let mut expected_hash = None;
        if let Some(producer) = plan.producers.get(name) {
            let upstream = &available[producer.as_str()];
            if upstream.phase != Phase::Done {
                missing.push(format!(
                    "{name} waits for {producer} ({})",
                    upstream.phase.label()
                ));
                continue;
            }
            producer_revision.clone_from(&upstream.revision);
            expected_hash = upstream.outputs.get(name);
        }
        let actual = if let Some(pointers) = stage.fields.input_fields.get(name) {
            match input_scope::read(run, name, pointers) {
                Ok(bytes) => bytes.map(|bytes| hash(&bytes)),
                Err(err) => {
                    missing.push(format!("input blocked: {err:#}"));
                    continue;
                }
            }
        } else {
            live_hash(run, name)?
        };
        match actual {
            Some(hash) if expected_hash.is_some_and(|expected| expected != &hash) => {
                missing.push(format!("upstream output changed during inspection: {name}"));
            }
            Some(hash) => {
                basis.insert(
                    name.clone(),
                    Input {
                        hash,
                        producer_revision,
                    },
                );
            }
            None => missing.push(format!("missing input: {name}")),
        }
    }
    for reference in &stage.fields.references {
        let label = reference.label();
        match reference::bytes(run, &reference.path)? {
            Some(bytes) => match reference.digest(&bytes) {
                Ok(hash) => {
                    basis.insert(
                        label,
                        Input {
                            hash,
                            producer_revision: None,
                        },
                    );
                }
                Err(err) => missing.push(format!("reference blocked: {err:#}")),
            },
            None => missing.push(format!("missing reference: {label}")),
        }
    }
    Ok((missing.is_empty().then_some(basis), missing))
}

pub fn reuse_unchanged(
    run: &Path,
    prior_run: &Path,
    plan: &Plan,
    prior: &State,
    source: &View<'_>,
    state: &mut State,
    update: &mut RunUpdate,
) -> Result<BTreeMap<String, Vec<String>>> {
    let mut available = BTreeMap::new();
    let mut pending = BTreeMap::new();
    let brief_hash = hash(&plan.brief);
    ensure!(
        plan.stages.len() == source.stages.len(),
        "update stages changed while copying"
    );
    for (stage, entry) in plan.stages.iter().zip(&source.stages) {
        ensure!(
            stage.id == entry.stage.id,
            "update stages changed while copying"
        );
        let (basis, mut reasons) = capture_basis(run, stage, plan, &brief_hash, &available)?;
        let mut result = Available {
            phase: Phase::Blocked,
            revision: None,
            outputs: Hashes::new(),
        };
        if let Some(basis) = basis {
            result.phase = Phase::Ready;
            if matches!(entry.phase, Phase::Done | Phase::Review)
                && let Some(old_id) = &entry.revision
            {
                let old = &prior.stages[&stage.id];
                let old_revision = &old.revisions[old_id];
                let mut expected = old_revision.basis.clone();
                for (name, input) in &mut expected {
                    if let Some(producer) = plan.producers.get(name)
                        && let Some(mapping) = update.reused.get(producer)
                        && input.producer_revision.as_deref() == Some(&mapping.from)
                    {
                        input.producer_revision = Some(mapping.revision.clone());
                    }
                }
                reasons = basis_diff(&expected, &basis);
                if reasons.is_empty() {
                    let (revision, errors) =
                        reuse_revision(run, prior_run, stage, old_revision, basis)?;
                    reasons = errors;
                    if let Some(revision) = revision {
                        let destination = state.stages.entry(stage.id.clone()).or_default();
                        let revision = destination.record(revision)?;
                        if old.approvals.contains(old_id) {
                            destination.approvals.insert(revision.clone());
                        }
                        update.reused.insert(
                            stage.id.clone(),
                            ReusedRevision {
                                from: old_id.clone(),
                                revision: revision.clone(),
                            },
                        );
                        result = Available {
                            phase: entry.phase,
                            revision: Some(revision),
                            outputs: old_revision.outputs.clone(),
                        };
                    }
                }
            } else {
                reasons.push("no verified recorded handoff to reuse".into());
            }
        }
        if !matches!(result.phase, Phase::Review | Phase::Done) {
            pending.insert(stage.id.clone(), reasons);
        }
        available.insert(stage.id.as_str(), result);
    }
    Ok(pending)
}

fn reuse_revision(
    run: &Path,
    source: &Path,
    stage: &Stage,
    previous: &Revision,
    basis: Basis,
) -> Result<(Option<Revision>, Vec<String>)> {
    for (name, digest) in &previous.outputs {
        copy_exact(&blob_path(source, digest)?, &file(run, name)?, digest)?;
    }
    let (checks, errors) = quality::run(run, stage, &previous.outputs);
    if !errors.is_empty() {
        for name in previous.outputs.keys() {
            fs::remove_file(file(run, name)?)?;
        }
        return Ok((None, errors));
    }
    archive_staged(run, &previous.outputs)?;
    Ok((
        Some(Revision {
            basis,
            outputs: previous.outputs.clone(),
            decision: previous.decision.clone(),
            edited: Vec::new(),
            checks,
        }),
        errors,
    ))
}

pub fn begin(state: &mut State, item: &StageView<'_>, retry: bool) -> Result<u64> {
    match item.phase {
        Phase::Review => bail!(
            "{} awaits human review; use status to see the current decision",
            item.stage.id
        ),
        Phase::Stale if item.basis.is_some() && !retry => {
            bail!(
                "{} is stale: {}; use next --retry to reopen it",
                item.stage.id,
                item.reasons.join(", ")
            )
        }
        Phase::Running if !retry => return item.attempt.context("missing active attempt"),
        Phase::Ready if retry => bail!("{} has no attempt or stale result to retry", item.stage.id),
        _ => {}
    }
    let basis = item
        .basis
        .clone()
        .with_context(|| item.reasons.join(", "))?;
    state.sequence = state
        .sequence
        .checked_add(1)
        .context("attempt counter exhausted")?;
    state
        .stages
        .entry(item.stage.id.clone())
        .or_default()
        .attempt = Some(Attempt {
        id: state.sequence,
        basis,
        checkpoint: None,
    });
    Ok(state.sequence)
}

pub fn prepare_completion(run: &Path, item: &StageView<'_>, attempt: u64) -> Result<Revision> {
    ensure!(
        item.phase == Phase::Running && item.attempt == Some(attempt),
        "{} cannot complete attempt {attempt}: {}{}",
        item.stage.id,
        item.phase.label(),
        if item.reasons.is_empty() {
            String::new()
        } else {
            format!(" ({})", item.reasons.join(", "))
        }
    );
    let (outputs, missing) = read_outputs(run, item.stage)?;
    ensure!(missing.is_empty(), "{}", missing.join(", "));
    let (checks, errors) = quality::run(run, item.stage, &outputs);
    ensure!(errors.is_empty(), "{}", errors.join("\n"));
    archive(run, &outputs)?;
    Ok(Revision {
        basis: item.basis.clone().context("stage basis is missing")?,
        outputs,
        decision: item.stage.fields.review.clone(),
        edited: Vec::new(),
        checks,
    })
}
