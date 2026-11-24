use crate::{
    engine::{self, Phase},
    input_scope, policy, recurring, review,
    storage::{LockedRun, copy_exact, fault, fingerprint, hash, lock_file, sync_dir},
    workspace::{PROGRESS, Plan, Workspace, file, names, optional, os_metadata, slug},
};
use anyhow::{Context, Result, ensure};
use chrono::{DateTime, Local, TimeDelta, Utc};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    fmt::Write as _,
    fs::{self, File},
    io::{Read, Write as _},
    path::{Path, PathBuf},
};
use tempfile::NamedTempFile;

const CONFIG: &str = ".anydock/daily.json";
const REPORT: &str = "briefs/daily.md";
const HEADER: &str = "<!-- anydock-daily-v1 ";
const COMMENT_END: &str = " -->";
const RECENT_SECONDS: i64 = 24 * 60 * 60;

#[derive(Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct Config {
    feeds: Vec<Feed>,
}

#[derive(Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct Feed {
    name: String,
    folder: PathBuf,
    stale_after_hours: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    files: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    failure_file: Option<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    fields: BTreeMap<String, Vec<String>>,
}

impl Config {
    fn from_feeds(feeds: &[String]) -> Result<Self> {
        let feeds = feeds
            .iter()
            .map(|argument| {
                let (name, folder) = argument
                    .split_once('=')
                    .with_context(|| format!("expected --feed NAME=FOLDER, got {argument:?}"))?;
                Ok(Feed {
                    name: name.into(),
                    folder: folder.into(),
                    stale_after_hours: 24,
                    files: None,
                    failure_file: None,
                    fields: BTreeMap::new(),
                })
            })
            .collect::<Result<_>>()?;
        let config = Self { feeds };
        config.validate()?;
        Ok(config)
    }

    fn validate(&self) -> Result<()> {
        let mut feeds = BTreeSet::new();
        for feed in &self.feeds {
            slug(&feed.name)?;
            ensure!(feeds.insert(&feed.name), "duplicate feed {}", feed.name);
            ensure!(
                !feed.folder.as_os_str().is_empty(),
                "missing folder for {}",
                feed.name
            );
            ensure!(
                feed.stale_after_hours > 0,
                "freshness must be positive for {}",
                feed.name
            );
            let mut files = BTreeSet::new();
            for path in feed.files.iter().flatten().chain(feed.failure_file.iter()) {
                validate_path(path)?;
                ensure!(files.insert(path), "duplicate feed path {path}");
            }
            for (path, pointers) in &feed.fields {
                validate_path(path)?;
                ensure!(
                    feed.failure_file.as_ref() != Some(path)
                        && feed
                            .files
                            .as_ref()
                            .map_or_else(|| !path.contains('/'), |files| files.contains(path)),
                    "selected feed file {path} is outside the feed input list"
                );
                input_scope::validate(path, pointers)?;
            }
            ensure!(
                feed.files.as_ref().is_none_or(|files| !files.is_empty()),
                "empty file list for {}",
                feed.name
            );
        }
        Ok(())
    }
}

#[derive(Default, Deserialize, Serialize)]
struct Receipt {
    day: String,
    feeds: BTreeMap<String, BTreeMap<String, Source>>,
}

#[derive(Clone, Deserialize, Serialize)]
struct Source {
    digest: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    previous_digest: Option<String>,
    modified: i64,
    new: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    observations: Vec<Observation>,
}
#[derive(Clone, Deserialize, Serialize)]
struct Observation {
    at: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    prior: Option<String>,
}

impl Source {
    fn retain_recent(&mut self, time: i64, same_day: bool) {
        if self.observations.is_empty() && self.new && same_day {
            self.observations.push(Observation {
                at: time,
                prior: self.previous_digest.take(),
            });
        }
        self.observations
            .retain(|entry| time.saturating_sub(entry.at) < RECENT_SECONDS);
        self.update_labels();
    }

    fn observe(&mut self, prior: Option<Self>, time: i64) {
        if let Some(prior) = prior {
            self.observations = prior.observations;
            if prior.digest != self.digest {
                self.observations.push(Observation {
                    at: time,
                    prior: Some(prior.digest),
                });
            }
        } else {
            self.observations.push(Observation {
                at: time,
                prior: None,
            });
        }
        self.update_labels();
    }

