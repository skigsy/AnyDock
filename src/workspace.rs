use crate::{
    input_scope,
    quality::{self, Check},
    reference::Reference,
};
use anyhow::{Context, Result, bail, ensure};
use serde::Deserialize;
use std::{
    collections::{BTreeMap, BTreeSet},
    fs, io,
    path::{Path, PathBuf},
};

pub const BRIEF: &str = "brief.md";
pub const PROGRESS: &str = "progress.md";

pub struct Workspace {
    pub root: PathBuf,
}

pub struct Stage {
    pub id: String,
    pub contract: Vec<u8>,
    pub fields: Fields,
}

impl Stage {
    pub fn read(id: String, contract: Vec<u8>) -> Result<Self> {
        let fields = parse_fields(&contract).with_context(|| format!("parse stage {id}"))?;
        Ok(Self {
            id,
            contract,
            fields,
        })
    }

    pub fn attach_reference(&mut self, path: &str) -> Result<()> {
        let (head, body, tail) = contract_parts(std::str::from_utf8(&self.contract)?)?;
        let mut fields: serde_json::Value = serde_json::from_str(body)?;
        let map = fields.as_object_mut().context("expected stage fields")?;
        map.entry("references")
            .or_insert_with(|| serde_json::json!([]))
            .as_array_mut()
            .context("expected stage references")?
            .push(serde_json::json!({"path": path}));
        let contract = format!("{head}{}\n{tail}", serde_json::to_string_pretty(&fields)?);
        self.fields = parse_fields(contract.as_bytes())?;
        self.contract = contract.into_bytes();
        Ok(())
    }

    pub fn contract_path(&self) -> String {
        format!("stages/{}/stage.md", self.id)
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Fields {
    pub inputs: Vec<String>,
    #[serde(default)]
    pub input_fields: BTreeMap<String, Vec<String>>,
    pub outputs: Vec<String>,
    #[serde(default)]
    pub references: Vec<Reference>,
    #[serde(default)]
    pub checks: Vec<Check>,
    #[serde(deserialize_with = "Deserialize::deserialize")]
    pub review: Option<String>,
}

pub struct Plan {
    pub brief: Vec<u8>,
    pub stages: Vec<Stage>,
    pub producers: BTreeMap<String, String>,
}

impl Workspace {
    pub fn open() -> Result<Self> {
        let root = std::env::current_dir()?.canonicalize()?;
        let version = fs::read_to_string(file(&root, "anydock.version")?)
            .context("open AnyDock at its workspace root (anydock.version is required)")?;
        ensure!(version.trim() == "1", "unsupported workspace version");
        Ok(Self { root })
    }

    pub fn run(&self, id: &str) -> Result<PathBuf> {
        named_dir(&self.root, "runs", id)
    }

    pub fn workflow(&self, id: &str) -> Result<PathBuf> {
        named_dir(&self.root, "workflows", id)
    }
}

pub fn segment(name: &str) -> Result<()> {
    let base = name.split('.').next().unwrap_or_default();
    let reserved = matches!(base, "con" | "prn" | "aux" | "nul")
        || (base.len() == 4
            && (base.starts_with("com") || base.starts_with("lpt"))
            && base.as_bytes()[3].is_ascii_digit());
    ensure!(
        !name.is_empty()
            && name.len() <= 128
            && !name.ends_with('.')
            && !name.starts_with('.')
            && !reserved
            && name
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"-_.".contains(&b)),
        "invalid portable path component {name:?}"
    );
    Ok(())
}

pub fn slug(name: &str) -> Result<()> {
    segment(name)?;
    ensure!(
        !name.contains('.') && name.as_bytes()[0].is_ascii_alphanumeric(),
        "invalid name {name:?}"
    );
    Ok(())
}

fn stage_number(id: &str) -> Result<&str> {
    let (number, label) = id.split_once('-').context("expected NN-name")?;
    ensure!(
        number.len() == 2 && number != "00" && number.bytes().all(|b| b.is_ascii_digit()),
        "expected stage number 01 through 99"
    );
    slug(label)?;
    Ok(number)
}

fn named_dir(root: &Path, collection: &str, id: &str) -> Result<PathBuf> {
    slug(id)?;
    directory(root, &format!("{collection}/{id}"))
}

pub fn names(root: &Path, collection: &str) -> Result<Vec<String>> {
    let mut result = Vec::new();
    let path = directory(root, collection)?;
    if let Some(entries) = optional(fs::read_dir(path))? {
        for entry in entries {
            let name = entry?
                .file_name()
                .into_string()
                .unwrap_or_else(|name| name.to_string_lossy().into_owned());
            result.push(name);
        }
    }
    result.sort_unstable();
    Ok(result)
}

pub fn optional<T>(result: io::Result<T>) -> io::Result<Option<T>> {
    match result {
        Ok(value) => Ok(Some(value)),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(err),
    }
}

fn checked(root: &Path, relative: &str, is_dir: bool) -> Result<PathBuf> {
    let mut path = root.to_owned();
    let mut existing = root.to_owned();
    let components = relative.split('/');
    let mut parts = components.peekable();
    while let Some(part) = parts.next() {
        // This function also resolves fixed internal dot directories.
        if part != ".anydock" {
            segment(part)?;
        }
        path.push(part);
        let Some(metadata) = optional(fs::symlink_metadata(&path))
            .with_context(|| format!("inspect {}", path.display()))?
        else {
            continue;
        };
        ensure!(
            !metadata.file_type().is_symlink(),
            "path redirects or uses a different spelling: {}",
            path.display()
        );
        #[cfg(windows)]
        {
            use std::os::windows::fs::MetadataExt;
            const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0400;
            if metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
                check_spelling(&path)?;
            }
        }
        existing.clone_from(&path);
        let needs_dir = is_dir || parts.peek().is_some();
        ensure!(
            if needs_dir {
                metadata.is_dir()
            } else {
                metadata.is_file()
            },
            "expected {} at {}",
            if needs_dir {
                "a directory"
            } else {
                "a regular file"
            },
            path.display()
        );
    }
    // Canonicalizing the deepest ordinary prefix checks the spelling of all
    // of its ancestors, including when the destination doesn't exist yet.
    if existing != root {
        check_spelling(&existing)?;
    }
    Ok(path)
}

