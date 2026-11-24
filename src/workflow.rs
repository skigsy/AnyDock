use crate::{
    reference,
    storage::{LockedRun, hash, sync_dir},
    workspace::{
        Plan, Stage, Workspace, directory, file, load_stages, render_contract, segment,
        validate_stages,
    },
};
use anyhow::{Context, Result, ensure};
use std::{collections::BTreeMap, fs, io::Write, path::Path};

pub fn save(space: &Workspace, id: &str, name: &str) -> Result<()> {
    let run = space.run(id)?;
    let _lock = LockedRun::acquire(&run, false)?;
    let plan = Plan::load(&run)?;
    let references = read_references(&run, &plan.stages)?;
    let dest = space.workflow(name)?;
    ensure!(!dest.try_exists()?, "workflow {name} already exists");
    let scratch = directory(&space.root, ".anydock")?;
    fs::create_dir_all(&scratch)?;
    let temp = tempfile::tempdir_in(scratch)?;
    publish(
        space,
        &dest,
        temp.path(),
        &Snapshot {
            stages: plan.stages,
            references,
        },
    )?;
    println!("Saved workflow definitions to workflows/{name}.");
    Ok(())
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Definition {
    stages: Vec<StageDefinition>,
}

#[derive(serde::Deserialize)]
struct StageDefinition {
    id: String,
    work: String,
    #[serde(flatten)]
    fields: serde_json::Map<String, serde_json::Value>,
}

pub fn define(space: &Workspace, name: &str, source: &str) -> Result<()> {
    let snapshot = Snapshot::from_definition(space, source)?;
    let dest = space.workflow(name)?;
    ensure!(!dest.try_exists()?, "workflow {name} already exists");
    let scratch = directory(&space.root, ".anydock")?;
    fs::create_dir_all(&scratch)?;
    let temp = tempfile::tempdir_in(scratch)?;
    publish(space, &dest, temp.path(), &snapshot)?;
    println!(
        "Defined {} stages in workflows/{name}.",
        snapshot.stages.len()
    );
    Ok(())
}

fn publish(space: &Workspace, dest: &Path, staged: &Path, snapshot: &Snapshot) -> Result<()> {
    snapshot.write(staged)?;
    validate(staged)?;
    let base = directory(&space.root, "workflows")?;
    fs::create_dir_all(&base)?;
    fs::rename(staged, dest).with_context(|| format!("publish {}", dest.display()))?;
    sync_dir(&base)
}

pub fn validate(folder: &Path) -> Result<()> {
    Snapshot::load(folder)?;
    Ok(())
}

pub fn instantiate(space: &Workspace, run: &Path, name: &str) -> Result<()> {
    let source = space.workflow(name)?;
    let Snapshot { stages, references } =
        Snapshot::load(&source).with_context(|| format!("open workflow {name}"))?;
    let dest = directory(run, "stages")?;
    if dest.try_exists()? {
        let existing = load_stages(run)?;
        ensure!(
            existing.len() == stages.len()
                && existing
                    .iter()
                    .zip(&stages)
                    .all(|(a, b)| a.id == b.id && a.contract == b.contract && a.format == b.format),
            "run already has different stage definitions"
        );
    }

    // A retry reuses matching copies. It never replaces a file the user edited.
    for (path, bytes) in references {
        if let Some(existing) = reference::bytes(run, &path)? {
            ensure!(
                existing == bytes,
                "run already contains different guidance at {path}"
            );
        } else {
            reference::write(run, &path, &bytes, false)?;
        }
    }
    if !dest.try_exists()? {
        let scratch = directory(run, ".anydock")?;
        fs::create_dir_all(&scratch)?;
        let temp = tempfile::tempdir_in(scratch)?;
        write_definitions(temp.path(), &stages)?;
        fs::rename(temp.path().join("stages"), dest)
            .context("copy workflow stages into the run")?;
        sync_dir(run)?;
    }
    Ok(())
}

pub fn adopt_reference(
    space: &Workspace,
    run: &Path,
    plan: &Plan,
    workflow: &str,
    name: &str,
) -> Result<()> {
    let uses: Vec<_> = plan
        .stages
        .iter()
        .flat_map(|stage| &stage.fields.references)
        .filter(|reference| reference.path == name)
        .collect();
    ensure!(
        !uses.is_empty(),
        "{name:?} is not a declared workflow reference in this run"
    );
    let source = space.workflow(workflow)?;
    let bytes = reference::bytes(&source, name)?
        .with_context(|| format!("missing workflows/{workflow}/{name}"))?;
    for reference in uses {
        reference.digest(&bytes)?;
    }
    if reference::bytes(run, name)?.as_deref() != Some(&bytes) {
        reference::write(run, name, &bytes, true)?;
    }
    Ok(())
}

pub fn guidance_path(stage: &str) -> String {
    format!("guidance/{stage}/review.md")
}

pub struct Snapshot {
    pub stages: Vec<Stage>,
    references: BTreeMap<String, Vec<u8>>,
}

impl Snapshot {
    pub fn from_definition(space: &Workspace, source: &str) -> Result<Self> {
        ensure!(
            source.starts_with("inbox/"),
            "put the definition under inbox/"
        );
        let source = file(&space.root, source)?;
        let root = source.parent().context("missing definition folder")?;
        let definition: Definition =
            serde_json::from_slice(&fs::read(&source)?).context("parse workflow definition")?;
        let mut stages = Vec::new();
        for stage in definition.stages {
            segment(&stage.id)?;
            let contract = render_contract(
                &stage.id,
                &stage.work,
                &serde_json::Value::Object(stage.fields),
            )?;
            stages.push(Stage::read(stage.id, contract.into_bytes())?);
        }
        let references = read_references(root, &stages)?;
        Ok(Self { stages, references })
    }

    pub fn write(&self, staged: &Path) -> Result<()> {
        write_definitions(staged, &self.stages)?;
        for (path, bytes) in &self.references {
            reference::write(staged, path, bytes, false)?;
        }
        Ok(())
    }

    pub fn digest(&self) -> Result<String> {
        let stages: Vec<_> = self
            .stages
            .iter()
            .flat_map(Stage::definition_hashes)
            .collect();
        let references: BTreeMap<_, _> = self
            .references
            .iter()
            .map(|(path, bytes)| (path, hash(bytes)))
            .collect();
        Ok(hash(&serde_json::to_vec(&(stages, references))?))
    }

    pub fn load(folder: &Path) -> Result<Self> {
        let mut stages = load_stages(folder)?;
        validate_stages(folder, &stages)?;
        let mut references = read_references(folder, &stages)?;
        for stage in &mut stages {
            let Some(bytes) = reference::bytes(folder, &guidance_path(&stage.id))? else {
                continue;
            };
            if std::str::from_utf8(&bytes)?.trim().is_empty() {
                continue;
            }
            let target = (1..)
                .map(|number| guidance_reference(&stage.id, number))
                .find(|path| !references.contains_key(path))
                .context("allocate a stage guidance path")?;
            stage.attach_reference(&target)?;
            references.insert(target, bytes);
        }
        Ok(Self { stages, references })
    }
}

fn guidance_reference(stage: &str, number: u32) -> String {
    format!("references/anydock/{stage}/review-{number}.md")
}

pub fn apply_guidance(space: &Workspace, name: &str, run: &Path) -> Result<()> {
    let workflow = space.workflow(name)?;
    if !workflow.try_exists()? {
        println!("Saved workflow {name} is unavailable. Keeping the run's pinned guidance.");
        return Ok(());
    }
    let saved = load_stages(&workflow)?;
    let declared = saved
        .iter()
        .flat_map(|stage| &stage.fields.references)
        .map(|entry| entry.path.as_str())
        .collect::<std::collections::BTreeSet<_>>();
    let plan = Plan::load(run)?;
    for mut stage in plan.stages {
        let guidance = guidance_path(&stage.id);
        let Some(bytes) = reference::bytes(&workflow, &guidance)? else {
            continue;
        };
        if std::str::from_utf8(&bytes)?.trim().is_empty() {
            continue;
        }
        let path = (1..)
            .map(|index| guidance_reference(&stage.id, index))
            .find(|path| !declared.contains(path.as_str()))
            .context("allocate a stage guidance path")?;
        let prior = stage
            .fields
            .references
            .iter()
            .find(|entry| entry.path == path && entry.section.is_none());
        if let Some(prior) = prior {
            if reference::bytes(run, &prior.path)?.as_deref() != Some(&bytes) {
                reference::write(run, &prior.path, &bytes, true)?;
            }
        } else {
            stage.attach_reference(&path)?;
            reference::write(run, &path, &bytes, false)?;
            let path = file(run, &stage.contract_path())?;
            let mut contract = fs::OpenOptions::new()
                .write(true)
                .truncate(true)
                .open(path)?;
            contract.write_all(&stage.contract)?;
            contract.sync_all()?;
        }
    }
    Ok(())
}

fn read_references(root: &Path, stages: &[Stage]) -> Result<BTreeMap<String, Vec<u8>>> {
    let mut result = BTreeMap::new();
    for reference in stages.iter().flat_map(|stage| &stage.fields.references) {
        if !result.contains_key(&reference.path) {
            let bytes = reference::bytes(root, &reference.path)?
                .with_context(|| format!("missing reference {}", reference.path))?;
            result.insert(reference.path.clone(), bytes);
        }
        reference.digest(&result[&reference.path])?;
    }
    Ok(result)
}

fn write_definitions(dest: &Path, stages: &[Stage]) -> Result<()> {
    let base = dest.join("stages");
    fs::create_dir(&base)?;
    for stage in stages {
        let folder = base.join(&stage.id);
        fs::create_dir(&folder)?;
        let file = folder.join("stage.md");
        let mut file = fs::File::create_new(file)?;
        file.write_all(&stage.contract)?;
        file.sync_all()?;
        if let Some(bytes) = stage.format_bytes() {
            crate::workspace::write_new(&folder.join("contract-format"), &bytes)?;
        }
        sync_dir(&folder)?;
    }
    sync_dir(&base)?;
    sync_dir(dest)
}