    fn update_labels(&mut self) {
        self.new = !self.observations.is_empty();
        self.previous_digest = self
            .observations
            .iter()
            .find_map(|entry| entry.prior.clone());
    }
}

#[derive(Default)]
struct Sections {
    decisions: String,
    intake: String,
    progress: String,
    completed: String,
    blockers: String,
}

pub fn write(space: &Workspace, feeds: &[String], add: &[String], remove: &[String]) -> Result<()> {
    let request = (!feeds.is_empty())
        .then(|| Config::from_feeds(feeds))
        .transpose()?;
    let config_path = file(&space.root, CONFIG)?;
    let parent = config_path
        .parent()
        .context("missing daily configuration directory")?;
    let needs_setup =
        || format!("configure {CONFIG} or use daily --feed NAME=FOLDER to enable the daily brief");
    if request.is_some() {
        fs::create_dir_all(parent)?;
    } else {
        ensure!(config_path.try_exists()?, "{}", needs_setup());
    }
    let _lock = lock_file(&file(&space.root, ".anydock/daily.lock")?, "daily brief")?;
    let prior_config = optional(fs::read(&config_path))?;
    let saved: Option<Config> = prior_config
        .as_deref()
        .map(|bytes| serde_json::from_slice(bytes).with_context(|| format!("read {CONFIG}")))
        .transpose()?;
    let initial = saved.is_none();
    let mut config = if let Some(saved) = saved {
        ensure!(
            request.as_ref().is_none_or(|request| *request == saved),
            "daily feed configuration already exists at {CONFIG}. Edit that file to change the feeds"
        );
        saved
    } else {
        request.with_context(needs_setup)?
    };
    let mut publish = initial;
    for name in remove {
        slug(name)?;
        let before = config.feeds.len();
        config.feeds.retain(|feed| feed.name != *name);
        publish |= before != config.feeds.len();
    }
    for entry in Config::from_feeds(add)?.feeds {
        if let Some(existing) = config.feeds.iter().find(|feed| feed.name == entry.name) {
            ensure!(
                *existing == entry,
                "feed {} already exists with different settings",
                entry.name
            );
        } else {
            config.feeds.push(entry);
            publish = true;
        }
    }
    config.validate()?;
    let report = file(&space.root, REPORT)?;
    let prior = optional(fs::read_to_string(&report))?;
    let previous_layout = crate::workspace::previous_layout(&space.root)?;
    let mut old = prior
        .as_deref()
        .map(|text| read_receipt(text, previous_layout.as_deref()))
        .transpose()?
        .unwrap_or_default();
    if publish {
        let mut staged = NamedTempFile::new_in(parent)?;
        serde_json::to_writer_pretty(&mut staged, &config)?;
        writeln!(staged)?;
        staged.as_file().sync_all()?;
        if initial {
            staged.persist_noclobber(&config_path)?;
        } else {
            ensure!(
                optional(fs::read(&config_path))? == prior_config,
                "{CONFIG} changed during setup; retry with the new settings"
            );
            crate::storage::persist_replace(staged, &config_path)?;
        }
        sync_dir(parent)?;
        fault("after-daily-config");
    }
    let now = Local::now();
    let mut receipt = Receipt {
        day: now.date_naive().to_string(),
        feeds: BTreeMap::new(),
    };
    let mut sections = Sections::default();
    for feed in &config.feeds {
        let mut previous = old.feeds.remove(&feed.name).unwrap_or_default();
        for record in previous.values_mut() {
            record.retain_recent(now.timestamp(), old.day == receipt.day);
        }
        let records = scan_feed(space, feed, previous, now.timestamp(), &mut sections)?;
        receipt.feeds.insert(feed.name.clone(), records);
    }
    inspect_runs(space, now - TimeDelta::hours(24), &mut sections)?;
    let mut body = format!(
        "<!-- {}{COMMENT_END}\n\n# Daily brief · {}\n\n",
        serde_json::to_string(&receipt)?,
        now.format("%Y-%m-%d"),
    );
    writeln!(body, "Generated {}.", now.format("%Y-%m-%d %H:%M %:z"))?;
    section(
        &mut body,
        "Decisions",
        &sections.decisions,
        "No pending managed reviews.",
    )?;
    section(
        &mut body,
        "Sources",
        &sections.intake,
        "No feeds configured.",
    )?;
    section(
        &mut body,
        "Agent progress",
        &sections.progress,
        "No work in progress.",
    )?;
    section(
        &mut body,
        "Completed work",
        &sections.completed,
        "No completed work in recently updated run records.",
    )?;
    section(
        &mut body,
        "Blockers",
        &sections.blockers,
        "No blockers found.",
    )?;
    let generated = format!("{HEADER}{}{COMMENT_END}\n{body}", hash(body.as_bytes()));
    let parent = report.parent().context("missing brief directory")?;
    fs::create_dir_all(parent)?;
    let mut staged = NamedTempFile::new_in(parent)?;
    staged.write_all(generated.as_bytes())?;
    staged.as_file().sync_all()?;
    // Check again before publication so edits during the scan are preserved.
    ensure!(
        optional(fs::read_to_string(&report))? == prior,
        "{REPORT} changed during generation; keep your edits and regenerate when ready"
    );
    if prior.is_some() {
        crate::storage::persist_replace(staged, &report)?;
    } else {
        staged.persist_noclobber(&report)?;
    }
    sync_dir(parent)?;
    println!("Wrote {REPORT}");
    Ok(())
}

