use crate::{
    input_scope,
    quality::{self, Check},
    reference::Reference,
};
use anyhow::{Context, Result, bail, ensure};
use serde::Deserialize;
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::{self, Write},
    path::{Path, PathBuf},
};

pub const BRIEF: &str = "brief.md";
pub const PROGRESS: &str = "progress.md";
pub const VERSION_FILE: &str = "anydock.version";
const LOCAL_FOLDERS: [&str; 5] = ["runs", "workflows", "inbox", "bundles", "briefs"];
const SIDE_WORKSPACE: &str = ".anydock-work";
const AGENT_RULES_START: &str = "<!-- anydock:workflow:start -->";
const AGENT_RULES_END: &str = "<!-- anydock:workflow:end -->";

pub struct Workspace {
    pub root: PathBuf,
    pub start: PathBuf,
}

pub struct Stage {
    pub id: String,
    pub contract: Vec<u8>,
    pub fields: Fields,
    pub format: Option<String>,
}

impl Stage {
    pub fn read(id: String, contract: Vec<u8>) -> Result<Self> {
        Self::read_as(id, contract, None)
    }

    fn read_as(id: String, contract: Vec<u8>, format: Option<String>) -> Result<Self> {
        if let Some(format) = &format {
            let markdown = std::str::from_utf8(&contract)?;
            let first = markdown
                .trim_start_matches('\u{feff}')
                .lines()
                .find_map(|line| {
                    line.trim_end()
                        .strip_prefix("\x60\x60\x60")
                        .filter(|label| !label.is_empty())
                });
            ensure!(
                first == Some(format),
                "the previous contract format must match its first code fence"
            );
        }
        let fields = parse_fields(&contract, format.as_deref().unwrap_or("anydock"))
            .with_context(|| format!("parse stage {id}"))?;
        Ok(Self {
            id,
            contract,
            fields,
            format,
        })
    }

    pub fn attach_reference(&mut self, path: &str) -> Result<()> {
        let format = self.format.as_deref().unwrap_or("anydock");
        let (head, body, tail) = contract_parts(std::str::from_utf8(&self.contract)?, format)?;
        let mut fields: serde_json::Value = serde_json::from_str(body)?;
        let map = fields.as_object_mut().context("expected stage fields")?;
        map.entry("references")
            .or_insert_with(|| serde_json::json!([]))
            .as_array_mut()
            .context("expected stage references")?
            .push(serde_json::json!({"path": path}));
        let contract = format!("{head}{}\n{tail}", serde_json::to_string_pretty(&fields)?);
        self.fields = parse_fields(contract.as_bytes(), format)?;
        self.contract = contract.into_bytes();
        Ok(())
    }

    pub fn contract_path(&self) -> String {
        format!("stages/{}/stage.md", self.id)
    }

    pub fn format_path(&self) -> String {
        format!("stages/{}/contract-format", self.id)
    }

    pub fn format_bytes(&self) -> Option<Vec<u8>> {
        self.format
            .as_ref()
            .map(|format| format!("{format}\n").into_bytes())
    }

    pub fn definition_hashes(&self) -> impl Iterator<Item = (String, String)> {
        std::iter::once((self.contract_path(), crate::storage::hash(&self.contract))).chain(
            self.format_bytes()
                .map(|bytes| (self.format_path(), crate::storage::hash(&bytes))),
        )
    }
}