fn check_spelling(path: &Path) -> Result<()> {
    ensure!(
        path.canonicalize()? == path,
        "path redirects or uses a different spelling: {}",
        path.display()
    );
    Ok(())
}

pub fn directory(root: &Path, relative: &str) -> Result<PathBuf> {
    checked(root, relative, true)
}

pub fn file(root: &Path, relative: &str) -> Result<PathBuf> {
    checked(root, relative, false)
}

pub fn render_contract(title: &str, work: &str, fields: &serde_json::Value) -> Result<String> {
    ensure!(
        !work.trim().is_empty(),
        "stage {title} needs work instructions"
    );
    let tick = "\x60\x60\x60";
    Ok(format!(
        "# {title}\n\n{tick}anydock\n{}\n{tick}\n\n## Work\n\n{}\n",
        serde_json::to_string_pretty(fields)?,
        work.trim()
    ))
}

fn contract_parts(markdown: &str) -> Result<(&str, &str, &str)> {
    const OPEN: &str = "\x60\x60\x60anydock";
    const CLOSE: &str = "\x60\x60\x60";
    let mut offset = 0;
    let mut body_start = None;
    for line in markdown.split_inclusive('\n') {
        let text = line.trim_end();
        if let Some(start) = body_start {
            if text == CLOSE {
                return Ok((
                    &markdown[..start],
                    &markdown[start..offset],
                    &markdown[offset..],
                ));
            }
        } else if text == OPEN || (offset == 0 && text.trim_start_matches('\u{feff}') == OPEN) {
            body_start = Some(offset + line.len());
        }
        offset += line.len();
    }
    if body_start.is_some() {
        bail!("unclosed anydock fence");
    }
    bail!("missing anydock fence")
}

fn parse_fields(contract: &[u8]) -> Result<Fields> {
    let (_, body, tail) = contract_parts(std::str::from_utf8(contract)?)?;
    let prose: Vec<_> = tail.lines().skip(1).map(str::trim_end).collect();
    ensure!(
        !prose.contains(&"\x60\x60\x60anydock"),
        "multiple anydock fences"
    );
    ensure!(
        prose
            .iter()
            .skip_while(|line| **line != "## Work")
            .skip(1)
            .any(|line| !line.trim().is_empty()),
        "add a ## Work section after the fence"
    );
    let fields: Fields = serde_json::from_str(body)?;
    if let Some(decision) = &fields.review {
        ensure!(!decision.trim().is_empty(), "review decision is empty");
    }
    Ok(fields)
}