fn read_receipt(text: &str, previous_layout: Option<&str>) -> Result<Receipt> {
    let protected = || {
        format!(
            "{REPORT} contains edits or is user-owned; move it to keep your notes before regenerating"
        )
    };
    let (header, body) = text.split_once('\n').with_context(protected)?;
    let header = header.strip_suffix('\r').unwrap_or(header);
    let digest = header
        .strip_prefix(HEADER)
        .or_else(|| header.strip_prefix(&format!("<!-- {}-daily-v1 ", previous_layout?)))
        .and_then(|s| s.strip_suffix(COMMENT_END))
        .with_context(protected)?;
    ensure!(
        digest == hash(body.as_bytes())
            || (body.contains("\r\n") && digest == hash(body.replace("\r\n", "\n").as_bytes())),
        "{}",
        protected()
    );
    let metadata = body
        .lines()
        .next()
        .and_then(|s| s.strip_prefix("<!-- "))
        .and_then(|s| s.strip_suffix(COMMENT_END))
        .with_context(protected)?;
    serde_json::from_str(metadata).with_context(protected)
}

fn section(body: &mut String, title: &str, contents: &str, empty: &str) -> Result<()> {
    writeln!(body, "\n## {title}\n")?;
    if contents.is_empty() {
        writeln!(body, "{empty}")?;
    } else {
        body.push_str(contents);
    }
    Ok(())
}

fn validate_path(path: &str) -> Result<()> {
    ensure!(
        !path.is_empty()
            && path.split('/').all(|part| {
                !part.is_empty()
                    && part != "."
                    && part != ".."
                    && !part.contains(['\\', ':', '<', '>', '"', '|', '?', '*'])
                    && !part.chars().any(char::is_control)
                    && !part.ends_with([' ', '.'])
            }),
        "invalid source file path {path:?}"
    );
    Ok(())
}

fn source_file(root: &Path, path: &str) -> Result<Option<PathBuf>> {
    validate_path(path)?;
    let mut file = root.to_owned();
    file.extend(path.split('/'));
    let Some(resolved) =
        optional(file.canonicalize()).with_context(|| format!("open {}", file.display()))?
    else {
        return Ok(None);
    };
    ensure!(
        resolved == file && resolved.is_file(),
        "expected a contained regular file at {}",
        file.display()
    );
    Ok(Some(file))
}

