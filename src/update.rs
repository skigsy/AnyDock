use crate::{
    engine::{self, Phase},
    policy, stage_context,
    storage::{
        Hashes, ReviewPresentation, RunUpdate, State, copy_exact, hash, live_hash, publish_run,
        verified_blob,
    },
    workspace::{BRIEF, Plan, Workspace, directory, file, validate_ref},
};
use anyhow::{Context, Result, ensure};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::Path,
};

pub struct Request<'a> {
    pub from: &'a str,
    pub id: &'a str,
    pub inputs: InputSelection<'a>,
    pub pending: bool,
    pub since: Option<&'a str>,
    pub submission: Option<&'a Hashes>,
    pub source_plan: Option<&'a Plan>,
}

pub fn create(
    space: &Workspace,
    previous: &Path,
    prior: &mut State,
    request: Request<'_>,
) -> Result<String> {
    let Request {
        from,
        id,
        inputs,
        pending,
        since,
        submission,
        source_plan,
    } = request;
    let target = space.run(id)?;
    ensure!(!target.try_exists()?, "run {id} already exists");
    let loaded;
    let plan = if let Some(plan) = source_plan {
        plan
    } else {
        loaded = Plan::load(previous)?;
        &loaded
    };
    policy::require_current(prior, plan)?;
    prior.verify_archives(previous, false)?;
    let view = engine::evaluate(previous, plan, prior, false)?;
    if pending {
        ensure!(
            !view.stages.iter().any(|item| item.attempt.is_some())
                && view
                    .action(prior.batch_review)
                    .is_some_and(|item| item.phase == Phase::Review),
            "a pending refresh requires the source run to be waiting for review"
        );
        if let Some(item) = view
            .stages
            .iter()
            .find(|item| item.phase == Phase::Review && item.revision.is_none())
        {
            anyhow::bail!(
                "{} has an unrecorded draft edit. Present the current review for {from} before updating",
                item.stage.id
            );
        }
    }
    if !pending && let Some(stage) = view.front() {
        anyhow::bail!(
            "finish the source run before updating. {} is {}",
            stage.stage.id,
            stage.phase.label()
        );
    }
    let mut outcome = RunUpdate {
        from: from.into(),
        identity: prior.identity.clone().context("missing source identity")?,
        inputs: BTreeSet::new(),
        baseline: prior
            .stages
            .iter()
            .filter_map(|(stage, entry)| {
                entry
                    .last_approved()
                    .map(|revision| (stage.clone(), revision.to_owned()))
            })
            .collect(),
        reused: BTreeMap::new(),
        presentation: None,
        submission: submission.cloned(),
    };
    let mut state = State::new(prior.workflow.clone())?;
    state.scoped_input_default = prior.scoped_input_default;
    state.batch_review = prior.batch_review;
    let scratch = directory(&space.root, ".anydock")?;
    fs::create_dir_all(&scratch)?;
    let temp = tempfile::tempdir_in(scratch)?;
    let staged = temp.path();
    let external = plan.external_inputs();

    let inputs = inputs.resolve(&space.root, &external)?;

    let mut definitions = BTreeMap::from([(BRIEF.into(), hash(&plan.brief))]);
    for stage in &plan.stages {
        definitions.extend(stage.definition_hashes());
        for reference in &stage.fields.references {
            let path = &reference.path;
            let digest = required_hash(previous, path)?;
            definitions.insert(path.clone(), digest);
        }
    }
    for (name, digest) in definitions {
        copy_exact(&file(previous, &name)?, &file(staged, &name)?, &digest)?;
    }
    if submission.is_some()
        && !pending
        && let Some(workflow) = &prior.workflow
    {
        crate::workflow::apply_guidance(space, workflow, staged)?;
    }
    for name in external {
        let old = required_hash(previous, name)?;
        let (root, path) = inputs
            .files
            .get(name)
            .map_or((previous, name), |source| (inputs.root, source.as_str()));
        let digest = if let Some(submission) = submission {
            submission
                .get(name)
                .cloned()
                .with_context(|| format!("missing submission receipt for {name}"))?
        } else {
            required_hash(root, path)?
        };
        if digest != old {
            outcome.inputs.insert(name.to_owned());
        }
        copy_exact(&file(root, path)?, &file(staged, name)?, &digest)?;
    }

    let plan = Plan::load(staged)?;
    policy::capture(&mut state, &plan);
    let pending = engine::reuse_unchanged(
        staged,
        previous,
        &plan,
        prior,
        &view,
        &mut state,
        &mut outcome,
    )?;
    if let Some(token) = since {
        let mut presentation = ReviewPresentation::capture(prior, token)?;
        for revision in &mut presentation.stages {
            revision.reused = outcome
                .reused
                .get(&revision.stage)
                .filter(|reuse| revision.reused.as_ref() == Some(&reuse.from))
                .map(|reuse| reuse.revision.clone());
        }
        for digest in presentation.digests().collect::<BTreeSet<_>>() {
            let source = verified_blob(previous, digest).context("retained review archive")?;
            let destination = file(staged, &format!(".anydock/blobs/{digest}"))?;
            if !destination.try_exists()? {
                copy_exact(&source, &destination, digest)?;
            }
        }
        outcome.presentation = Some(presentation);
    }
    state.set_update(outcome);
    let view = engine::evaluate(staged, &plan, &mut state, false)?;
    let start = if let Some(item) = view
        .action(state.batch_review)
        .filter(|item| item.phase == Phase::Ready)
    {
        for name in &item.stage.fields.outputs {
            let path = file(staged, name)?;
            fs::create_dir_all(path.parent().context("missing stage output folder")?)?;
        }
        let attempt = engine::begin(&mut state, item, false)?;
        Some((item.stage, attempt))
    } else {
        None
    };
    publish_run(&temp, &target, id, &state)?;
    println!("Updated {from} as {id}.");
    let outcome = state.update.as_ref().context("missing update record")?;
    for path in &outcome.inputs {
        println!("Changed input: runs/{id}/{path}");
    }
    let approvals =
        plan.stages
            .iter()
            .filter(|stage| {
                outcome.reused.get(&stage.id).is_some_and(|reuse| {
                    state.stages[&stage.id].approvals.contains(&reuse.revision)
                })
            })
            .count();
    println!(
        "Verified {} reused stages and kept {approvals} approvals.",
        outcome.reused.len()
    );
    for stage in &plan.stages {
        if let Some(reuse) = outcome.reused.get(&stage.id) {
            let approval = if stage.fields.review.is_none() {
                ""
            } else if state.stages[&stage.id].approvals.contains(&reuse.revision) {
                " | approval kept"
            } else {
                " | draft awaits review"
            };
            println!("Reused {} | {}{approval}", stage.id, reuse.from);
        } else if let Some(reasons) = pending.get(&stage.id) {
            println!("Rework {} | {}", stage.id, reasons.join(", "));
        }
    }
    if let Some((stage, attempt)) = start {
        println!("Stage {} | attempt {attempt}", stage.id);
        for output in &stage.fields.outputs {
            println!("Write: runs/{id}/{output}");
        }
        println!(
            "After verification: {} complete {id} {} --attempt {attempt}",
            crate::workspace::invocation(&space.root),
            stage.id
        );
        stage_context::show(&target, id, &plan, stage, state.input_format(stage))?;
    } else if view.stages.iter().any(|item| item.phase == Phase::Review) {
        println!(
            "Pending review: {} review {id}",
            crate::workspace::invocation(&space.root)
        );
    } else if view.front().is_some() {
        println!(
            "Inspect unresolved stages with {} status {id}.",
            crate::workspace::invocation(&space.root)
        );
    } else {
        println!("All stages are current. There is no new work to review.");
    }
    state.identity.context("published run has no identity")
}