pub fn previous_layout(root: &Path) -> Result<Option<String>> {
    optional(fs::read_to_string(file(root, ".anydock/previous-layout")?))?
        .map(|saved| {
            let name = saved.trim();
            slug(name)?;
            Ok(name.to_owned())
        })
        .transpose()
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Fields {
    pub inputs: Vec<String>,
    #[serde(default)]
    pub input_fields: BTreeMap<String, Vec<String>>,
    #[serde(default)]
    pub input_value_format: Option<input_scope::Format>,
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
    pub fn open(start: &Path) -> Result<Self> {
        let start = start.canonicalize()?;
        ensure!(start.is_dir(), "expected a workspace directory");
        for root in start.ancestors() {
            if has_marker(root)? {
                return Ok(Self {
                    root: root.into(),
                    start,
                });
            }
        }
        bail!("no AnyDock workspace found. Open a checkout or use anydock init")
    }

    pub fn init(start: &Path) -> Result<()> {
        let root = start.canonicalize()?;
        ensure!(root.is_dir(), "expected a workspace directory");
        if has_marker(&root)? {
            println!("AnyDock is already initialized here.");
            return Ok(());
        }
        let scratch = std::env::temp_dir().canonicalize()?;
        let locks = directory(&scratch, "anydock-init-locks")?;
        fs::create_dir_all(&locks)?;
        let key = crate::storage::hash(root.as_os_str().as_encoded_bytes());
        let _lock = crate::storage::lock_file(&file(&locks, &key)?, "workspace initialization")?;
        if has_marker(&root)? {
            println!("AnyDock is already initialized here.");
            return Ok(());
        }
        for name in LOCAL_FOLDERS {
            ensure!(
                optional(fs::symlink_metadata(root.join(name)))?.is_none(),
                "{name}/ is reserved for AnyDock. Choose an empty subfolder and run anydock -C <folder> init there"
            );
        }
        let instructions = file(&root, "AGENTS.md")?;
        let ignore = file(&root, ".gitignore")?;
        let meta = directory(&root, ".anydock")?;
        let lock = file(&root, ".anydock/init.lock")?;
        if let Some(mut entries) = optional(fs::read_dir(&meta))? {
            ensure!(
                entries.all(|entry| entry.is_ok_and(|entry| entry.path() == lock)),
                "existing .anydock data requires its workspace marker or a verified upgrade"
            );
        }
        let before = optional(fs::read(&ignore))?;
        let after = ignore_contents(before.as_deref());
        let mut installed_ignore = false;
        let mut installed_instructions = false;
        let instructions_bytes = include_bytes!("../AGENTS.md");
        let initialized = (|| {
            installed_ignore = replace_setup_file(&ignore, before.as_deref(), &after)?;
            if !instructions.try_exists()? {
                write_new(&instructions, instructions_bytes)?;
                installed_instructions = true;
            }
            crate::storage::fault("before-init-marker");
            write_new(&root.join(VERSION_FILE), b"1\n")
        })();
        if let Err(error) = initialized {
            let rollback = (|| {
                if installed_instructions && fs::read(&instructions)? == instructions_bytes {
                    fs::remove_file(&instructions)?;
                }
                if installed_ignore && fs::read(&ignore)? == after {
                    if let Some(before) = before.as_deref() {
                        replace_setup_file(&ignore, Some(&after), before)?;
                    } else {
                        fs::remove_file(&ignore)?;
                    }
                }
                Ok::<(), anyhow::Error>(())
            })();
            return Err(error).with_context(|| match rollback {
                Ok(()) => "workspace initialization failed".to_owned(),
                Err(restore) => {
                    format!("workspace initialization failed; restore setup files: {restore:#}")
                }
            });
        }
        crate::storage::sync_dir(&root)?;
        println!("AnyDock initialized. The workspace is idle.");
        Ok(())
    }

    pub fn setup(start: &Path) -> Result<()> {
        let root = start.canonicalize()?;
        ensure!(root.is_dir(), "expected a project directory");
        let instructions = file(&root, "AGENTS.md")?;
        let previous = optional(fs::read(&instructions))?;
        // A malformed instructions block should fail before initializing anything.
        agent_rules_contents(previous.as_deref())?;

        let target = setup_workspace(&root)?;
        Self::init(&target)?;
        if target != root {
            let ignore = file(&root, ".gitignore")?;
            let previous = optional(fs::read(&ignore))?;
            let pattern = format!("/{SIDE_WORKSPACE}/");
            let updated = append_ignore_pattern(previous.as_deref(), &pattern);
            replace_setup_file(&ignore, previous.as_deref(), &updated)?;
        }

        let meta = directory(&target, ".anydock")?;
        fs::create_dir_all(&meta)?;
        let executable = file(&meta, "executable")?;
        let previous = optional(fs::read(&executable))?;
        let location = format!("{}\n", display_native(&std::env::current_exe()?));
        replace_setup_file(&executable, previous.as_deref(), location.as_bytes())?;

        let previous = optional(fs::read(&instructions))?;
        let updated = agent_rules_contents(previous.as_deref())?;
        replace_setup_file(&instructions, previous.as_deref(), &updated)?;
        crate::storage::sync_dir(&root)?;
        println!(
            "AnyDock project defaults ready. Workspace: {}",
            display_native(&target)
        );
        Ok(())
    }

    pub fn run(&self, id: &str) -> Result<PathBuf> {
        named_dir(&self.root, "runs", id)
    }

    pub fn workflow(&self, id: &str) -> Result<PathBuf> {
        named_dir(&self.root, "workflows", id)
    }
}

pub fn invocation(root: &Path) -> String {
    let current = std::env::current_dir()
        .and_then(|path| path.canonicalize())
        .ok();
    let relative = current
        .as_deref()
        .and_then(|current| current.strip_prefix(root).ok());
    let executable = if cfg!(windows) {
        "anydock.exe"
    } else {
        "anydock"
    };
    let packaged = root.join("bin").join(executable);
    let mut command = if packaged.is_file() {
        if let Some(relative) = relative {
            let separator = std::path::MAIN_SEPARATOR;
            let base = if relative.as_os_str().is_empty() {
                ".".into()
            } else {
                vec![".."; relative.components().count()].join(&separator.to_string())
            };
            format!("{base}{separator}bin{separator}{executable}")
        } else {
            let absolute = shell_word(&display_native(&packaged));
            if cfg!(windows) {
                format!("& {absolute}")
            } else {
                absolute
            }
        }
    } else {
        "anydock".into()
    };
    if relative.is_none() {
        command.push_str(" -C ");
        command.push_str(&shell_word(&display_native(root)));
    }
    command
}

pub fn run_invocation(run: &Path) -> String {
    run.parent()
        .and_then(Path::parent)
        .map_or_else(|| "anydock".into(), invocation)
}

pub fn shell_word(text: &str) -> String {
    if text
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || "-_.:/\\".contains(ch))
    {
        text.into()
    } else if cfg!(windows) {
        format!("'{}'", text.replace('\'', "''"))
    } else {
        format!("'{}'", text.replace('\'', "'\\''"))
    }
}