pub fn load_stages(folder: &Path) -> Result<Vec<Stage>> {
    let dir = directory(folder, "stages")?;
    let mut stages = Vec::new();
    let mut numbers = BTreeSet::new();
    for entry in fs::read_dir(&dir).with_context(|| format!("open {}", dir.display()))? {
        let id = entry?
            .file_name()
            .into_string()
            .map_err(|_| anyhow::anyhow!("non-UTF-8 stage name"))?;
        segment(&id)?;
        let number = stage_number(&id).with_context(|| format!("invalid stage {id}"))?;
        ensure!(
            numbers.insert(number.to_owned()),
            "duplicate stage number {number}"
        );
        let name = format!("stages/{id}/stage.md");
        let contract = fs::read(file(folder, &name)?)
            .with_context(|| format!("read {name} in {}", folder.display()))?;
        let fields = parse_fields(&contract).with_context(|| format!("parse {name}"))?;
        stages.push(Stage {
            id,
            contract,
            fields,
        });
    }
    stages.sort_unstable_by(|a, b| a.id.cmp(&b.id));
    ensure!(!stages.is_empty(), "{} has no stages", folder.display());
    Ok(stages)
}

pub fn read_brief(run: &Path) -> Result<Vec<u8>> {
    let path = file(run, BRIEF)?;
    let brief = fs::read(&path).with_context(|| format!("read {}", path.display()))?;
    ensure!(
        !std::str::from_utf8(&brief)?
            .trim_start_matches('\u{feff}')
            .trim()
            .is_empty(),
        "{} is empty",
        path.display()
    );
    Ok(brief)
}

pub fn external_inputs(stages: &[Stage]) -> BTreeSet<&str> {
    stages
        .iter()
        .flat_map(|stage| &stage.fields.inputs)
        .map(String::as_str)
        .filter(|path| path.starts_with("inputs/"))
        .collect()
}

impl Plan {
    pub fn external_inputs(&self) -> BTreeSet<&str> {
        external_inputs(&self.stages)
    }

    pub fn load(run: &Path) -> Result<Self> {
        let brief = read_brief(run)?;
        let stages = load_stages(run)?;
        let producers = validate_stages(run, &stages)?;
        Ok(Self {
            brief,
            stages,
            producers,
        })
    }
}

pub fn validate_stages(root: &Path, stages: &[Stage]) -> Result<BTreeMap<String, String>> {
    let mut producers = BTreeMap::new();
    let mut paths = BTreeSet::new();
    for stage in stages {
        let prefix = format!("stages/{}/outputs/", stage.id);
        ensure!(
            !stage.fields.outputs.is_empty(),
            "{} must declare outputs",
            stage.id
        );
        quality::validate(stage)?;
        let mut inputs = BTreeSet::new();
        for input in &stage.fields.inputs {
            validate_ref(input)?;
            ensure!(
                input.starts_with("inputs/") || producers.contains_key(input),
                "{} input {input} must be in inputs/ or be declared by an earlier stage",
                stage.id
            );
            ensure!(inputs.insert(input), "{} repeats input {input}", stage.id);
            file(root, input)?;
            paths.insert(input.as_str());
        }
        for (name, pointers) in &stage.fields.input_fields {
            ensure!(
                name.starts_with("inputs/") && inputs.contains(name),
                "{} input fields must name a declared external input: {name}",
                stage.id
            );
            input_scope::validate(name, pointers)?;
        }
        let mut references = BTreeSet::new();
        for reference in &stage.fields.references {
            reference.validate(root)?;
            ensure!(
                references.insert((&reference.path, &reference.section)),
                "{} repeats a declared reference",
                stage.id
            );
            paths.insert(reference.path.as_str());
        }
        for output in &stage.fields.outputs {
            validate_ref(output)?;
            ensure!(
                output.starts_with(&prefix),
                "{} must write under {prefix}",
                stage.id
            );
            file(root, output)?;
            ensure!(
                producers.insert(output.clone(), stage.id.clone()).is_none(),
                "duplicate output {output}"
            );
            paths.insert(output.as_str());
        }
    }
    for path in &paths {
        for (idx, _) in path.match_indices('/') {
            ensure!(
                !paths.contains(&path[..idx]),
                "conflicting file paths: {} and {path}",
                &path[..idx]
            );
        }
    }
    Ok(producers)
}

pub fn validate_ref(path: &str) -> Result<()> {
    if path.rsplit_once('/').is_none() {
        bail!("expected a run-relative file path, got {path:?}");
    }
    for part in path.split('/') {
        segment(part)?;
    }
    Ok(())
}