#[derive(Clone, Copy)]
pub enum InputSelection<'a> {
    Inbox(&'a str),
    Mapped(&'a [String]),
    Source(&'a Path),
}

pub struct InputFiles<'root, 'name> {
    pub root: &'root Path,
    pub files: BTreeMap<&'name str, String>,
}

impl<'root> InputSelection<'root> {
    pub fn resolve<'name>(
        self,
        workspace: &'root Path,
        external: &BTreeSet<&'name str>,
    ) -> Result<InputFiles<'root, 'name>> {
        let (root, files) = match self {
            Self::Inbox(folder) => (workspace, inbox_files(workspace, external, folder)?),
            Self::Mapped(mappings) => (workspace, select(external, mappings)?),
            Self::Source(folder) => {
                ensure!(
                    folder.is_absolute()
                        && folder.canonicalize().ok().as_deref() == Some(folder)
                        && folder.is_dir(),
                    "saved source folder has moved or redirects: {}",
                    folder.display()
                );
                (folder, same_names(external, None)?)
            }
        };
        Ok(InputFiles { root, files })
    }
}

fn inbox_files<'a>(
    root: &Path,
    external: &BTreeSet<&'a str>,
    folder: &str,
) -> Result<BTreeMap<&'a str, String>> {
    ensure!(
        folder == "inbox" || folder.starts_with("inbox/"),
        "the update folder must be under inbox/"
    );
    directory(root, folder)?;
    same_names(external, Some(folder))
}

fn same_names<'a>(
    external: &BTreeSet<&'a str>,
    folder: Option<&str>,
) -> Result<BTreeMap<&'a str, String>> {
    external
        .iter()
        .map(|name| {
            let relative = name
                .strip_prefix("inputs/")
                .context("invalid declared input")?;
            Ok((
                *name,
                folder.map_or_else(
                    || relative.to_owned(),
                    |folder| format!("{folder}/{relative}"),
                ),
            ))
        })
        .collect()
}

fn required_hash(root: &Path, name: &str) -> Result<String> {
    live_hash(root, name)?.with_context(|| format!("missing update file {name}"))
}

fn select<'a>(
    allowed: &BTreeSet<&'a str>,
    mappings: &[String],
) -> Result<BTreeMap<&'a str, String>> {
    let mut selected = BTreeMap::new();
    for mapping in mappings {
        let (source, target) = mapping
            .split_once('=')
            .map_or((mapping.as_str(), None), |(source, target)| {
                (source, Some(target))
            });
        validate_ref(source)?;
        let name = source
            .strip_prefix("inbox/")
            .context("update imports files from inbox/")?;
        let default = format!("inputs/{name}");
        let target = target.unwrap_or(&default);
        let target = *allowed
            .get(target)
            .with_context(|| format!("{target} is not a declared run input"))?;
        ensure!(
            selected.insert(target, source.to_owned()).is_none(),
            "repeated destination {target}"
        );
    }
    Ok(selected)
}