pub(crate) fn display_native(path: &Path) -> String {
    let display = path.to_string_lossy();
    if let Some(tail) = display.strip_prefix(r"\\?\UNC\") {
        format!(r"\\{tail}")
    } else {
        display.strip_prefix(r"\\?\").unwrap_or(&display).to_owned()
    }
}

pub fn append_ignore(path: &Path) -> Result<()> {
    let previous = optional(fs::read(path))?;
    replace_setup_file(
        path,
        previous.as_deref(),
        &ignore_contents(previous.as_deref()),
    )?;
    Ok(())
}

fn ignore_contents(previous: Option<&[u8]>) -> Vec<u8> {
    let mut updated = previous.unwrap_or_default().to_vec();
    let block: String = LOCAL_FOLDERS
        .into_iter()
        .chain([".anydock"])
        .map(|folder| format!("/{folder}/\n"))
        .collect();
    if !updated.ends_with(block.as_bytes()) {
        if !updated.is_empty() && !updated.ends_with(b"\n") {
            updated.push(b'\n');
        }
        updated.extend_from_slice(block.as_bytes());
    }
    updated
}

fn setup_workspace(root: &Path) -> Result<PathBuf> {
    if has_marker(root)? {
        return Ok(root.to_owned());
    }
    let side = directory(root, SIDE_WORKSPACE)?;
    let mut collision = false;
    for name in LOCAL_FOLDERS {
        collision |= optional(fs::symlink_metadata(root.join(name)))?.is_some();
    }
    if has_marker(&side)? {
        return Ok(side);
    }
    if !collision {
        return Ok(root.to_owned());
    }
    if let Some(mut entries) = optional(fs::read_dir(&side))? {
        ensure!(
            entries.next().is_none(),
            "{} already contains data. Set up AnyDock in a separate folder.",
            display_native(&side)
        );
    } else {
        fs::create_dir_all(&side)?;
    }
    Ok(side)
}

fn agent_rules_contents(previous: Option<&[u8]>) -> Result<Vec<u8>> {
    let bundled = include_str!("../AGENTS.md");
    let (_, after) = bundled
        .split_once(AGENT_RULES_START)
        .context("missing bundled workflow")?;
    let (body, _) = after
        .split_once(AGENT_RULES_END)
        .context("missing bundled workflow end")?;
    let block = format!("{AGENT_RULES_START}{body}{AGENT_RULES_END}\n").replace("\r\n", "\n");
    let mut updated = previous.unwrap_or_default().to_vec();
    let existing =
        std::str::from_utf8(&updated).context("AGENTS.md must be UTF-8 to add the workflow")?;
    let start = existing
        .match_indices(AGENT_RULES_START)
        .collect::<Vec<_>>();
    let end = existing.match_indices(AGENT_RULES_END).collect::<Vec<_>>();
    match (start.as_slice(), end.as_slice()) {
        ([(start, _)], [(end, _)]) if start < end => return Ok(updated),
        ([], []) => {}
        _ => bail!("AGENTS.md contains an incomplete or duplicate AnyDock workflow block"),
    }
    let newline = if existing.contains("\r\n") {
        "\r\n"
    } else {
        "\n"
    };
    if !updated.is_empty() {
        if !updated.ends_with(b"\n") {
            updated.extend_from_slice(newline.as_bytes());
        }
        updated.extend_from_slice(newline.as_bytes());
    }
    updated.extend_from_slice(block.replace('\n', newline).as_bytes());
    Ok(updated)
}

fn append_ignore_pattern(previous: Option<&[u8]>, pattern: &str) -> Vec<u8> {
    let mut updated = previous.unwrap_or_default().to_vec();
    let before_newline = updated.strip_suffix(b"\n").unwrap_or(&updated);
    let last = before_newline
        .rsplit(|byte| *byte == b'\n')
        .next()
        .unwrap_or_default();
    if last.strip_suffix(b"\r").unwrap_or(last) == pattern.as_bytes() {
        return updated;
    }
    if !updated.is_empty() && !updated.ends_with(b"\n") {
        updated.push(b'\n');
    }
    updated.extend_from_slice(pattern.as_bytes());
    updated.push(b'\n');
    updated
}

pub(crate) fn replace_setup_file(
    path: &Path,
    previous: Option<&[u8]>,
    updated: &[u8],
) -> Result<bool> {
    if let Some(previous) = previous {
        if updated == previous {
            return Ok(false);
        }
        let mut pending =
            tempfile::NamedTempFile::new_in(path.parent().context("missing parent")?)?;
        pending.write_all(updated)?;
        pending.as_file().sync_all()?;
        ensure!(
            fs::read(path)? == previous,
            "{} changed during setup. Retry initialization",
            path.display()
        );
        crate::storage::persist_replace(pending, path)?;
    } else {
        if let Err(error) = write_new(path, updated) {
            if optional(fs::read(path))?.as_deref() == Some(updated) {
                return Ok(false);
            }
            return Err(error);
        }
    }
    Ok(true)
}

pub fn write_new(path: &Path, bytes: &[u8]) -> Result<()> {
    let mut pending = tempfile::NamedTempFile::new_in(path.parent().context("missing parent")?)?;
    pending.write_all(bytes)?;
    pending.as_file().sync_all()?;
    pending.persist_noclobber(path)?;
    Ok(())
}

pub fn has_marker(root: &Path) -> Result<bool> {
    let Some(version) = optional(fs::read_to_string(file(root, VERSION_FILE)?))? else {
        return Ok(false);
    };
    ensure!(version.trim() == "1", "unsupported workspace version");
    Ok(true)
}

pub fn segment(name: &str) -> Result<()> {
    let base = name.split('.').next().unwrap_or_default();
    ensure!(
        !name.is_empty()
            && name.len() <= 128
            && !name.ends_with('.')
            && !name.starts_with('.')
            && !reserved_device(base)
            && name
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"-_.".contains(&b)),
        "invalid portable path component {name:?}"
    );
    Ok(())
}

