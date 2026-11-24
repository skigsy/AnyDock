# AnyDock

![AnyDock. Start with one prompt. Keep your work when you change agents.](docs/assets/anydock-banner.svg)

**Your agent can change. Your work stays with you.**

AnyDock is a local AI workspace for the work you already do. Open a folder, give your agent a real task, and put its brief, inputs, editable drafts, and decisions on disk. Tomorrow, a fresh session can see what finished, what changed, and exactly where to pick up.

**One prompt to set up. Model- and harness-independent by design.** Start with a single task, then keep useful workflows as editable files. The same folder can grow with your work and still open with whatever agent you choose next.

[**Releases**](https://github.com/skigsy/AnyDock/releases) · [**One-prompt install**](#one-prompt-install) · [Worked example](docs/example.md) · [CLI guide](USAGE.md)

## One-prompt install

Paste this into an agent that can read and write local files and run shell commands. Fill in the bracketed lines.

```text
Set up AnyDock at [local folder] for [what I want to use it for].
Bring in [the documents, projects, skills or agent definitions I name, with
their current paths, or say "none yet"].

Use the extracted AnyDock 0.2.0 folder I already opened or a 0.2.0 package
I provide. Verify a package against its SHA-256 checksum before extracting it.
If I asked to use an existing project, use the packaged binary with
`-C "<project folder>" init`. Leave any existing project instructions intact.

Read AGENTS.md. Copy only the named material. Keep projects and their Git
metadata together. Preserve originals unless I explicitly asked you to move
them. Keep imported work out of AnyDock's Git history. Bring in skills and
agent definitions as local files and enable only the ones I asked to use.

Set up only the features I asked for. Leave the daily brief disabled unless
I explicitly ask for it. Leave the workspace idle until I give you work to
execute. If the AnyDock CLI is available, run its check command from the
workspace root. Show me where everything went, which mode is active, and what
to tell a fresh agent when I want to continue.
```

## Why it carries forward

[![The local workspace carries the run brief, stage contracts, editable outputs, and exact review state.](docs/assets/workspace-handoff.svg)](docs/assets/workspace-handoff.svg)

- **Start Monday's work on Tuesday.** Open the same folder in a fresh session and ask the agent to resume. The managed handoff identifies the current files, the next action, and any decision that stands in its way.
- **Change the model or harness without migrating the run.** The new agent needs access to your workspace files and a terminal for managed work. Read `AGENTS.md` and use `anydock handoff <run>`. The brief, stage contracts, attempts, and approvals stay in the folder. Your login, tools, and permissions remain with your chosen harness.
- **Keep the parts that work.** Save your stage definitions and approved corrections for later jobs. When a recurring input changes, managed runs can carry forward untouched work and its approvals. You review the work the change actually affects.
- **Keep approval attached to the file you saw.** The CLI binds approval to an exact revision. Edit the draft in any editor and AnyDock asks for a new approval before affected work continues.

A fresh workspace starts empty. The built-in instructions work on their own. Add the Rust CLI for exact review gates, input tracking, archived revisions, and recoverable transitions. The morning Markdown brief is optional. If you enable it, it brings your local feeds, recent work, and pending decisions into one file.

## Download or build

Use the 0.2.0 archive for your platform from the [release page](https://github.com/skigsy/AnyDock/releases) when available, or an archive you already have. Each archive extracts to `AnyDock-0.2.0/`. Open that folder in your agent.

| Platform | Archive | Check |
| --- | --- | --- |
| Windows x64 | `AnyDock-0.2.0-windows-x64.zip` | `.\bin\anydock.exe check` |
| Linux x64 | `AnyDock-0.2.0-linux-x64.tar.gz` | `./bin/anydock check` |
| macOS ARM64 | `AnyDock-0.2.0-macos-arm64.tar.gz` | `./bin/anydock check` |

For a source checkout, open the root to start in agent-only mode. Build the CLI with `cargo build --locked --release` on Rust 1.89 or newer with your platform linker installed.

Use `run --source "<file>"` for authorized single-stage work with files you already have. It preserves ordinary filenames and snapshots the source before work starts. Use `handoff <run>` to resume and `review <run> --interactive` to read and decide in one terminal. `reject` preserves the draft and records what should change. Use `upgrade --check` before adopting a newer binary in an existing workspace. `mcp` gives a compatible agent read-only discovery by default.

Inputs, artifacts, workflows, and state are ignored by Git by default. Your chosen agent handles model calls under its own settings. The AnyDock CLI reads and writes local files.

[CLI and workspace guide](USAGE.md) · [Workspace instructions](AGENTS.md)
