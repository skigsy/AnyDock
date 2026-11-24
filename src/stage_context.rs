use crate::{
    input_scope, reference,
    workspace::{Plan, Stage, file},
};
use anyhow::{Context, Result};
use std::{fs::File, io::Read, path::Path};

const PER_FILE: usize = 4096;
const TOTAL: usize = 12_288;

pub fn show(
    run: &Path,
    id: &str,
    plan: &Plan,
    stage: &Stage,
    format: input_scope::Format,
) -> Result<()> {
    let command = crate::workspace::run_invocation(run);
    let mut available = TOTAL;
    let mut show = |label: &str, bytes: &[u8]| {
        println!("\n--- {label} ---");
        if available == 0 {
            println!("Context limit reached.");
            return;
        }
        let size = available.min(PER_FILE).min(bytes.len());
        let prefix = &bytes[..size];
        if prefix.contains(&0) {
            println!("Read the file with an appropriate tool.");
            return;
        }
        let valid = match std::str::from_utf8(prefix) {
            Ok(text) => text,
            Err(error) if error.error_len().is_none() => {
                std::str::from_utf8(&prefix[..error.valid_up_to()]).unwrap_or_default()
            }
            Err(_) => {
                println!("Read the file with an appropriate tool.");
                return;
            }
        };
        println!("{valid}");
        available -= size;
        if size < bytes.len() {
            println!("Preview truncated.");
        }
    };
    show("brief.md", &plan.brief);
    show(&stage.contract_path(), &stage.contract);
    for name in &stage.fields.inputs {
        if let Some(pointers) = stage.fields.input_fields.get(name) {
            let data = input_scope::read(run, name, pointers, format)?
                .with_context(|| format!("missing input {name}"))?;
            let label = format!("Selected JSON values in {name}");
            show(&label, &data);
            println!(
                "Full selected values: {command} input {id} {} {}",
                stage.id,
                crate::workspace::shell_word(name)
            );
            continue;
        }
        let mut preview = Vec::new();
        File::open(file(run, name)?)?
            .take((PER_FILE + 1) as u64)
            .read_to_end(&mut preview)?;
        show(name, &preview);
    }
    for item in &stage.fields.references {
        let bytes = reference::bytes(run, &item.path)?
            .with_context(|| format!("missing reference {}", item.path))?;
        show(&item.label(), item.select(&bytes)?);
    }
    Ok(())
}
