use crate::{
    storage::{hash, sync_dir},
    workspace::{file, optional, validate_ref},
};
use anyhow::{Context, Result, ensure};
use serde::Deserialize;
use std::{fs, io::Write, path::Path};
use tempfile::NamedTempFile;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Reference {
    pub path: String,
    pub section: Option<String>,
}

impl Reference {
    pub fn validate(&self, root: &Path) -> Result<()> {
        validate_ref(&self.path)?;
        ensure!(
            self.path.starts_with("references/"),
            "references must be under references/"
        );
        if let Some(title) = &self.section {
            ensure!(
                !title.trim().is_empty() && title.trim() == title && !title.contains(['\r', '\n']),
                "invalid reference heading in {}",
                self.path
            );
        }
        file(root, &self.path)?;
        Ok(())
    }

    pub fn label(&self) -> String {
        match &self.section {
            Some(title) => format!("{} (heading {title:?})", self.path),
            None => self.path.clone(),
        }
    }

    pub fn digest(&self, bytes: &[u8]) -> Result<String> {
        Ok(hash(self.select(bytes)?))
    }

    pub fn select<'a>(&self, bytes: &'a [u8]) -> Result<&'a [u8]> {
        if let Some(title) = &self.section {
            let selected = section(std::str::from_utf8(bytes)?, title)
                .with_context(|| format!("{} heading {title:?}", self.path))?;
            Ok(selected.as_bytes())
        } else {
            Ok(bytes)
        }
    }
}

pub fn bytes(root: &Path, name: &str) -> Result<Option<Vec<u8>>> {
    optional(fs::read(file(root, name)?)).with_context(|| format!("read reference {name}"))
}

pub fn write(root: &Path, name: &str, content: &[u8], replace: bool) -> Result<()> {
    let dest = file(root, name)?;
    let parent = dest.parent().context("missing reference folder")?;
    fs::create_dir_all(parent)?;
    let mut temp = NamedTempFile::new_in(parent)?;
    temp.write_all(content)?;
    temp.as_file().sync_all()?;
    if replace {
        temp.persist(&dest).with_context(|| format!("pin {name}"))?;
    } else {
        temp.persist_noclobber(&dest)
            .with_context(|| format!("pin {name}"))?;
    }
    sync_dir(parent)
}

fn section<'a>(text: &'a str, title: &str) -> Result<&'a str> {
    let mut start = None;
    let mut end = text.len();
    let mut depth = 0;
    let mut cursor = 0;
    let mut fence = None;
    for raw in text.split_inclusive('\n') {
        let line = raw.trim_end_matches(['\r', '\n']);
        let left = line.trim_start_matches(' ');
        if line.len() - left.len() <= 3 {
            if let Some((marker, width)) = fence {
                let count = left.bytes().take_while(|b| *b == marker).count();
                if count >= width && left[count..].trim().is_empty() {
                    fence = None;
                }
            } else if let Some(marker @ (b'\x60' | b'~')) = left.as_bytes().first().copied()
                && left.bytes().take_while(|b| *b == marker).count() >= 3
            {
                fence = Some((marker, left.bytes().take_while(|b| *b == marker).count()));
            } else if let Some((level, heading)) = heading(left.trim_start_matches('\u{feff}')) {
                if start.is_some() && level <= depth && end == text.len() {
                    end = cursor;
                }
                if heading == title {
                    ensure!(start.is_none(), "heading is ambiguous");
                    start = Some(cursor);
                    depth = level;
                }
            }
        }
        cursor += raw.len();
    }
    let start = start.context("heading is absent")?;
    Ok(&text[start..end])
}

fn heading(line: &str) -> Option<(usize, &str)> {
    let level = line.bytes().take_while(|b| *b == b'#').count();
    if (1..=6).contains(&level)
        && let Some(title) = line[level..].strip_prefix([' ', '\t'])
    {
        return Some((level, title.trim()));
    }
    None
}
