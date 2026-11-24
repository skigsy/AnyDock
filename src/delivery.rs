use crate::{
    engine::{self, Phase},
    policy,
    storage::{State, copy_exact, hash, sync_dir, verified_blob},
    workspace::{Plan, directory, file, slug, validate_ref},
};
use anyhow::{Context, Result, ensure};
use serde::Serialize;
use std::{collections::BTreeMap, fs, io::Write, path::Path};

#[derive(Serialize)]
struct Item {
    file: String,
    sha256: String,
    archive: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    checks: Vec<String>,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    guidance: BTreeMap<String, String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    decision: Option<String>,
}

pub struct Request<'a> {
    pub name: &'a str,
    pub include: &'a [String],
    pub guidance: bool,
    pub decisions: bool,
    pub confirm: Option<&'a str>,
}

pub fn run(run: &Path, id: &str, state: &mut State, request: Request<'_>) -> Result<()> {
    slug(request.name)?;
    let plan = Plan::load(run)?;
    state.verify_archives(run, false)?;
    policy::require_current(state, &plan)?;
    let view = engine::evaluate(run, &plan, state, false)?;
    let mut found = BTreeMap::new();
    for name in request.include {
        validate_ref(name)?;
        ensure!(!found.contains_key(name), "repeated included file {name}");
        let producer = plan
            .producers
            .get(name)
            .with_context(|| format!("{name} is not a declared handoff"))?;
        let item = view
            .stages
            .iter()
            .find(|item| &item.stage.id == producer)
            .context("missing producer stage")?;
        ensure!(
            item.phase == Phase::Done,
            "{name} is {}; resolve this run before delivery",
            item.phase.label()
        );
        let id = item
            .revision
            .as_deref()
            .context("missing recorded handoff")?;
        let revision = &state.stages[producer].revisions[id];
        let sha256 = revision
            .outputs
            .get(name)
            .context("missing archived output")?;
        let guidance = if request.guidance {
            revision
                .basis
                .iter()
                .filter(|(path, _)| path.starts_with("references/"))
                .map(|(path, input)| (path.to_owned(), input.hash.clone()))
                .collect()
        } else {
            BTreeMap::new()
        };
        found.insert(
            name,
            Item {
                file: format!("files/{name}"),
                sha256: sha256.to_owned(),
                archive: id.to_owned(),
                checks: revision
                    .checks
                    .iter()
                    .filter(|check| check.split_once(' ').is_some_and(|(_, path)| path == name))
                    .cloned()
                    .collect(),
                guidance,
                decision: request
                    .decisions
                    .then(|| revision.decision.clone())
                    .flatten(),
            },
        );
    }
    ensure!(!found.is_empty(), "select at least one declared handoff");
    let manifest = serde_json::to_string_pretty(&found.values().collect::<Vec<_>>())?;
    let identity = state
        .identity
        .as_deref()
        .context("use status to update this legacy run first")?;
    let token = hash(&serde_json::to_vec(&(identity, request.name, &manifest))?);
    let destination = directory(run, &format!("deliveries/{}", request.name))?;
    println!("Delivery preview for {id} / {}", request.name);
    println!("{manifest}");
    println!("Confirmation: {token}");
    let Some(confirmation) = request.confirm else {
        println!("To write this packet, repeat the selection with --confirm {token}.");
        return Ok(());
    };
    ensure!(
        confirmation == token,
        "delivery preview changed; review the current selection"
    );
    ensure!(
        !destination.try_exists()?,
        "delivery {} already exists",
        request.name
    );
    let scratch = directory(run, ".anydock")?;
    let temp = tempfile::tempdir_in(scratch)?;
    for item in found.values() {
        copy_exact(
            &verified_blob(run, &item.sha256)?,
            &file(temp.path(), &item.file)?,
            &item.sha256,
        )?;
    }
    let mut summary = fs::File::create_new(file(temp.path(), "delivery.json")?)?;
    writeln!(summary, "{manifest}")?;
    summary.sync_all()?;
    drop(summary);
    let parent = destination
        .parent()
        .context("missing deliveries directory")?;
    fs::create_dir_all(parent)?;
    fs::rename(temp.path(), &destination).context("write local delivery packet")?;
    sync_dir(parent)?;
    println!(
        "Wrote local delivery packet to runs/{id}/deliveries/{}/.",
        request.name
    );
    Ok(())
}
