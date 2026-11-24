use crate::{
    storage::{Hashes, hash},
    workspace::{Stage, file, input_component},
};
use anyhow::{Context, Result, ensure};
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
    for relative in markdown_targets(output, markdown) {
        let relative = relative?;
        ensure!(
            file(run, &relative)?.try_exists()?,
            "missing linked file {relative}"
        );
    }
    Ok(())
}

pub fn markdown_targets<'a>(
    output: &'a str,
    markdown: &'a str,
) -> impl Iterator<Item = Result<String>> + 'a {
    Parser::new(markdown).filter_map(move |event| {
        let Event::Start(Tag::Link { dest_url, .. } | Tag::Image { dest_url, .. }) = event else {
            return None;
        };
        let target = dest_url.split('#').next()?;
        if target.is_empty()
            || ["https://", "http://", "mailto:", "tel:"]
                .iter()
                .any(|scheme| target.starts_with(scheme))
        {
            return None;
        }
        Some(resolve_link(output, target))
    })
}

fn resolve_link(output: &str, target: &str) -> Result<String> {
    ensure!(
        !target.starts_with('/'),
        "expected a run-local relative link, found {target:?}"
    );
    let (parent, _) = output.rsplit_once('/').context("invalid output path")?;
    let mut components: Vec<_> = parent.split('/').map(str::to_owned).collect();
    for part in target.split('/') {
        let part =
            decode_url_part(part).with_context(|| format!("invalid link target {target:?}"))?;
        match part.as_str() {
            "." => {}
            ".." => {
                components
                    .pop()
                    .with_context(|| format!("link escapes the run: {target:?}"))?;
            }
            _ => {
                input_component(&part)
                    .with_context(|| format!("invalid link target {target:?}"))?;
                components.push(part);
            }
        }
    }
    ensure!(
        !components.is_empty(),
        "link does not name a file: {target:?}"
    );
    Ok(components.join("/"))
}

fn decode_url_part(part: &str) -> Result<String> {
    let mut decoded = Vec::with_capacity(part.len());
    let mut bytes = part.bytes();
    while let Some(byte) = bytes.next() {
        if byte == b'%' {
            let hi = bytes
                .next()
                .and_then(|byte| char::from(byte).to_digit(16))
                .context("invalid URL escape")?;
            let lo = bytes
                .next()
                .and_then(|byte| char::from(byte).to_digit(16))
                .context("invalid URL escape")?;
            decoded.push((hi * 16 + lo) as u8);
        } else {
            decoded.push(byte);
        }
    }
    Ok(String::from_utf8(decoded)?)
}
