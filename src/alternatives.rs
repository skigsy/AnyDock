use crate::{
    engine::{self, Phase},
    policy, review,
    storage::{LockedRun, Origin, State, copy_exact, hash, live_hash, sync_dir, verified_blob},
    workspace::{BRIEF, Plan, Workspace, directory, file},
};
use anyhow::{Context, Result, ensure};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
};

pub fn fork(space: &Workspace, path: &Path, source: &str, dest: &str, state: &State) -> Result<()> {
    let target = space.run(dest)?;
    ensure!(!target.try_exists()?, "run {dest} already exists");
    let plan = Plan::load(path)?;
    policy::require_current(state, &plan)?;
    state.verify_archives(path, false)?;
    let mut snapshot = BTreeMap::from([(BRIEF.to_owned(), hash(&plan.brief))]);
    for stage in &plan.stages {
        snapshot.extend(stage.definition_hashes());
        let names = stage
            .fields
            .inputs
            .iter()
            .filter(|name| name.starts_with("inputs/"))
            .chain(
                stage
                    .fields
                    .references
                    .iter()
                    .map(|reference| &reference.path),
            );
        for name in names {
            if !snapshot.contains_key(name) {
                let digest =
                    live_hash(path, name)?.with_context(|| format!("missing fork input {name}"))?;
                snapshot.insert(name.clone(), digest);
            }
        }
    }
    let scratch = directory(&space.root, ".anydock")?;
    fs::create_dir_all(&scratch)?;
    let staged = tempfile::tempdir_in(scratch)?;
    let copy = staged.path();
    for (name, digest) in &snapshot {
        copy_exact(&file(path, name)?, &file(copy, name)?, digest)?;
    }
    for (name, digest) in &snapshot {
        ensure!(
            live_hash(path, name)?.as_ref() == Some(digest),
            "{name} changed during the fork; retry"
        );
    }
    let copied = Plan::load(copy)?;
    let mut branch = State::new(state.workflow.clone())?;
    branch.origin = Some(Origin {
        run: source.to_owned(),
        identity: state.identity.clone().context("missing source identity")?,
        snapshot: hash(&serde_json::to_vec(&snapshot)?),
    });
    policy::capture(&mut branch, &copied);
    let lock = LockedRun::acquire(copy, true)?.context("initialize fork state")?;
    lock.save(&branch)?;
    lock.ensure_registered_marker()?;
    drop(lock);
    let parent = target.parent().context("missing runs directory")?;
    fs::rename(copy, &target).with_context(|| format!("publish isolated run {dest}"))?;
    sync_dir(parent)?;
    println!(
        "Forked {source} as {dest} from snapshot {}.",
        branch
            .origin
            .as_ref()
            .context("missing fork origin")?
            .snapshot
    );
    println!(
        "Run starts at stage one with its own decisions. Use {} handoff {dest}.",
        crate::workspace::invocation(&space.root)
    );
    Ok(())
}

struct Alternatives {
    origin: Option<Origin>,
    files: BTreeMap<String, ResultFile>,
}

struct ResultFile {
    digest: String,
    path: PathBuf,
    reviewed: bool,
}

impl Alternatives {
    fn load(run: &Path) -> Result<Self> {
        let mut state = LockedRun::inspect(run)?.context("run is not registered")?;
        let plan = Plan::load(run)?;
        state.verify_archives(run, false)?;
        policy::require_current(&state, &plan)?;
        let view = engine::evaluate(run, &plan, &mut state, false)?;
        let mut files = BTreeMap::new();
        for item in &view.stages {
            if matches!(item.phase, Phase::Done | Phase::Review)
                && let Some(id) = &item.revision
            {
                let revision = &state.stages[&item.stage.id].revisions[id];
                for (name, digest) in &revision.outputs {
                    files.insert(
                        name.clone(),
                        ResultFile {
                            digest: digest.clone(),
                            path: verified_blob(run, digest)?,
                            reviewed: item.phase == Phase::Done,
                        },
                    );
                }
            }
        }
        Ok(Self {
            origin: state.origin,
            files,
        })
    }
}

pub fn compare(space: &Workspace, left: &str, right: &str) -> Result<()> {
    ensure!(left != right, "choose two different runs");
    let prior = Alternatives::load(&space.run(left)?)?;
    let next = Alternatives::load(&space.run(right)?)?;
    let same = matches!((&prior.origin, &next.origin), (Some(a), Some(b))
        if a.identity == b.identity && a.snapshot == b.snapshot);
    println!(
        "{left} / {right} | same fork starting files: {}",
        if same { "yes" } else { "unconfirmed" }
    );
    let paths: BTreeSet<_> = prior.files.keys().chain(next.files.keys()).collect();
    ensure!(
        !paths.is_empty(),
        "neither run has a current recorded result"
    );
    for name in paths {
        let old = prior.files.get(name);
        let new = next.files.get(name);
        let describe = |item: Option<&ResultFile>| match item {
            Some(file) if file.reviewed => "current",
            Some(_) => "awaiting review",
            None => "no current recorded output",
        };
        println!(
            "{name} | {left}: {} | {right}: {}",
            describe(old),
            describe(new)
        );
        if let (Some(old), Some(new)) = (old, new) {
            if old.digest == new.digest {
                println!("  Same content.");
            } else {
                review::compare_archives(&old.path, &old.digest, &new.path, &new.digest)?;
            }
        }
    }
    println!(
        "Select the preferred run's eligible files with {} delivery.",
        crate::workspace::invocation(&space.root)
    );
    Ok(())
}
