use crate::workspace::{Workspace, file, optional};
use anyhow::{Context, Result, ensure};
use serde::Deserialize;
use std::{
    fs,
    io::{self, IsTerminal},
};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Config {
    mode: Mode,
}

#[derive(Deserialize)]
#[serde(rename_all = "kebab-case")]
enum Mode {
    HumanTty,
}

#[derive(Clone, Copy)]
pub struct Terminal;

pub enum Choice {
    Approve,
    Reject,
    Skip,
}

pub fn configured_terminal(space: &Workspace) -> Result<Option<Terminal>> {
    const CONFIG: &str = ".anydock/approval.json";
    let Some(bytes) = optional(fs::read(file(&space.root, CONFIG)?))? else {
        return Ok(None);
    };
    let config: Config =
        serde_json::from_slice(&bytes).with_context(|| format!("read {CONFIG}"))?;
    match config.mode {
        Mode::HumanTty => Terminal::require().map(Some),
    }
}

impl Terminal {
    pub fn require() -> Result<Self> {
        ensure!(
            io::stdin().is_terminal() && io::stderr().is_terminal(),
            "this command requires a human terminal confirmation with interactive stdin and stderr"
        );
        Ok(Self)
    }

    pub fn subject<'a>(
        self,
        run: &str,
        revision: &str,
        stages: impl IntoIterator<Item = (&'a str, &'a [String])>,
    ) {
        eprintln!("Review {run} | revision {}", &revision[..12]);
        for (stage, files) in stages {
            eprintln!("  Stage: {stage}");
            for path in files {
                eprintln!("  File: runs/{run}/{path}");
            }
        }
    }

    pub fn confirm(self, action: &str) -> Result<()> {
        let typed = self.read(&format!("Type \"{action}\" or \"y\" to confirm [y/N]"))?;
        ensure!(confirmed(&typed, action), "decision cancelled");
        Ok(())
    }

    pub fn choose(self) -> Result<Choice> {
        match self
            .read("[a]pprove / [r]eject / [s]kip [s]")?
            .to_ascii_lowercase()
            .as_str()
        {
            "a" | "approve" => Ok(Choice::Approve),
            "r" | "reject" => Ok(Choice::Reject),
            "" | "s" | "skip" => Ok(Choice::Skip),
            _ => anyhow::bail!("enter a, r, or s; no decision was recorded"),
        }
    }

    pub fn read(self, prompt: &str) -> Result<String> {
        use std::io::Write;
        io::stdout().flush()?;
        eprintln!("{prompt}");
        let mut typed = String::new();
        io::stdin().read_line(&mut typed)?;
        Ok(typed.trim().to_owned())
    }
}

fn confirmed(typed: &str, action: &str) -> bool {
    typed.eq_ignore_ascii_case(action) || typed.eq_ignore_ascii_case("y")
}

#[cfg(test)]
mod tests {
    use super::confirmed;

    #[test]
    fn terminal_accepts_an_explicit_action_or_y() {
        assert!(confirmed("approve", "approve"));
        assert!(confirmed("Y", "approve"));
        assert!(!confirmed("deadbeef0012", "approve"));
        assert!(!confirmed("", "approve"));
        assert!(!confirmed("reject", "approve"));
    }
}