fn scan_feed(
    space: &Workspace,
    feed: &Feed,
    mut records: BTreeMap<String, Source>,
    now: i64,
    sections: &mut Sections,
) -> Result<BTreeMap<String, Source>> {
    let mut issues = Vec::new();
    let mut seen = BTreeSet::new();
    let source = space.root.join(&feed.folder);
    let result = (|| -> Result<()> {
        let Some(folder) = optional(source.canonicalize())? else {
            issues.push(format!(
                "Missing source folder {}",
                inline(&source.display().to_string())
            ));
            return Ok(());
        };
        ensure!(folder.is_dir(), "expected a folder at {}", source.display());
        if let Some(marker) = &feed.failure_file
            && let Some(path) = source_file(&folder, marker)?
        {
            let status = capture(space, &feed.name, marker, &path)?;
            let destination = snapshot_path(&feed.name, marker, &status.digest);
            issues.push(format!(
                "Producer reported failure in {}",
                link(marker, &destination)
            ));
        }
        let inputs = match &feed.files {
            Some(inputs) => inputs.clone(),
            None => {
                let mut files = Vec::new();
                for entry in fs::read_dir(&folder)? {
                    let entry = entry?;
                    if entry.file_type()?.is_file() {
                        let name = entry
                            .file_name()
                            .into_string()
                            .map_err(|_| anyhow::anyhow!("source filename is not valid text"))?;
                        if feed.failure_file.as_ref() != Some(&name)
                            && entry.path() != space.root.join(REPORT)
                            && !os_metadata(&name)
                        {
                            files.push(name);
                        }
                    }
                }
                files.extend(feed.fields.keys().cloned());
                files.sort();
                files.dedup();
                files
            }
        };
        if inputs.is_empty() {
            issues.push("Missing input files".into());
        }
        for input in inputs {
            let result = source_file(&folder, &input).and_then(|path| {
                path.map(|path| capture(space, &feed.name, &input, &path))
                    .transpose()
            });
            match result {
                Ok(Some(mut source)) => {
                    source.observe(records.remove(&input), now);
                    seen.insert(input.clone());
                    records.insert(input, source);
                }
                Ok(None) => issues.push(format!("Missing source file {}", inline(&input))),
                Err(error) => issues.push(format!(
                    "Failed to read {} · {}",
                    inline(&input),
                    inline(&format!("{error:#}"))
                )),
            }
        }
        Ok(())
    })();
    if let Err(error) = result {
        issues.push(format!(
            "Failed to scan feed · {}",
            inline(&format!("{error:#}"))
        ));
    }
    let newest = records.iter().max_by_key(|(_, source)| source.modified);
    let updated = newest
        .and_then(|(_, source)| DateTime::from_timestamp(source.modified, 0))
        .map_or_else(
            || "none recorded".into(),
            |time| {
                time.with_timezone(&Local)
                    .format("%Y-%m-%d %H:%M %:z")
                    .to_string()
            },
        );
    if newest.is_some_and(|(_, source)| {
        now.saturating_sub(source.modified) >= i64::from(feed.stale_after_hours) * 3600
    }) {
        issues.push(format!(
            "Stale · no update within {} hours",
            feed.stale_after_hours
        ));
    }
    writeln!(
        sections.intake,
        "### {}\n\nLast source update · {updated}\n",
        feed.name
    )?;
    let mut listed = 0;
    for (path, record) in records.iter().filter(|(path, source)| {
        source.new || (seen.contains(*path) && feed.fields.contains_key(*path))
    }) {
        let snapshot = snapshot_path(&feed.name, path, &record.digest);
        let label = if record.new {
            "New input"
        } else {
            "Source snapshot"
        };
        writeln!(sections.intake, "- {label} · {}", link(path, &snapshot))?;
        if seen.contains(path) {
            if feed.fields.contains_key(path) {
                if let Err(error) =
                    selected_values(space, feed, path, record, &mut sections.intake, &mut issues)
                {
                    issues.push(format!(
                        "Selected values in {} unavailable · {}",
                        inline(path),
                        inline(&format!("{error:#}"))
                    ));
                }
            } else {
                preview(space, &snapshot, &mut sections.intake)?;
            }
        }
        listed += 1;
    }
    if listed == 0 {
        if let Some((path, record)) = newest {
            writeln!(
                sections.intake,
                "- Latest snapshot · {}",
                link(path, &snapshot_path(&feed.name, path, &record.digest))
            )?;
        } else {
            writeln!(sections.intake, "- No source snapshot.")?;
        }
    }
    for issue in issues {
        writeln!(sections.blockers, "- **{}** · {issue}", feed.name)?;
    }
    writeln!(sections.intake)?;
    if !seen.is_empty() && feed.files.is_none() {
        records.retain(|path, source| source.new || seen.contains(path));
    }
    Ok(records)
}

fn snapshot_bytes(space: &Workspace, path: &str, digest: &str) -> Result<Vec<u8>> {
    let bytes = fs::read(file(&space.root, path)?)?;
    ensure!(hash(&bytes) == digest, "source snapshot changed: {path}");
    Ok(bytes)
}

