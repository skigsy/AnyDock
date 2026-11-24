# AnyDock

![AnyDock. Start with one prompt. Keep your work when you change agents.](docs/assets/anydock-banner.svg)

**Your agent can change. Your work stays with you.**

AnyDock is a local AI workspace for the work you already do. Open a folder, give your agent a real task, and put its brief, inputs, editable drafts, and decisions on disk. Tomorrow, a fresh session can see what finished, what changed, and exactly where to pick up.

**One prompt to set up. Model- and harness-independent by design.** Start with a single task, then keep useful workflows as editable files. The same folder can grow with your work and still open with whatever agent you choose next.

[**Get AnyDock v0.1.1**](https://github.com/skigsy/AnyDock/releases/tag/v0.1.1) · [**One-prompt install**](#one-prompt-install) · [Why it carries forward](#why-it-carries-forward) · [CLI guide](USAGE.md)

## One-prompt install

Paste this into an agent that can read and write local files and run shell commands. Fill in the bracketed lines.

```text
Set up AnyDock at [local folder] for [what I want to use it for].
Bring in [the documents, projects, skills or agent definitions I name, with
their current paths, or say "none yet"].

Use the AnyDock folder I already opened, or get v0.1.1 from
https://github.com/skigsy/AnyDock/releases/tag/v0.1.1 and put it at the location
I gave you. On Windows x64, use the executable ZIP and verify its SHA-256
against the release checksums. If using the source, read its build instructions.

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

- **Windows x64** — [Download the executable ZIP](https://github.com/skigsy/AnyDock/releases/download/v0.1.1/AnyDock-0.1.1-windows-x64.zip), extract it, and open its root in your agent. In PowerShell, run `.\bin\anydock.exe check` to verify the empty workspace.
- **Source** — Download the source from [v0.1.1](https://github.com/skigsy/AnyDock/releases/tag/v0.1.1) or clone this repo. Open the root to start in agent-only mode. For the CLI, install Rust 1.89 or later and your platform's linker, then run `cargo build --locked --release` and `cargo run --locked -- check`.

Inputs, artifacts, workflows, and state are ignored by Git by default. Your chosen agent handles model calls under its own settings. The AnyDock CLI reads and writes local files.

[CLI and workspace guide](USAGE.md) · [Workspace instructions](AGENTS.md)
