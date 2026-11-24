use crate::{
    storage::{Hashes, LockedRun, State, copy_exact, hash, live_hash, lock_file, sync_dir},
    workspace::{BRIEF, Plan, Workspace, directory, file, names, slug},
};
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeSet,
    fs::{self, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
};

const STATE: &str = ".anydock/state.json";
const MARKER: &str = ".anydock/registered";
const MARKER_BYTES: &[u8] = b"anydock-managed\n";

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Manifest {
    version: u32,
    run: String,
    identity: String,
    files: Hashes,
}

pub fn pack(
    space: &Workspace,
    source: &Path,
    run: &str,
    state: &mut State,
    bundle: &str,
) -> Result<()> {
    let destination = bundle_path(space, bundle)?;
    ensure!(!destination.try_exists()?, "bundle {bundle} already exists");
    state.verify_archives(source, true)?;
    let plan = Plan::load(source)?;
    let snapshot = live_files(source, &plan, state)?;
    let parent = destination.parent().context("missing bundle collection")?;
    fs::create_dir_all(parent)?;
    let staged = tempfile::tempdir_in(scratch(space)?)?;
    let contents = directory(staged.path(), "files")?;
    fs::create_dir(&contents)?;
    copy_files(source, &contents, &snapshot)?;

    // Keep local launch records outside the portable run.
    let offer = state.switch_offer.take();
    let receipt = state.switch_receipt.take();
    let encoded = serde_json::to_vec_pretty(state);
    state.switch_offer = offer;
    state.switch_receipt = receipt;
    let encoded = encoded?;
    let meta = directory(&contents, ".anydock")?;
    fs::create_dir_all(&meta)?;
    write(&file(&contents, STATE)?, &encoded)?;
    write(&file(&contents, MARKER)?, MARKER_BYTES)?;
    ensure!(
        snapshot == live_files(source, &Plan::load(source)?, state)?,
        "run files changed during packing. Retry after saving the working files"
    );

    let mut manifest = Manifest {
        version: 1,
        run: run.into(),
        identity: state.identity.clone().context("missing run identity")?,
        files: snapshot,
    };
    manifest.files.insert(STATE.into(), hash(&encoded));
    manifest.files.insert(MARKER.into(), hash(MARKER_BYTES));
    verify(&contents, &manifest)?;
    write(
        &file(staged.path(), "manifest.json")?,
        &serde_json::to_vec_pretty(&manifest)?,
    )?;
    publish(staged.path(), &destination)?;
    println!(
        "Packed {run} to bundles/{bundle}/ ({} files).",
        manifest.files.len()
    );
    Ok(())
}

pub fn unpack(space: &Workspace, bundle: &str, run: &str) -> Result<()> {
    let source = bundle_path(space, bundle)?;
    let manifest: Manifest = serde_json::from_slice(&fs::read(file(&source, "manifest.json")?)?)
        .context("read bundle manifest")?;
    ensure!(manifest.version == 1, "unsupported bundle version");
    slug(&manifest.run)?;
    let contents = directory(&source, "files")?;
    let destination = space.run(run)?;
    let scratch = scratch(space)?;
    // Serialize identity checks and installation across simultaneous imports.
    let _lock = lock_file(&file(&scratch, "bundle-import.lock")?, "bundle imports")?;
    ensure!(!destination.try_exists()?, "run {run} already exists");
    for existing in names(&space.root, "runs")? {
        if let Some(state) = LockedRun::inspect(&space.run(&existing)?)? {
            ensure!(
                state.identity.as_deref() != Some(&manifest.identity),
                "bundle identity is already installed as run {existing}"
            );
        }
    }

    let staged = tempfile::tempdir_in(scratch)?;
    copy_files(&contents, staged.path(), &manifest.files)?;
    verify(staged.path(), &manifest)?;
    fs::create_dir_all(destination.parent().context("missing runs collection")?)?;
    publish(staged.path(), &destination)?;
    println!(
        "Installed bundle {bundle} as {run}. Run {} handoff {run}.",
        crate::workspace::invocation(&space.root)
    );
    Ok(())
}

fn scratch(space: &Workspace) -> Result<PathBuf> {
    let scratch = directory(&space.root, ".anydock")?;
    fs::create_dir_all(&scratch)?;
    Ok(scratch)
}

fn bundle_path(space: &Workspace, name: &str) -> Result<PathBuf> {
    slug(name)?;
    directory(&space.root, &format!("bundles/{name}"))
}

fn live_files(run: &Path, plan: &Plan, state: &State) -> Result<Hashes> {
    let mut files = Hashes::from([(BRIEF.into(), hash(&plan.brief))]);
    if let Some(digest) = live_hash(run, "notes.md")? {
        files.insert("notes.md".into(), digest);
    }
    let mut names = BTreeSet::new();
    for stage in &plan.stages {
        files.extend(stage.definition_hashes());
        names.extend(stage.fields.inputs.iter());
        names.extend(stage.fields.outputs.iter());
        names.extend(
            stage
                .fields
                .references
                .iter()
                .map(|reference| &reference.path),
        );
    }
    for name in names {
        if let Some(digest) = live_hash(run, name)? {
            files.insert(name.clone(), digest);
        }
    }
    for digest in state
        .stages
        .values()
        .flat_map(|entry| entry.revisions.values())
        .flat_map(|revision| revision.outputs.values())
        .chain(
            state
                .update
                .iter()
                .filter_map(|update| update.presentation.as_ref())
                .flat_map(|presentation| presentation.digests()),
        )
    {
        files.insert(format!(".anydock/blobs/{digest}"), digest.clone());
    }
    Ok(files)
}

fn verify(run: &Path, manifest: &Manifest) -> Result<()> {
    let state = LockedRun::inspect(run)?.context("bundle is missing managed state")?;
    ensure!(
        state.identity.as_deref() == Some(&manifest.identity),
        "bundle identity mismatch"
    );
    ensure!(
        state.switch_offer.is_none() && state.switch_receipt.is_none(),
        "bundle contains a local launch record"
    );
    ensure!(
        fs::read(file(run, MARKER)?)? == MARKER_BYTES,
        "invalid managed marker in bundle"
    );
    state.verify_archives(run, true)?;
    let mut expected = live_files(run, &Plan::load(run)?, &state)?;
    expected.insert(
        STATE.into(),
        live_hash(run, STATE)?.context("missing state file")?,
    );
    expected.insert(MARKER.into(), hash(MARKER_BYTES));
    ensure!(
        expected == manifest.files,
        "bundle files do not match the manifest and declared run data"
    );
    Ok(())
}

fn copy_files(source: &Path, destination: &Path, files: &Hashes) -> Result<()> {
    for (name, digest) in files {
        copy_exact(&file(source, name)?, &file(destination, name)?, digest)
            .with_context(|| format!("verify and copy {name}"))?;
    }
    Ok(())
}

fn write(path: &Path, bytes: &[u8]) -> Result<()> {
    let mut file = OpenOptions::new().write(true).create_new(true).open(path)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    sync_dir(path.parent().context("missing destination folder")?)
}

fn publish(source: &Path, destination: &Path) -> Result<()> {
    ensure!(!destination.try_exists()?, "destination already exists");
    sync_dir(source)?;
    fs::rename(source, destination)
        .with_context(|| format!("publish {}", destination.display()))?;
    sync_dir(destination.parent().context("missing destination folder")?)
}
