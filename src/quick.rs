use crate::{
    engine, policy,
    storage::{State, copy_exact, fingerprint, live_hash, publish_run, sync_dir},
    workspace::{Plan, Stage, Workspace, directory, file, input_component, segment, validate_ref},
};
use anyhow::{Context, Result, ensure};
use serde_json::json;
use std::{
    collections::BTreeSet,
    fs,
    io::Write,
    path::{Path, PathBuf},
};

pub struct Request<'a> {
    pub run: &'a str,
    pub brief: &'a str,
    pub work: &'a str,
    pub input: &'a [String],
    pub sources: &'a [PathBuf],
    pub output: &'a str,
    pub review: Option<&'a str>,
}

pub fn create(space: &Workspace, request: Request<'_>) -> Result<()> {
    let target = space.run(request.run)?;
    ensure!(!target.try_exists()?, "run {} already exists", request.run);
    segment(request.output)?;
    ensure!(
        !request.brief.trim().is_empty() && !request.work.trim().is_empty(),
        "brief and work are required"
    );
    let scratch = directory(&space.root, ".anydock")?;
    fs::create_dir_all(&scratch)?;
    let temp = tempfile::tempdir_in(scratch)?;
    let staged = temp.path();
    let mut inputs = Vec::new();
    let mut seen = BTreeSet::new();
    for source in request.input {
        validate_ref(source)?;
        let name = source
            .strip_prefix("inbox/")
            .context("quick imports files from inbox/")?;
        let dest = format!("inputs/{name}");
        ensure!(seen.insert(source), "repeated input {source}");
        let digest =
            live_hash(&space.root, source)?.with_context(|| format!("missing input {source}"))?;
        copy_exact(&file(&space.root, source)?, &file(staged, &dest)?, &digest)?;
        inputs.push(dest);
    }
    for selected in request.sources {
        let source = space.start.join(selected);
        let name = source
            .file_name()
            .and_then(|name| name.to_str())
            .context("expected a UTF-8 source filename")?;
        input_component(name)?;
        let canonical = source
            .parent()
            .context("source has no parent folder")?
            .canonicalize()?;
        let path = file(&canonical, name)?;
        ensure!(
            path.is_file(),
            "source file does not exist: {}",
            selected.display()
        );
        let dest = format!("inputs/{name}");
        ensure!(
            !inputs
                .iter()
                .any(|other| other.to_lowercase() == dest.to_lowercase()),
            "repeated destination {dest}"
        );
        copy_exact(
            &path,
            &file(staged, &dest)?,
            &fingerprint(fs::File::open(&path)?)?,
        )?;
        inputs.push(dest);
    }
    let output = format!("stages/01-work/outputs/{}", request.output);
    let fields = json!({"inputs": inputs, "outputs": [&output], "review": request.review});
    let tick = "\x60\x60\x60";
    let contract = format!(
        "# Work\n\n{tick}anydock\n{fields}\n{tick}\n\n## Work\n\n{}\n",
        request.work.trim()
    );
    save(file(staged, "brief.md")?.as_path(), request.brief.trim())?;
    save(&file(staged, "stages/01-work/stage.md")?, &contract)?;
    fs::create_dir_all(directory(staged, "stages/01-work/outputs")?)?;
    let plan = Plan::load(staged)?;
    let mut state = State::new(None)?;
    policy::capture(&mut state, &plan);
    let view = engine::evaluate(staged, &plan, &mut state, false)?;
    let item = view.front().context("missing one-stage work")?;
    let attempt = engine::begin(&mut state, item, false)?;
    publish_run(&temp, &target, request.run, &state)?;
    println!("Stage 01-work | attempt {attempt}");
    for input in inputs {
        println!("Read: runs/{}/{input}", request.run);
    }
    println!("Write: runs/{}/{output}", request.run);
    print_completion(space, request.run, "01-work", attempt);
    Ok(())
}

pub fn save(path: &Path, content: &str) -> Result<()> {
    let parent = path.parent().context("missing destination folder")?;
    fs::create_dir_all(parent)?;
    let mut file = fs::File::create_new(path)?;
    writeln!(file, "{content}")?;
    file.sync_all()?;
    sync_dir(parent)
}

pub fn repeat(
    space: &Workspace,
    workflow: &str,
    id: &str,
    brief: &str,
    mappings: &[String],
) -> Result<()> {
    let target = space.run(id)?;
    ensure!(!target.try_exists()?, "run {id} already exists");
    ensure!(!brief.trim().is_empty(), "a fresh brief is required");
    let scratch = directory(&space.root, ".anydock")?;
    fs::create_dir_all(&scratch)?;
    let temp = tempfile::tempdir_in(scratch)?;
    let staged = temp.path();
    save(&file(staged, "brief.md")?, brief.trim())?;
    crate::workflow::instantiate(space, staged, workflow)?;
    let plan = Plan::load(staged)?;

    let required = plan.external_inputs();
    let mut selected = BTreeSet::new();
    for mapping in mappings {
        let (source, named_target) = mapping
            .split_once('=')
            .map_or((mapping.as_str(), None), |(from, to)| (from, Some(to)));
        validate_ref(source)?;
        let name = source
            .strip_prefix("inbox/")
            .context("repeat imports files from inbox/")?;
        let default = format!("inputs/{name}");
        let destination = named_target.unwrap_or(&default);
        let destination = *required
            .get(destination)
            .with_context(|| format!("{destination} is not a declared workflow input"))?;
        ensure!(
            selected.insert(destination),
            "repeated destination {destination}"
        );
        let digest =
            live_hash(&space.root, source)?.with_context(|| format!("missing input {source}"))?;
        copy_exact(
            &file(&space.root, source)?,
            &file(staged, destination)?,
            &digest,
        )?;
    }
    let missing: Vec<_> = required.difference(&selected).copied().collect();
    ensure!(
        missing.is_empty(),
        "map the required inputs: {}",
        missing.join(", ")
    );

    let mut state = State::new(Some(workflow.to_owned()))?;
    let (stage, attempt) = begin_staged(staged, &plan, &mut state)?;
    publish_run(&temp, &target, id, &state)?;
    println!("Stage {} | attempt {attempt}", stage.id);
    print_completion(space, id, &stage.id, attempt);
    crate::stage_context::show(&target, id, &plan, stage, state.input_format(stage))
}

pub fn begin_staged<'a>(run: &Path, plan: &'a Plan, state: &mut State) -> Result<(&'a Stage, u64)> {
    policy::capture(state, plan);
    let view = engine::evaluate(run, plan, state, false)?;
    let item = view.front().context("workflow has no stage to begin")?;
    for name in &item.stage.fields.outputs {
        let output = file(run, name)?;
        fs::create_dir_all(output.parent().context("missing stage output folder")?)?;
    }
    let attempt = engine::begin(state, item, false)?;
    Ok((item.stage, attempt))
}

fn print_completion(space: &Workspace, id: &str, stage: &str, attempt: u64) {
    let command = crate::workspace::invocation(&space.root);
    println!("After verification: {command} complete {id} {stage} --attempt {attempt}");
}
