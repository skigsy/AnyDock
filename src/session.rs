use anyhow::{Context, Result, ensure};
use clap::Parser;
use serde::{Deserialize, Serialize};
use std::io::{self, BufRead, Read, Write};

use crate::{Cli, Command, TransferCommand, dispatch};

const LIMIT: usize = 64 * 1024;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Request {
    id: String,
    args: Vec<String>,
}

#[derive(Serialize)]
struct Response<'a> {
    id: &'a str,
    ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

pub fn serve() -> Result<()> {
    let mut input = io::stdin().lock();
    loop {
        let mut line = String::new();
        let read = (&mut input).take((LIMIT + 1) as u64).read_line(&mut line)?;
        if read == 0 {
            break;
        }
        ensure!(read <= LIMIT, "session request exceeds {LIMIT} bytes");
        let request: Request = serde_json::from_str(&line).context("invalid session request")?;
        ensure!(
            !request.id.is_empty()
                && request.id.len() <= 64
                && request
                    .id
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-'),
            "invalid session request identifier"
        );
        let outcome = run(request.args);
        let response = Response {
            id: &request.id,
            ok: outcome.is_ok(),
            error: outcome.err().map(|error| format!("{error:#}")),
        };
        let mut output = io::stdout().lock();
        writeln!(output, "\x1e{}", serde_json::to_string(&response)?)?;
        output.flush()?;
    }
    Ok(())
}

fn run(args: Vec<String>) -> Result<()> {
    let cli = Cli::try_parse_from(std::iter::once("anydock".to_owned()).chain(args))?;
    ensure!(
        !matches!(
            cli.command,
            Command::Session | Command::Transfer(TransferCommand::Switch { .. })
        ),
        "command requires a separate AnyDock process"
    );
    dispatch(cli.command)
}
