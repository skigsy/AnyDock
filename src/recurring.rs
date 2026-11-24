use crate::{
    engine, handoff, policy, quick, stage_context,
    storage::{
        Hashes, LockedRun, RecurringStart, State, copy_exact, fault, hash, is_hash, live_hash,
        lock_file, publish_run, sync_dir, validate_input_hashes,
    },
    update::{self, InputFiles, InputSelection},
    workflow::Snapshot,
    workspace::{BRIEF, Plan, Workspace, directory, external_inputs, file, names, optional, slug},
};
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
};
use tempfile::NamedTempFile;

const RECORDS: &str = ".anydock/recurring";

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Head {
    version: u32,
    seed: String,
    run: String,
    identity: String,
    sequence: u64,
    submitted: Hashes,
}

impl Head {
    fn read(path: &Path, name: &str) -> Result<Option<Self>> {
        let Some(bytes) = optional(fs::read(path))? else {
            return Ok(None);
        };
        let head: Self = serde_json::from_slice(&bytes)
            .with_context(|| format!("read recurring task {name}"))?;
        ensure!(
            head.version == 1 && is_hash(&head.identity),
            "invalid recurring task {name}"
        );
        slug(&head.seed)?;
        let current = if head.sequence == 0 {
            head.seed.clone()
        } else {
            run_name(name, head.sequence)
        };
        ensure!(
            head.run == current,
            "invalid current run for recurring task {name}"
        );
        validate_input_hashes(&head.submitted)?;
        Ok(Some(head))
    }

    fn write(&self, path: &Path) -> Result<()> {
        write_task_record(path, self)
    }

    fn current(&self, space: &Workspace) -> Result<State> {
        let root = space.run(&self.run)?;
        let state = LockedRun::inspect(&root)?.with_context(|| {
            format!("restore the tracked run {} from a complete copy", self.run)
        })?;
        self.verify(&state)?;
        Ok(state)
    }

    fn verify(&self, state: &State) -> Result<()> {
        ensure!(
            state.identity.as_ref() == Some(&self.identity),
            "the tracked run {} has a different identity; restore its original state",
            self.run
        );
        ensure!(
            self.sequence == 0
                || state
                    .update
                    .as_ref()
                    .and_then(|update| update.submission.as_ref())
                    == Some(&self.submitted),
            "the tracked run {} has a different submission receipt",
            self.run
        );
        Ok(())
    }

    fn next(&self, name: &str) -> Result<(u64, String)> {
        let next = self
            .sequence
            .checked_add(1)
            .context("recurring run counter exhausted")?;
        let id = run_name(name, next);
        slug(&id)?;
        Ok((next, id))
    }

    fn published(&self, space: &Workspace, next: u64, id: &str) -> Result<Option<Self>> {
        let root = space.run(id)?;
        if !root.try_exists()? {
            return Ok(None);
        }
        let state = LockedRun::inspect(&root)?.with_context(|| {
            format!("recurring destination {id} already exists without managed state")
        })?;
        let update = state
            .update
            .as_ref()
            .filter(|update| update.from == self.run && update.identity == self.identity)
            .with_context(|| format!("existing run {id} does not belong to the pending refresh"))?;
        let submitted = update
            .submission
            .clone()
            .with_context(|| format!("existing run {id} has no recurring submission receipt"))?;
        state.verify_archives(&root, false)?;
        Ok(Some(Self {
            version: self.version,
            seed: self.seed.clone(),
            run: id.into(),
            identity: state.identity.context("published run has no identity")?,
            sequence: next,
            submitted,
        }))
    }
}

fn write_task_record(path: &Path, record: &impl Serialize) -> Result<()> {
    let folder = path.parent().context("missing recurring task directory")?;
    let mut file = NamedTempFile::new_in(folder)?;
    serde_json::to_writer_pretty(&mut file, record)?;
    writeln!(file)?;
    file.as_file().sync_all()?;
    crate::storage::persist_replace(file, path)
        .with_context(|| format!("update {}", path.display()))?;
    sync_dir(folder)
}