fn reserved_device(base: &str) -> bool {
    let base = base.trim_end_matches(' ').to_ascii_lowercase();
    matches!(
        base.as_str(),
        "con" | "prn" | "aux" | "nul" | "conin$" | "conout$"
    ) || base
        .strip_prefix("com")
        .or_else(|| base.strip_prefix("lpt"))
        .is_some_and(|suffix| {
            matches!(
                suffix,
                "0" | "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9" | "¹" | "²" | "³"
            )
        })
}

pub fn input_component(name: &str) -> Result<()> {
    ensure!(
        !name.is_empty()
            && name.encode_utf16().count() <= 255
            && !name.ends_with(['.', ' '])
            && !name.starts_with('.')
            && !name
                .chars()
                .any(|value| value.is_control() || "<>:\"/\\|?*".contains(value))
            && !reserved_device(name.split('.').next().unwrap_or_default()),
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
            let entry = entry?;
            let name = entry
                .file_name()
                .into_string()
                .unwrap_or_else(|name| name.to_string_lossy().into_owned());
            if os_metadata(&name) && entry.file_type()?.is_file() {
                continue;
            }
            result.push(name);
        }
    }
    result.sort_unstable();
    Ok(result)
}

pub fn os_metadata(name: &str) -> bool {
    name.eq_ignore_ascii_case(".DS_Store")
        || name.eq_ignore_ascii_case("desktop.ini")
        || name.eq_ignore_ascii_case("Thumbs.db")
        || name.starts_with("._")
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
        if !matches!(part, ".anydock" | ".gitignore" | "AGENTS.md") && relative != SIDE_WORKSPACE {
            input_component(part)?;
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

fn contract_parts<'a>(markdown: &'a str, format: &str) -> Result<(&'a str, &'a str, &'a str)> {
    let open = format!("\x60\x60\x60{format}");
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
        } else if text == open || (offset == 0 && text.trim_start_matches('\u{feff}') == open) {
            body_start = Some(offset + line.len());
        }
        offset += line.len();
    }
    if body_start.is_some() {
        bail!("unclosed {format} fence");
    }
    bail!("missing {format} fence")
}