fn selected_values(
    space: &Workspace,
    feed: &Feed,
    path: &str,
    record: &Source,
    output: &mut String,
    issues: &mut Vec<String>,
) -> Result<()> {
    let snapshot = snapshot_path(&feed.name, path, &record.digest);
    let bytes = snapshot_bytes(space, &snapshot, &record.digest)?;
    let current = input_scope::parse(&bytes).context("invalid source JSON")?;
    let mut earlier = None;
    if let Some(digest) = &record.previous_digest
        && digest != &record.digest
    {
        ensure!(
            crate::storage::is_hash(digest),
            "invalid earlier source digest"
        );
        let before_path = snapshot_path(&feed.name, path, digest);
        match snapshot_bytes(space, &before_path, digest) {
            Ok(bytes) => {
                writeln!(output, "  {}", link("Earlier snapshot", &before_path))?;
                earlier = Some(bytes);
            }
            Err(error) => issues.push(format!(
                "Earlier snapshot for {} unavailable · {}",
                inline(path),
                inline(&format!("{error:#}"))
            )),
        }
    }
    let previous = earlier
        .as_deref()
        .and_then(|bytes| match input_scope::parse(bytes) {
            Ok(document) => Some(document),
            Err(_) => {
                issues.push(format!(
                    "Earlier snapshot for {} contains invalid JSON",
                    inline(path)
                ));
                None
            }
        });
    writeln!(output)?;
    for pointer in &feed.fields[path] {
        match input_scope::select(current, pointer) {
            Ok(value) => {
                let exact = value.get();
                let value = bounded_literal(exact);
                let label = bounded_literal(if pointer.is_empty() {
                    "(document)"
                } else {
                    pointer
                });
                let before = previous.map(|document| input_scope::select(document, pointer));
                match before {
                    Some(Ok(old)) if old.get() != exact => writeln!(
                        output,
                        "      {label}: {} → {value}",
                        bounded_literal(old.get())
                    )?,
                    Some(Err(_)) => {
                        writeln!(output, "      {label}: earlier value unavailable → {value}")?
                    }
                    _ => writeln!(output, "      {label}: {value}")?,
                }
            }
            Err(error) => issues.push(format!(
                "Selected field {} in {} unavailable · {}",
                inline(pointer),
                inline(path),
                inline(&format!("{error:#}"))
            )),
        }
    }
    Ok(())
}

fn bounded_literal(text: &str) -> String {
    let end = text
        .char_indices()
        .nth(180)
        .map_or(text.len(), |(index, _)| index);
    let mut visible = String::new();
    for c in text[..end].chars() {
        if c.is_control() {
            visible.extend(c.escape_default());
        } else {
            visible.push(c);
        }
    }
    if end < text.len() {
        visible.push('…');
    }
    visible
}

fn snapshot_path(feed: &str, name: &str, digest: &str) -> String {
    let extension = Path::new(name)
        .extension()
        .and_then(|ext| ext.to_str())
        .filter(|ext| {
            !ext.is_empty() && ext.len() <= 12 && ext.bytes().all(|b| b.is_ascii_alphanumeric())
        })
        .unwrap_or("bin")
        .to_ascii_lowercase();
    format!("briefs/sources/{feed}/{digest}.{extension}")
}

fn capture(space: &Workspace, feed: &str, relative: &str, origin: &Path) -> Result<Source> {
    let file_handle = File::open(origin)?;
    let modified = DateTime::<Utc>::from(file_handle.metadata()?.modified()?).timestamp();
    let digest = fingerprint(file_handle)?;
    let path = snapshot_path(feed, relative, &digest);
    store_snapshot(space, origin, &path, &digest)?;
    Ok(Source {
        digest,
        previous_digest: None,
        modified,
        new: false,
        observations: Vec::new(),
    })
}