fn run_name(name: &str, sequence: u64) -> String {
    format!("{name}-{sequence:04}")
}

fn seed(space: &Workspace, id: &str) -> Result<Head> {
    crate::with_run(space, id, |root, _, state| {
        let plan = Plan::load(root)?;
        policy::require_current(state, &plan)?;
        state.verify_archives(root, false)?;
        let view = engine::evaluate(root, &plan, state, false)?;
        if let Some(item) = view.front() {
            anyhow::bail!(
                "finish seed run {id} first. {} is {}",
                item.stage.id,
                item.phase.label()
            );
        }
        let submitted = plan
            .external_inputs()
            .into_iter()
            .map(|input| {
                let digest = live_hash(root, input)?
                    .with_context(|| format!("missing seed input {input}"))?;
                Ok((input.to_owned(), digest))
            })
            .collect::<Result<_>>()?;
        validate_input_hashes(&submitted)?;
        Ok(Head {
            version: 1,
            seed: id.into(),
            run: id.into(),
            identity: state.identity.clone().context("seed run has no identity")?,
            sequence: 0,
            submitted,
        })
    })
}

pub enum Definition<'a> {
    Inbox(&'a str),
    Saved(&'a str),
}

pub fn begin(
    space: &Workspace,
    name: &str,
    definition: Definition<'_>,
    brief: &str,
    inputs: InputSelection<'_>,
    batch_review: bool,
) -> Result<()> {
    slug(name)?;
    slug(&run_name(name, u64::MAX))?;
    let id = run_name(name, 0);
    let target = space.run(&id)?;
    let brief = brief.trim();
    ensure!(!brief.is_empty(), "a brief is required");
    let meta = directory(&space.root, &format!("{RECORDS}/{name}"))?;
    fs::create_dir_all(&meta)?;
    let _lock = lock_file(&file(&meta, "lock")?, "recurring task")?;
    let path = file(&meta, "head.json")?;
    let previous = Head::read(&path, name)?;
    let pointer_exists = previous.is_some();
    let previous = match previous {
        Some(head) => Some(head),
        None => recover_start(space, name)?,
    };
    if let Some(head) = &previous {
        ensure!(
            head.sequence == 0 && head.seed == id,
            "recurring task {name} already exists; use refresh to open it or choose a new name"
        );
    } else {
        ensure!(!target.try_exists()?, "initial run {id} already exists");
    }
    let canonical;
    let inputs = if let InputSelection::Source(folder) = inputs {
        canonical = folder
            .canonicalize()
            .with_context(|| format!("open source folder {}", folder.display()))?;
        InputSelection::Source(&canonical)
    } else {
        inputs
    };
    let (snapshot, workflow) = match definition {
        Definition::Inbox(path) => (Snapshot::from_definition(space, path)?, None),
        Definition::Saved(name) => (Snapshot::load(&space.workflow(name)?)?, Some(name)),
    };
    let files = inputs.resolve(&space.root, &external_inputs(&snapshot.stages))?;
    let submitted = input_hashes(&files)?;
    let request = hash(&serde_json::to_vec(&(
        brief,
        snapshot.digest()?,
        &submitted,
        batch_review,
    ))?);
    let request = match inputs {
        InputSelection::Source(folder) => hash(&serde_json::to_vec(&(request, folder))?),
        _ => request,
    };
    let request = match workflow {
        Some(name) => hash(&serde_json::to_vec(&(request, name))?),
        None => request,
    };

    if let Some(head) = previous {
        let state = head.current(space)?;
        ensure!(
            state
                .recurring_start
                .as_ref()
                .is_some_and(|start| start.task == name
                    && start.request == request
                    && start.submitted == head.submitted),
            "recurring task {name} already exists with a different start request; choose a new name"
        );
        if !pointer_exists {
            head.write(&path)?;
        }
        println!("This task's initial submission is already recorded.");
        return show(space, name, &head.run);
    }

    let scratch = directory(&space.root, ".anydock")?;
    let temp = tempfile::tempdir_in(scratch)?;
    let staged = temp.path();
    quick::save(&file(staged, BRIEF)?, brief)?;
    snapshot.write(staged)?;
    for (name, source) in files.files {
        copy_exact(
            &file(files.root, &source)?,
            &file(staged, name)?,
            &submitted[name],
        )?;
    }
    let plan = Plan::load(staged)?;
    let mut state = State::new(workflow.map(str::to_owned))?;
    state.batch_review = batch_review;
    state.set_recurring_start(RecurringStart {
        task: name.into(),
        request,
        submitted: submitted.clone(),
    });
    let (stage, attempt) = quick::begin_staged(staged, &plan, &mut state)?;
    sync_dir(meta.parent().context("missing recurring records folder")?)?;
    let identity = state
        .identity
        .clone()
        .context("initial run has no identity")?;
    let source_record = file(&meta, "source.json")?;
    if let InputSelection::Source(folder) = inputs {
        write_task_record(&source_record, &folder)?;
    } else {
        clear_source(&meta)?;
    }
    publish_run(&temp, &target, &id, &state)?;
    fault("after-begin-publish");
    Head {
        version: 1,
        seed: id.clone(),
        run: id.clone(),
        identity,
        sequence: 0,
        submitted,
    }
    .write(&path)?;
    println!("Recurring task {name} | Current run: {id}");
    println!("Stage {} | attempt {attempt}", stage.id);
    for path in &stage.fields.outputs {
        println!("Write: runs/{id}/{path}");
    }
    println!(
        "After verification: {} complete {id} {} --attempt {attempt}",
        crate::workspace::invocation(&space.root),
        stage.id
    );
    stage_context::show(&target, &id, &plan, stage, state.input_format(stage))
}

fn recover_start(space: &Workspace, name: &str) -> Result<Option<Head>> {
    let run = run_name(name, 0);
    let root = space.run(&run)?;
    let Some(state) = LockedRun::inspect(&root)? else {
        return Ok(None);
    };
    let Some(start) = &state.recurring_start else {
        return Ok(None);
    };
    ensure!(
        start.task == name,
        "initial run {run} belongs to a different recurring task"
    );
    state.verify_archives(&root, false)?;
    Ok(Some(Head {
        version: 1,
        seed: run.clone(),
        run,
        identity: state.identity.context("initial run has no identity")?,
        sequence: 0,
        submitted: start.submitted.clone(),
    }))
}

fn read_source(space: &Workspace, name: &str) -> Result<Option<PathBuf>> {
    let record = file(&space.root, &format!("{RECORDS}/{name}/source.json"))?;
    optional(fs::read(record))?
        .map(|bytes| {
            serde_json::from_slice(&bytes)
                .with_context(|| format!("read source folder for recurring task {name}"))
        })
        .transpose()
}

fn clear_source(meta: &Path) -> Result<()> {
    if optional(fs::remove_file(file(meta, "source.json")?))?.is_some() {
        sync_dir(meta)?;
    }
    Ok(())
}

fn input_hashes(inputs: &InputFiles<'_, '_>) -> Result<Hashes> {
    let hashes = inputs
        .files
        .iter()
        .map(|(name, path)| {
            let digest = live_hash(inputs.root, path)?.with_context(|| {
                format!(
                    "missing submitted input {}",
                    inputs.root.join(path).display()
                )
            })?;
            Ok(((*name).to_owned(), digest))
        })
        .collect::<Result<_>>()?;
    validate_input_hashes(&hashes)?;
    Ok(hashes)
}

fn packet_revision(name: &str, predecessor: &str, inputs: &Hashes) -> Result<String> {
    Ok(hash(&serde_json::to_vec(&(
        "anydock-source-v1",
        name,
        predecessor,
        inputs,
    ))?))
}

pub struct Request<'a> {
    pub name: &'a str,
    pub from: Option<&'a str>,
    pub inputs: Option<InputSelection<'a>>,
    pub submit_saved: bool,
    pub source_revision: Option<&'a str>,
    pub pending: bool,
    pub since: Option<&'a str>,
}

