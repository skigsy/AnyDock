use crate::{
    storage::{Hashes, hash},
    workspace::{Stage, file, segment},
};
use anyhow::{Context, Result, bail, ensure};
use pulldown_cmark::{Event, Parser, Tag};
use serde::Deserialize;
use std::{collections::BTreeSet, fs, path::Path};

#[derive(Clone, Deserialize, Eq, Ord, PartialEq, PartialOrd)]
#[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
pub enum Check {
    Nonempty { path: String },
    CsvHeader { path: String, columns: Vec<String> },
    JsonKeys { path: String, keys: Vec<String> },
    MarkdownLinks { path: String },
}

impl Check {
    pub fn path(&self) -> &str {
        match self {
            Self::Nonempty { path }
            | Self::CsvHeader { path, .. }
            | Self::JsonKeys { path, .. }
            | Self::MarkdownLinks { path } => path,
        }
    }

    pub fn label(&self) -> String {
        let kind = match self {
            Self::Nonempty { .. } => "nonempty",
            Self::CsvHeader { .. } => "csv-header",
            Self::JsonKeys { .. } => "json-keys",
            Self::MarkdownLinks { .. } => "markdown-links",
        };
        format!("{kind} {}", self.path())
    }

    fn verify(&self, run: &Path, bytes: &[u8]) -> Result<()> {
        match self {
            Self::Nonempty { .. } => {
                ensure!(
                    bytes.iter().any(|b| !b.is_ascii_whitespace()),
                    "file is empty"
                );
            }
            Self::CsvHeader { columns, .. } => {
                let mut reader = csv::Reader::from_reader(bytes);
                let actual = reader.headers()?;
                ensure!(
                    actual.iter().eq(columns.iter().map(String::as_str)),
                    "expected columns {columns:?}, found {actual:?}",
                );
                for (index, record) in reader.records().enumerate() {
                    record.with_context(|| format!("CSV data row {}", index + 1))?;
                }
            }
            Self::JsonKeys { keys, .. } => {
                let value: serde_json::Value = serde_json::from_slice(bytes)?;
                let object = value.as_object().context("expected a JSON object")?;
                let absent: Vec<_> = keys
                    .iter()
                    .filter(|key| !object.contains_key(*key))
                    .collect();
                ensure!(absent.is_empty(), "missing keys {absent:?}");
            }
            Self::MarkdownLinks { path } => markdown_links(run, path, std::str::from_utf8(bytes)?)?,
        }
        Ok(())
    }
}

pub fn validate(stage: &Stage) -> Result<()> {
    let mut checks = BTreeSet::new();
    for check in &stage.fields.checks {
        ensure!(
            stage.fields.outputs.iter().any(|path| path == check.path()),
            "{} has a check for an undeclared output {}",
            stage.id,
            check.path()
        );
        ensure!(
            checks.insert(check),
            "{} repeats {}",
            stage.id,
            check.label()
        );
        let names = match check {
            Check::CsvHeader { columns, .. } => columns,
            Check::JsonKeys { keys, .. } => keys,
            _ => continue,
        };
        ensure!(
            !names.is_empty()
                && names.iter().all(|name| !name.trim().is_empty())
                && names.iter().collect::<BTreeSet<_>>().len() == names.len(),
            "{} requires distinct nonempty field names for {}",
            stage.id,
            check.label()
        );
    }
    Ok(())
}

pub fn run(run: &Path, stage: &Stage, outputs: &Hashes) -> (Vec<String>, Vec<String>) {
    let mut passed = Vec::new();
    let mut errors = Vec::new();
    for check in &stage.fields.checks {
        let result = (|| {
            let content = fs::read(file(run, check.path())?)?;
            ensure!(
                outputs.get(check.path()) == Some(&hash(&content)),
                "file changed during verification"
            );
            check.verify(run, &content)
        })();
        match result {
            Ok(()) => passed.push(check.label()),
            Err(err) => errors.push(format!("check failed: {} ({err:#})", check.label())),
        }
    }
    (passed, errors)
}

fn markdown_links(run: &Path, output: &str, markdown: &str) -> Result<()> {
    for event in Parser::new(markdown) {
        let Event::Start(Tag::Link { dest_url, .. } | Tag::Image { dest_url, .. }) = event else {
            continue;
        };
        let target = dest_url.split('#').next().unwrap_or_default();
        if target.is_empty()
            || ["https://", "http://", "mailto:", "tel:"]
                .iter()
                .any(|scheme| target.starts_with(scheme))
        {
            continue;
        }
        ensure!(
            !target.starts_with('/'),
            "expected a run-local relative link, found {target:?}"
        );
        let (parent, _) = output.rsplit_once('/').context("invalid output path")?;
        let mut components: Vec<_> = parent.split('/').collect();
        for part in target.split('/') {
            match part {
                "." => {}
                ".." => {
                    components
                        .pop()
                        .with_context(|| format!("link escapes the run: {target:?}"))?;
                }
                _ => {
                    segment(part).with_context(|| format!("invalid link target {target:?}"))?;
                    components.push(part);
                }
            }
        }
        if components.is_empty() {
            bail!("link does not name a file: {target:?}");
        }
        let relative = components.join("/");
        ensure!(
            file(run, &relative)?.try_exists()?,
            "missing linked file {relative}"
        );
    }
    Ok(())
}