fn store_snapshot(space: &Workspace, origin: &Path, path: &str, digest: &str) -> Result<()> {
    ensure!(
        origin != space.root.join(REPORT) && !origin.starts_with(space.root.join("briefs/sources")),
        "generated brief data cannot be used as an input feed"
    );
    let dest = file(&space.root, path)?;
    if dest.try_exists()? {
        ensure!(
            fingerprint(File::open(&dest)?)? == digest,
            "source snapshot changed: {path}"
        );
    } else {
        let parent = dest.parent().context("missing source snapshot directory")?;
        fs::create_dir_all(parent)?;
        let stage = tempfile::tempdir_in(parent)?;
        let copy = stage.path().join("source");
        copy_exact(origin, &copy, digest)?;
        fs::rename(&copy, &dest)?;
        sync_dir(parent)?;
    }
    Ok(())
}

fn preview(space: &Workspace, path: &str, output: &mut String) -> Result<()> {
    const MAX: usize = 600;
    let mut bytes = Vec::new();
    File::open(file(&space.root, path)?)?
        .take((MAX + 1) as u64)
        .read_to_end(&mut bytes)?;
    let text = match std::str::from_utf8(&bytes) {
        Ok(text) => text,
        Err(error) if error.error_len().is_none() => {
            std::str::from_utf8(&bytes[..error.valid_up_to()])?
        }
        Err(_) => return Ok(()),
    };
    if text.trim().is_empty()
        || text
            .chars()
            .any(|c| c.is_control() && !['\n', '\r', '\t'].contains(&c))
    {
        return Ok(());
    }
    writeln!(output)?;
    for line in text.lines().take(6) {
        writeln!(output, "      {line}")?;
    }
    if bytes.len() > MAX || text.lines().count() > 6 {
        writeln!(output, "      … Open the snapshot for the complete input.")?;
    }
    Ok(())
}