pub fn run(space: &Workspace, request: Request<'_>) -> Result<()> {
    let Request {
        name,
        from,
        inputs,
        submit_saved,
        source_revision,
        pending,
        since,
    } = request;
    let submitting = inputs.is_some() || submit_saved;
    ensure!(
        !pending || submitting,
        "provide --input, --inbox, or --submit for a pending refresh"
    );
    ensure!(
        source_revision.is_none_or(is_hash),
        "invalid source revision; use the exact revision from the daily brief"
    );
    slug(&run_name(name, u64::MAX))?;
    slug(name)?;
    let meta = directory(&space.root, &format!("{RECORDS}/{name}"))?;
    if !meta.try_exists()? {
        ensure!(
            from.is_some(),
            "recurring task {name} is not registered; use --from with a completed run"
        );
        fs::create_dir_all(&meta)?;
    }
    let _lock = lock_file(&file(&meta, "lock")?, "recurring task")?;
    let path = file(&meta, "head.json")?;
    let existing = Head::read(&path, name)?;
    let mut head = if let Some(head) = existing {
        ensure!(
            from.is_none_or(|id| id == head.seed),
            "{name} is already tracked from {}",
            head.seed
        );
        head
    } else if let Some(head) = recover_start(space, name)? {
        ensure!(
            from.is_none_or(|id| id == head.seed),
            "{name} is already tracked from {}",
            head.seed
        );
        head.write(&path)?;
        println!("Recovered published initial run {}.", head.run);
        head
    } else {
        let candidate = seed(
            space,
            from.context("choose a completed seed run with --from")?,
        )?;
        let (_, first) = candidate.next(name)?;
        ensure!(
            !space.run(&first)?.try_exists()?,
            "recurring destination {first} already exists"
        );
        sync_dir(meta.parent().context("missing recurring records folder")?)?;
        clear_source(&meta)?;
        candidate.write(&path)?;
        candidate
    };

    let (next, id) = head.next(name)?;
    if let Some(published) = head.published(space, next, &id)? {
        head.current(space)?;
        published.write(&path)?;
        println!("Recovered published refresh {}.", published.run);
        head = published;
    }

    if let Some(revision) = source_revision
        && let Some(update) = head.current(space)?.update
        && update.submission.as_ref() == Some(&head.submitted)
        && revision == packet_revision(name, &update.identity, &head.submitted)?
    {
        println!("This source revision is already recorded.");
        return show(space, name, &head.run);
    }

    if !submitting {
        head.current(space)?;
        return show(space, name, &head.run);
    }
    let saved;
    let inputs = if submit_saved {
        saved = read_source(space, name)?
            .with_context(|| format!("recurring task {name} has no saved source folder"))?;
        println!("Submitting from: {}", saved.display());
        InputSelection::Source(&saved)
    } else {
        inputs.context("missing refresh inputs")?
    };

    let (next, id) = head.next(name)?;
    let created = crate::with_run(space, &head.run, |root, _, state| {
        head.verify(state)?;
        let plan = Plan::load(root)?;
        let external = plan.external_inputs();
        let files = inputs.resolve(&space.root, &external)?;
        let mut submitted = input_hashes(&files)?;
        for path in external
            .into_iter()
            .filter(|path| !files.files.contains_key(path))
        {
            let digest = live_hash(root, path)?
                .with_context(|| format!("missing inherited input {path}"))?;
            submitted.insert(path.to_owned(), digest);
        }
        if let Some(revision) = source_revision {
            ensure!(
                revision == packet_revision(name, &head.identity, &submitted)?,
                "the observed source revision is no longer current for {name}; regenerate the daily brief before submitting"
            );
        }
        if head.submitted == submitted {
            ensure!(
                since.is_none_or(|token| state
                    .update
                    .as_ref()
                    .and_then(|update| update.presentation.as_ref())
                    .is_some_and(|presentation| presentation.token == token)),
                "this packet is already tracked with a different review comparison"
            );
            return Ok(None);
        }
        let identity = update::create(
            space,
            root,
            state,
            update::Request {
                from: &head.run,
                id: &id,
                inputs,
                pending,
                since,
                submission: Some(&submitted),
                source_plan: Some(&plan),
            },
        )?;
        Ok(Some((identity, submitted)))
    })?;
    let Some((identity, submitted)) = created else {
        println!("This submission is already recorded.");
        return show(space, name, &head.run);
    };

    fault("after-refresh-publish");
    Head {
        run: id.clone(),
        identity,
        sequence: next,
        submitted,
        ..head
    }
    .write(&path)?;
    println!("Recurring task {name} | Current run: {id}");
    Ok(())
}

