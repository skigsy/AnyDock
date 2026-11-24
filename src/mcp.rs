use crate::{handoff, queue, recurring, workspace::Workspace};
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, de::DeserializeOwned};
use serde_json::{Value, json};
use std::{
    io::{self, BufRead, Read, Write},
    process::{Command, Stdio},
};

const LIMIT: usize = 64 * 1024;
const VERSION: &str = "2025-11-25";

#[derive(Deserialize)]
struct Request {
    jsonrpc: String,
    id: Option<Value>,
    method: String,
    #[serde(default)]
    params: Value,
}

#[derive(Deserialize)]
struct Initialize {
    #[serde(rename = "protocolVersion")]
    version: String,
}

#[derive(Deserialize)]
struct Call {
    name: String,
    #[serde(default)]
    arguments: Value,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Run {
    run: String,
}

#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct Status {
    run: Option<String>,
}

#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct Queue {
    #[serde(default)]
    all: bool,
    #[serde(default)]
    sources: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Next {
    run: String,
    #[serde(default)]
    retry: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Complete {
    run: String,
    stage: String,
    attempt: u64,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Refresh {
    task: String,
    source_revision: String,
}

pub fn serve(space: &Workspace, allow_work: bool) -> Result<()> {
    let mut input = io::stdin().lock();
    let mut output = io::stdout().lock();
    let mut protocol = None;
    let mut ready = false;
    loop {
        let mut line = Vec::new();
        if (&mut input)
            .take((LIMIT + 1) as u64)
            .read_until(b'\n', &mut line)?
            == 0
        {
            return Ok(());
        }
        let response = if line.len() > LIMIT {
            if !line.ends_with(b"\n") {
                input.skip_until(b'\n')?;
            }
            error(Value::Null, -32700, "request exceeds 64 KiB")
        } else if let Ok(request) = serde_json::from_slice::<Request>(&line) {
            if request.jsonrpc != "2.0"
                || request
                    .id
                    .as_ref()
                    .is_some_and(|id| !id.is_string() && !id.is_i64() && !id.is_u64())
            {
                error(Value::Null, -32600, "invalid JSON-RPC request")
            } else if let Some(id) = request.id {
                match request.method.as_str() {
                    "ping" => result(id, json!({})),
                    "initialize" if protocol.is_none() => {
                        if let Ok(initial) = decode::<Initialize>(request.params) {
                            let negotiated = match initial.version.as_str() {
                                "2024-11-05" => "2024-11-05",
                                "2025-03-26" => "2025-03-26",
                                "2025-06-18" => "2025-06-18",
                                _ => VERSION,
                            };
                            protocol = Some(negotiated);
                            result(
                                id,
                                json!({"protocolVersion": negotiated, "capabilities":{"tools":{}},
                                "serverInfo":{"name":"anydock", "version":env!("CARGO_PKG_VERSION")}}),
                            )
                        } else {
                            error(id, -32602, "protocolVersion is required")
                        }
                    }
                    _ if !ready => error(id, -32002, "complete the MCP initialization first"),
                    "tools/list" => result(id, json!({"tools": list(allow_work)})),
                    "tools/call" => match decode::<Call>(request.params) {
                        Ok(call) => {
                            let outcome = call_tool(space, allow_work, call);
                            let (value, failed, text) = match outcome {
                                Ok(value) => {
                                    let text = value
                                        .get("text")
                                        .and_then(Value::as_str)
                                        .map_or_else(|| value.to_string(), str::to_owned);
                                    (value, false, text)
                                }
                                Err(err) => (Value::Null, true, format!("{err:#}")),
                            };
                            let mut response =
                                json!({"content":[{"type":"text","text":text}],"isError":failed});
                            if let Some("2025-06-18" | VERSION) = protocol
                                && !failed
                            {
                                response["structuredContent"] = value;
                            }
                            result(id, response)
                        }
                        Err(_) => error(id, -32602, "invalid tool request"),
                    },
                    _ => error(id, -32601, "method not found"),
                }
            } else {
                if request.method == "notifications/initialized" && protocol.is_some() {
                    ready = true;
                }
                continue;
            }
        } else {
            let code = if serde_json::from_slice::<Value>(&line).is_ok() {
                -32600
            } else {
                -32700
            };
            error(Value::Null, code, "invalid JSON-RPC message")
        };
        serde_json::to_writer(&mut output, &response)?;
        writeln!(output)?;
        output.flush()?;
    }
}

fn decode<T: DeserializeOwned>(value: Value) -> Result<T> {
    serde_json::from_value(value).context("invalid arguments")
}

fn result(id: Value, value: Value) -> Value {
    json!({"jsonrpc":"2.0","id":id,"result":value})
}
fn error(id: Value, code: i32, message: &str) -> Value {
    json!({"jsonrpc":"2.0","id":id,"error":{"code":code,"message":message}})
}

fn list(allow_work: bool) -> Vec<Value> {
    let run = json!({"type":"string", "description":"Existing AnyDock run name"});
    let tool = |name, description, properties, required, read_only| {
        json!({
            "name":name, "description":description,
            "inputSchema":{"type":"object","properties":properties,"required":required,"additionalProperties":false},
            "annotations":{"readOnlyHint":read_only, "openWorldHint":false}
        })
    };
    let mut tools = vec![
        tool(
            "handoff",
            "Read a run's portable continuation packet.",
            json!({"run":run}),
            json!(["run"]),
            true,
        ),
        tool(
            "status",
            "List workspace runs, or inspect the state, pending revision and blockers of one run.",
            json!({"run":run}),
            json!([]),
            true,
        ),
        tool(
            "queue",
            "List recurring tasks and pending reviews. Set sources to inspect saved source changes.",
            json!({"all":{"type":"boolean"},"sources":{"type":"boolean"}}),
            json!([]),
            true,
        ),
    ];
    if allow_work {
        tools.extend([
            tool("next", "Open or recover an attempt only when work is authorized.", json!({"run":run,"retry":{"type":"boolean"}}), json!(["run"]), false),
            tool("complete", "Record the output of the exact authorized attempt.", json!({"run":run,"stage":{"type":"string"},"attempt":{"type":"integer","minimum":1}}), json!(["run","stage","attempt"]), false),
            tool("review", "Record observed edits and show the exact pending human decision.", json!({"run":run}), json!(["run"]), false),
            tool("refresh", "Submit the exact saved source packet when the user authorizes it.", json!({"task":{"type":"string"},"source_revision":{"type":"string"}}), json!(["task","source_revision"]), false),
        ]);
    }
    tools
}

fn call_tool(space: &Workspace, allow_work: bool, call: Call) -> Result<Value> {
    let name = call.name.as_str();
    match name {
        "handoff" => {
            let Run { run } = decode(call.arguments)?;
            return state(space, &run);
        }
        "status" => {
            let Status { run } = optional_arguments(call.arguments)?;
            return match run {
                Some(run) => state(space, &run),
                None => Ok(serde_json::to_value(handoff::overview(space)?)?),
            };
        }
        "queue" => {
            let Queue { all, sources } = optional_arguments(call.arguments)?;
            return Ok(serde_json::to_value(queue::collect(space, all, sources)?)?);
        }
        _ => {}
    }
    ensure!(allow_work, "this MCP connection exposes read-only tools");
    let (args, run, task) = match name {
        "next" => {
            let Next { run, retry } = decode(call.arguments)?;
            let mut args = vec![name.into(), run.clone(), "--context".into()];
            if retry {
                args.push("--retry".into());
            }
            (args, run, false)
        }
        "complete" => {
            let Complete {
                run,
                stage,
                attempt,
            } = decode(call.arguments)?;
            (
                vec![
                    name.into(),
                    run.clone(),
                    stage,
                    "--attempt".into(),
                    attempt.to_string(),
                    "--quiet".into(),
                ],
                run,
                false,
            )
        }
        "review" => {
            let Run { run } = decode(call.arguments)?;
            (vec![name.into(), run.clone()], run, false)
        }
        "refresh" => {
            let Refresh {
                task,
                source_revision,
            } = decode(call.arguments)?;
            (
                vec![
                    name.into(),
                    task.clone(),
                    "--submit".into(),
                    "--source-revision".into(),
                    source_revision,
                ],
                task,
                true,
            )
        }
        _ => anyhow::bail!("unknown tool {name}"),
    };
    let output = Command::new(std::env::current_exe()?)
        .args(args)
        .current_dir(&space.root)
        .stdin(Stdio::null())
        .output()?;
    ensure!(output.status.success(), "{}", bounded(&output.stderr));
    let run = if task {
        recurring::inspect(space, &run)?
            .context("task not found")?
            .run
    } else {
        run
    };
    Ok(json!({"text":bounded(&output.stdout), "handoff": state(space, &run)?}))
}

fn optional_arguments<T: Default + DeserializeOwned>(arguments: Value) -> Result<T> {
    if arguments.is_null() {
        Ok(T::default())
    } else {
        decode(arguments)
    }
}

fn state(space: &Workspace, run: &str) -> Result<Value> {
    Ok(serde_json::to_value(handoff::collect(
        &space.run(run)?,
        run,
    )?)?)
}

fn bounded(bytes: &[u8]) -> String {
    let mut text = String::from_utf8_lossy(&bytes[..bytes.len().min(LIMIT)]).into_owned();
    if bytes.len() > LIMIT {
        text.push_str("\nOutput truncated. Open the named files for the rest.\n");
    }
    text
}