fn inspect_runs(space: &Workspace, cutoff: DateTime<Local>, sections: &mut Sections) -> Result<()> {
    let cli = crate::workspace::invocation(&space.root);
    let mut current_runs = BTreeSet::new();
    let mut superseded = BTreeSet::new();
    let mut incoming: BTreeMap<String, Vec<_>> = BTreeMap::new();
    for task in recurring::tracked_names(space)? {
        match recurring::inspect(space, &task) {
            Ok(Some(current)) => {
                match current.changed_sources(space, &task) {
                    Ok(Some(changes)) => incoming
                        .entry(current.run.clone())
                        .or_default()
                        .push((task, changes)),
                    Err(error) => writeln!(
                        sections.blockers,
                        "- **{}** · saved source unavailable · {}",
                        task,
                        inline(&format!("{error:#}"))
                    )?,
                    Ok(None) => {}
                }
                current_runs.insert(current.run);
                let mut state = current.state;
                while let Some(update) = state.update {
                    let Ok(prior) = space
                        .run(&update.from)
                        .and_then(|path| LockedRun::inspect(&path))
                    else {
                        break;
                    };
                    let Some(prior) = prior else {
                        break;
                    };
                    if prior.identity.as_ref() != Some(&update.identity)
                        || !superseded.insert(update.from)
                    {
                        break;
                    }
                    state = prior;
                }
            }
            Ok(None) => {}
            Err(error) => writeln!(
                sections.blockers,
                "- **{}** · {}",
                task,
                inline(&format!("{error:#}"))
            )?,
        }
    }
    for run in names(&space.root, "runs")? {
        let active = !superseded.contains(&run) || current_runs.contains(&run);
        let finished = match inspect_run(space, &run, active, cutoff, &cli, sections) {
            Ok(finished) => finished,
            Err(error) => {
                writeln!(
                    sections.blockers,
                    "- **{}** · {}",
                    inline(&run),
                    inline(&format!("{error:#}"))
                )?;
                false
            }
        };
        for (task, packet) in incoming.remove(&run).into_iter().flatten() {
            let brief = link(&run, &format!("runs/{run}/brief.md"));
            writeln!(
                sections.intake,
                "- **{task}** · incoming changes in {} · current run {brief}",
                inline(
                    &packet
                        .files
                        .keys()
                        .map(String::as_str)
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            )?;
            if finished {
                match packet_links(space, &task, &packet) {
                    Ok(links) => {
                        let command = code(&format!(
                            "{cli} refresh {task} --submit --source-revision {}",
                            packet.revision
                        ));
                        writeln!(sections.intake, "  - Observed files · {links}")?;
                        writeln!(
                            sections.intake,
                            "  - To submit this exact packet after authorization, run {command}."
                        )?;
                    }
                    Err(error) => writeln!(
                        sections.blockers,
                        "- **{task}** · could not retain the observed files · {}",
                        inline(&format!("{error:#}"))
                    )?,
                }
            }
        }
    }
    Ok(())
}

fn packet_links(
    space: &Workspace,
    task: &str,
    packet: &recurring::SourceChanges,
) -> Result<String> {
    let links = packet
        .files
        .iter()
        .map(|(name, digest)| {
            let source = file(&packet.source, name)?;
            let snapshot = snapshot_path(task, name, digest);
            store_snapshot(space, &source, &snapshot, digest)?;
            Ok(link(name, &snapshot))
        })
        .collect::<Result<Vec<_>>>()?;
    Ok(links.join(" · "))
}

fn inspect_run(
    space: &Workspace,
    run: &str,
    active: bool,
    cutoff: DateTime<Local>,
    cli: &str,
    sections: &mut Sections,
) -> Result<bool> {
    let root = space.run(run)?;
    let brief = link(run, &format!("runs/{run}/brief.md"));
    let Some(mut state) = LockedRun::inspect(&root)? else {
        if active && file(&root, PROGRESS)?.is_file() {
            writeln!(
                sections.progress,
                "- {brief} · {}",
                link("Agent progress", &format!("runs/{run}/{PROGRESS}"))
            )?;
        }
        return Ok(false);
    };
    let recent =
        DateTime::<Utc>::from(fs::metadata(file(&root, ".anydock/state.json")?)?.modified()?)
            >= cutoff;
    if !recent
        && !active
        && !state.stages.values().any(|entry| {
            entry
                .finalized_at
                .is_some_and(|at| at >= cutoff.timestamp())
        })
    {
        return Ok(false);
    }
    let plan = Plan::load(&root)?;
    state.verify_archives(&root, false)?;
    let changes = policy::changes(&state, &plan)?;
    for change in changes.iter().filter(|_| active) {
        writeln!(
            sections.blockers,
            "- {brief} · review requirement changed for {}",
            code(change.stage)
        )?;
    }
    if !changes.is_empty() {
        return Ok(false);
    }
    let view = engine::evaluate(&root, &plan, &mut state, false)?;
    for item in view.stages.iter().filter(|item| item.phase == Phase::Done) {
        let recorded = state.stages[&item.stage.id].finalized_at;
        let reused = state.update.as_ref().is_some_and(|update| {
            update
                .reused
                .get(&item.stage.id)
                .is_some_and(|entry| item.revision.as_ref() == Some(&entry.revision))
        });
        if !reused && recorded.map_or(recent, |at| at >= cutoff.timestamp()) {
            let when = recorded
                .and_then(|timestamp| DateTime::from_timestamp(timestamp, 0))
                .map_or_else(
                    || "completion time unavailable".into(),
                    |time| {
                        time.with_timezone(&Local)
                            .format("%Y-%m-%d %H:%M %:z")
                            .to_string()
                    },
                );
            writeln!(
                sections.completed,
                "- {brief} · {} · {when}{}",
                code(&item.stage.id),
                output_links(run, &item.stage.fields.outputs)
            )?;
        }
    }
    if !active {
        return Ok(false);
    }
    let Some(action) = view.action(state.batch_review) else {
        return Ok(true);
    };
    match action.phase {
        Phase::Review => {
            let review_command = code(&format!("{cli} review {run}"));
            let revisions = if state.batch_review {
                review::pending_batch(&view, &state)
                    .ok()
                    .map(|(token, _)| token)
            } else {
                action
                    .revision
                    .as_deref()
                    .and_then(|rev| state.review_token(&action.stage.id, rev))
            };
            let note = revisions.as_deref().map_or_else(
                || format!("A draft has an unrecorded revision. Get its current exact revision with {review_command}."),
                |token| format!("Pending revision {}. Review the linked draft and its changes.", code(token)),
            );
            writeln!(sections.decisions, "- {brief} · {note}")?;
            for item in view.stages.iter().filter(|item| {
                item.phase == Phase::Review
                    && (state.batch_review || item.stage.id == action.stage.id)
            }) {
                let decision = item
                    .stage
                    .fields
                    .review
                    .as_deref()
                    .unwrap_or("Review handoff");
                writeln!(
                    sections.decisions,
                    "  - {} · {}{}",
                    code(&item.stage.id),
                    inline(decision),
                    output_links(run, &item.stage.fields.outputs)
                )?;
                if revisions.is_some() {
                    for change in review::approved_changes(space, &root, &state, item)? {
                        writeln!(
                            sections.decisions,
                            "    - Changes since approval in {}
",
                            inline(&change.file)
                        )?;
                        for line in change.lines {
                            writeln!(sections.decisions, "          {line}")?;
                        }
                        writeln!(sections.decisions)?;
                        if change.truncated {
                            writeln!(
                                sections.decisions,
                                "      Open the full file for the complete changes.
"
                            )?;
                        }
                    }
                }
            }
        }
        Phase::Ready | Phase::Running => {
            let checkpoint = state
                .stages
                .get(&action.stage.id)
                .and_then(|entry| entry.attempt.as_ref())
                .and_then(|attempt| attempt.checkpoint.as_deref());
            let contract = link(
                &action.stage.id,
                &format!("runs/{run}/{}", action.stage.contract_path()),
            );
            let status = action.attempt.map_or_else(
                || action.phase.label().into(),
                |attempt| format!("attempt {attempt}"),
            );
            writeln!(sections.progress, "- {brief} · {contract} · {status}")?;
            for output in &action.stage.fields.outputs {
                if file(&root, output)?.is_file() {
                    writeln!(
                        sections.progress,
                        "  - {}",
                        link("Working file", &format!("runs/{run}/{output}"))
                    )?;
                }
            }
            if let Some(note) = checkpoint {
                writeln!(sections.progress, "  - {}", inline(note))?;
            }
        }
        phase => {
            writeln!(
                sections.blockers,
                "- {brief} · {} · {}",
                code(&action.stage.id),
                phase.label()
            )?;
            for reason in &action.reasons {
                writeln!(sections.blockers, "  - {}", inline(reason))?;
            }
        }
    }
    Ok(false)
}

fn output_links(run: &str, outputs: &[String]) -> String {
    outputs
        .iter()
        .map(|path| {
            let name = path.rsplit('/').next().unwrap_or(path);
            format!(" · {}", link(name, &format!("runs/{run}/{path}")))
        })
        .collect()
}

fn inline(text: &str) -> String {
    let mut result = String::new();
    for character in text.chars() {
        match character {
            '<' => result.push_str("&lt;"),
            '>' => result.push_str("&gt;"),
            '&' => result.push_str("&amp;"),
            '\\' | '\x60' | '*' | '_' | '[' | ']' | '~' | '|' => {
                result.push('\\');
                result.push(character);
            }
            c if c.is_control() => result.push(' '),
            c => result.push(c),
        }
    }
    result
}

fn code(text: &str) -> String {
    format!("\x60{text}\x60")
}

fn link(label: &str, path: &str) -> String {
    let relative = path
        .strip_prefix("briefs/")
        .map_or_else(|| format!("../{path}"), str::to_owned);
    format!("[{}]({relative})", inline(label))
}

#[cfg(test)]
mod observation_tests {
    use super::{Observation, RECENT_SECONDS, Source};

    #[test]
    fn rolling_history_expiration_advances_the_baseline_only_on_time() {
        let mut source = Source {
            digest: "current".into(),
            previous_digest: Some("oldest".into()),
            modified: RECENT_SECONDS,
            new: true,
            observations: vec![
                Observation {
                    at: 30,
                    prior: Some("oldest".into()),
                },
                Observation {
                    at: 120,
                    prior: Some("middle".into()),
                },
            ],
        };
        source.retain_recent(RECENT_SECONDS + 20, false);
        assert_eq!(source.previous_digest.as_deref(), Some("oldest"));
        source.retain_recent(RECENT_SECONDS + 30, false);
        assert_eq!(source.previous_digest.as_deref(), Some("middle"));

        let mut repeat = Source {
            digest: "current".into(),
            previous_digest: None,
            modified: RECENT_SECONDS + 60,
            new: false,
            observations: Vec::new(),
        };
        repeat.observe(Some(source), RECENT_SECONDS + 60);
        assert_eq!(repeat.observations.len(), 1);
        assert_eq!(repeat.observations[0].at, 120);
        repeat.retain_recent(RECENT_SECONDS + 120, false);
        assert!(!repeat.new);
        assert!(repeat.previous_digest.is_none());
    }
}