fn show(space: &Workspace, name: &str, id: &str) -> Result<()> {
    println!("Recurring task {name} | Current run: {id}");
    handoff::show(&space.run(id)?, id, false)
}

pub fn with_current<T>(
    space: &Workspace,
    name: &str,
    operation: impl FnOnce(&str) -> Result<T>,
) -> Result<T> {
    slug(name)?;
    let meta = directory(&space.root, &format!("{RECORDS}/{name}"))?;
    ensure!(
        meta.try_exists()?,
        "recurring task {name} is not registered"
    );
    let _lock = lock_file(&file(&meta, "lock")?, "recurring task")?;
    let task = inspect(space, name)?
        .with_context(|| format!("recurring task {name} is not registered"))?;
    operation(&task.run)
}

pub fn tracked_names(space: &Workspace) -> Result<Vec<String>> {
    names(&space.root, RECORDS)
}

pub struct Current {
    pub run: String,
    pub state: State,
    submitted: Hashes,
}

pub struct SourceChanges {
    pub source: PathBuf,
    pub files: Hashes,
    pub revision: String,
}

impl Current {
    pub fn changed_sources(&self, space: &Workspace, name: &str) -> Result<Option<SourceChanges>> {
        let Some(source) = read_source(space, name)? else {
            return Ok(None);
        };
        let paths = self.submitted.keys().map(String::as_str).collect();
        let files = InputSelection::Source(&source).resolve(&space.root, &paths)?;
        let hashes = input_hashes(&files)?;
        let changed: Hashes = files
            .files
            .into_iter()
            .filter(|(name, _)| hashes[*name] != self.submitted[*name])
            .map(|(name, path)| (path, hashes[name].clone()))
            .collect();
        if changed.is_empty() {
            return Ok(None);
        }
        let identity = self
            .state
            .identity
            .as_deref()
            .context("missing run identity")?;
        Ok(Some(SourceChanges {
            source,
            files: changed,
            revision: packet_revision(name, identity, &hashes)?,
        }))
    }
}

pub fn inspect(space: &Workspace, name: &str) -> Result<Option<Current>> {
    let path = file(&space.root, &format!("{RECORDS}/{name}/head.json"))?;
    let head = match Head::read(&path, name)? {
        Some(head) => head,
        None => match recover_start(space, name)? {
            Some(head) => head,
            None => return Ok(None),
        },
    };
    let current = head.current(space)?;
    let (next, id) = head.next(name)?;
    if let Some(published) = head.published(space, next, &id)? {
        let state = published.current(space)?;
        return Ok(Some(Current {
            run: published.run,
            state,
            submitted: published.submitted,
        }));
    }
    Ok(Some(Current {
        run: head.run,
        state: current,
        submitted: head.submitted,
    }))
}

pub fn check(space: &Workspace) -> Result<()> {
    for name in tracked_names(space)? {
        inspect(space, &name)?;
    }
    Ok(())
}