fn parse_fields(contract: &[u8], format: &str) -> Result<Fields> {
    let (_, body, tail) = contract_parts(std::str::from_utf8(contract)?, format)?;
    let prose: Vec<_> = tail.lines().skip(1).map(str::trim_end).collect();
    ensure!(
        !prose.contains(&format!("\x60\x60\x60{format}").as_str()),
        "multiple {format} fences"
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
        let entry = entry?;
        let id = entry
            .file_name()
            .into_string()
            .map_err(|_| anyhow::anyhow!("non-UTF-8 stage name"))?;
        if os_metadata(&id) && entry.file_type()?.is_file() {
            continue;
        }
        segment(&id)?;
        let number = stage_number(&id).with_context(|| format!("invalid stage {id}"))?;
        ensure!(
            numbers.insert(number.to_owned()),
            "duplicate stage number {number}"
        );
        let name = format!("stages/{id}/stage.md");
        let contract = fs::read(file(folder, &name)?)
            .with_context(|| format!("read {name} in {}", folder.display()))?;
        let format = optional(fs::read_to_string(file(
            folder,
            &format!("stages/{id}/contract-format"),
        )?))?;
        let format = format
            .map(|format| {
                let name = format.trim_end_matches('\n');
                slug(name)?;
                ensure!(
                    format == format!("{name}\n"),
                    "invalid contract format receipt"
                );
                Ok::<_, anyhow::Error>(name.to_owned())
            })
            .transpose()?;
        stages.push(Stage::read_as(id, contract, format).with_context(|| format!("parse {name}"))?);
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
    let mut spellings = BTreeMap::new();
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
        for prefix in path
            .match_indices('/')
            .map(|(idx, _)| &path[..idx])
            .chain([*path])
        {
            if let Some(other) = spellings.insert(prefix.to_lowercase(), prefix) {
                ensure!(
                    other == prefix,
                    "conflicting path spellings: {other} and {prefix}"
                );
            }
            ensure!(
                prefix == *path || !paths.contains(prefix),
                "conflicting file paths: {prefix} and {path}"
            );
        }
    }
    Ok(producers)
}

pub fn validate_ref(path: &str) -> Result<()> {
    let (scope, components) = path
        .split_once('/')
        .with_context(|| format!("expected a run-relative file path, got {path:?}"))?;
    segment(scope)?;
    for part in components.split('/') {
        if matches!(scope, "inputs" | "inbox") {
            input_component(part)?;
        } else {
            segment(part)?;
        }
    }
    Ok(())
}
