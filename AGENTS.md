# AnyDock

AnyDock is an empty workspace for the user's tasks. Opening it requires no setup action. Respond to the user's actual request and use only the files relevant to it. A request to discuss, explore, or plan stays in the conversation. Create a run only when the user authorizes execution. Direct requests to maintain AnyDock itself are ordinary repository work.

## Daily brief

When the user requests a daily overview, run `anydock daily` from the workspace root and show `briefs/daily.md`. For authorized first-time setup with named folders, use repeated `--feed NAME=FOLDER` options. For changes to existing settings, edit `.anydock/daily.json` using only the folders and freshness intervals the user requests. Keep notes separate. If the user authorizes work on a packet shown in the brief, use its exact submission command. If the revision has changed, show the updated evidence and get authorization for that packet. Follow the normal approval gate for the result.

## Update a completed run

For an overview of named recurring tasks, use `anydock queue`. Use `anydock queue --sources` when the user asks what changed in their saved source folders or what is ready to submit. This inspects the saved sources without opening a run. Present the review decision and actual files for the tasks the user asks about. Use the indicated review command to reconcile edits and present the current exact revision.

If the user has a named recurring task, use `anydock refresh <task>` to locate its current run. For an authorized refresh with a complete input set, use `anydock refresh <task> --inbox` or name its inbox subfolder. When the user supplies specific replacements, use repeated `refresh <task> --input inbox/<file>=inputs/<declared-file>` mappings. A repeated packet resumes the same run. Keep its current attempt and review. Only attach a task with `--from <completed-run>` when the user authorizes recurring tracking and the seed is clear.

For an authorized refresh with a complete, same-name input set, start with `anydock update <previous-run> <new-run> --inbox`. Use `--inbox inbox/<folder>` for a full set in a subfolder, or repeated `--input inbox/<file>=inputs/<declared-file>` mappings when replacing selected files. Use a fresh run when the task or workflow changes.

The command verifies the source revisions, checks the copied bytes, carries eligible approvals, and prints the first affected stage's work context. Use that report as the evidence for unchanged work. Write the named output from the printed context, then use the printed `complete` command. Its status identifies the next decision or action. Open additional files when the preview is incomplete or a verification error calls for investigation.

For an authorized correction while a named task waits at batch review, add `--pending` to the `refresh <task>` command with its `--inbox` or `--input` arguments. Use `update --pending` for a standalone run. If the human saw the earlier presentation, retain its exact token with `--since` and use `review <task> --task --retained` after the corrected work is ready. For a standalone run, use `review <run> --since <earlier-batch-token>`. Follow the printed approval command only after explicit approval.

If the user says recurring inputs already land in a stable local folder, use `begin --source <folder>` to bind that folder when opening the task. Use `refresh <task> --submit` only when the user authorizes submitting its current packet. For a corrected packet at a review gate, add `--pending` and use `--since` for an earlier batch review token. Plain `refresh <task>` resumes the saved run without reading the source folder.

## Start authorized work

1. Read the request and relevant user-provided files, including any the user puts in `inbox/`. Check for an existing matching run before creating another one. Ask only when the intended run or authorization is genuinely unclear.
2. For an authorized one-stage task with files in `inbox/`, use `anydock quick <run> --brief <task> --work <instructions> --input inbox/<file> --output <filename>`. Repeat `--input` as needed and pass `--review <decision>` when required. It registers and begins `01-work` with attempt 1. Use its output path and completion command. For other tasks, create `runs/<run>/brief.md` using the brief format below. Record the real deliverable, constraints, and any user decisions needed. Place needed source copies under `runs/<run>/inputs/`. Keep the originals intact.
3. Create only the numbered stages the task needs and declare the exact handoff files. For recurring work, give independently changing deliverables separate stages with their own declared inputs so a change can rebuild only the affected work. If the user wants one approval across those stages, group the reviews. Use one `stage.md` per stage. Read each stage contract and its declared inputs before working on it. Keep the live outputs editable in their stage folders.
4. Use the AnyDock executable from the workspace root when available. It may be on PATH, under the packaged `bin/` directory, or under `target/release/` or `target/debug/`. State which mode applies. In managed mode run `anydock start <run>` for new multi-stage work. For authorized work on a ready or active stage, use `anydock next <run> --context` to get the attempt and the declared material. Open the named files if the preview is incomplete. Use the printed completion command. Completion reports the resulting status. Continue until a gate, a blocker, or completion.
5. At a review gate, show links to the actual editable files, the exact decision and revision from status, and the work it gates. Wait for the human's explicit approval of the displayed revision. The human may run the printed `approve` command or explicitly instruct you to record that approval. Check the revision again before recording it. If a policy change blocks the run, show the previous and proposed decisions, then wait for explicit human authorization of that exact change. An edit, silence, or approval of earlier bytes does not authorize the new revision.


## Repeat a task and keep a correction

When the user authorizes work with a matching saved workflow, use `anydock repeat <workflow> <new-run> --brief <current-task> --input inbox/<file>=inputs/<declared-file>`. Repeat the input option for each declared input or omit it for a workflow with no external inputs. Use the returned stage context, open named files when the preview is incomplete, and follow the normal completion path. For a stage with `input_fields`, use only the selected values. When their preview is incomplete, run `anydock input <run> <stage> <path>` for the full selection. Choose whole-file inputs when the task depends on the remaining fields. Without the executable, track the full file version conservatively. Only choose a saved workflow that fits this task. For recurring work, keep current facts in the brief and inputs so stage instructions can be reused.

If the user wants a correction carried forward from an approved edit, inspect the source with `anydock diff`. Use the named saved workflow or save reusable definitions when asked. Propose the smallest rule that states what to do on the next task. Keep one-off facts in the run. Call `anydock learn <run> <stage> --workflow <workflow> --rule <text>` and show the exact proposed wording, source edit, and destination. Apply the printed confirmation token only after the human authorizes that proposal. A deliverable approval by itself authorizes only that revision.

For a user-authorized new recurring task, create its JSON definition under inbox/ and run `anydock begin <name> --definition inbox/<file>.json --brief "<task>"` with a complete same-name input set. Use `--inbox` for a named subfolder. The command starts the first stage and registers the task. To save only reusable definitions, use `anydock define <name> --from inbox/<file>.json`. Use a `stages` array with `id`, `work`, and the normal stage contract fields for each entry. Put declared references beside the definition under their declared paths. A regular hand-authored workflow is also valid.

At a managed review gate, use `anydock review <run>` to show the actual file, exact approval token, and comparison to the previous approved version when available. Open the complete file when the comparison is missing or truncated.

Use `batch-review <run>` only for a user-authorized group review. Continue staging independent work sequentially with `next` and `complete`. Present the files and decisions from `review <run>` with its exact batch token. Apply `approve-batch` only when the human approves that entire displayed group.

## Resume from files

For an inspection or harness switch on a named run, start with `anydock handoff <run>` before opening files. In a Windows executable package, run `bin\anydock.exe handoff <run>` from the root. It gives the current attempt, stage, working files, review decision, and next action in one step. It reads the run without changing it. A new harness uses the same folder and instructions. After a meaningful partial step, use `anydock checkpoint <run> --attempt <id> --note <short-note>` to record completed work and the next action. Keep authorization decisions in their proper review record. Stop the old harness before allowing another to edit. If the human supplies a bootstrap with `claim`, record the claim and follow the returned handoff. Issue an `offer` or launch a `switch` only when the human asks to transfer the run. Use the current attempt ID when there is active work. Record `takeover` only on explicit request and use the active attempt ID. Never treat a transfer as approval of work.

- Locate the requested run in `runs/`. Read `brief.md`. A run with managed state, a registration marker, or managed archives stays in managed mode. If that evidence is incomplete, use the CLI diagnosis and restore a complete copy before advancing. For normal resumption, run `anydock status <run>`. For an inspection-only request, start with `anydock status <run> --inspect --diff`. For a packaged checkout on Windows call `bin/anydock.exe` directly. Use the status, preview, and named files to answer. Read only the evidence needed for the request. Report completed work, changes, the first blocker, and the next authorized action. Keep existing state intact if the executable is unavailable and restore access to it before attempting a managed transition.
- For an active attempt, use `anydock next <run>` to recover the attempt ID and continue the work in place. For stale work, inspect the named changes and whether the brief still authorizes the original task, then use `anydock next <run> --retry` when ready to rework it. Inspect existing output before replacing it. Use `anydock diff <run> <stage>` to compare recorded revisions, `history` to see their archive IDs, and `restore` to recover a verified copy under `recovered/`. An approval must match the current revision.
- If execution is already authorized in the brief, continue eligible work unless the resume request limits the turn to inspection. Pause at any human gate. On completion, show the actual handoffs and their verification.

## Agent-only operation

Without the executable, use the same brief, stage contracts, and handoff paths. Tell the user that validation, revision tracking, and gates are being maintained by the agent. Before work begins, create `progress.md` in the run to record the active stage, the brief, contract and input versions (use file digests), and the authorized next action. Update it after each meaningful transition. At completion, verify the input digests, copy the finished handoff into a new revision folder under `revisions/`, and record its paths, output digests, result, and any exact decision. Record human approval with the approved revision. On resumption, compare live files with the recorded digests. Archive a changed handoff as a new revision, reopen its review if required, and mark its declared consumers and their dependent stages stale. Rework stale stages in order. Read this record and the current stage contract in a fresh session. Keep the progress file available for later sessions.

## Blank formats

Use lowercase letters, digits, dots, hyphens, or underscores in run-relative file paths. Run and workflow names and stage labels use letters, digits, hyphens, or underscores and start with a letter or digit. Stage folder names use unique two-digit numbers from 01 to 99 followed by a hyphen and label. Use forward slashes in contracts. Copy files with incompatible names to portable filenames inside the run.

Write `brief.md` with these sections.

```markdown
# Run brief

## Task

## Deliverable and constraints

## Authorization and decisions
```

Each `stages/<number>-<label>/stage.md` uses the following format. Fill in the stage title and Work with instructions for this task. List each required file under `inputs/` or under the output path of an earlier stage. The brief and contract are automatic inputs. Declare at least one output under this stage's `outputs/`. Set `review` to an explicit decision question when human approval is required, or `null` otherwise.

~~~markdown
# Stage title

```anydock
{
  "inputs": [],
  "outputs": [],
  "review": null
}
```

## Work
~~~

Save a useful stage definition only when the user asks. Use `anydock save-workflow <run> <name>` in managed mode. Keep task-specific data in the brief and input files. To reuse a saved definition, prepare a new run folder with its own brief and inputs, then run `anydock start <new-run> --workflow <name>`. Review the copied contracts for relevance to the new task before beginning.

## Reusable guidance

If a stage needs shared guidance, add an optional `references` array to its contract. Each item declares a run-relative `path` under `references/` and may give a `section` matching one distinct Markdown heading. Read just that section and its subsections when declared. The full reference travels with the run. In agent-only mode, include the selected reference material in the progress digests. In managed mode, the CLI tracks the declared section or whole file.

Saving a workflow copies the declared references with its stage definitions. Edit the canonical guidance under `workflows/<name>/references/` for future runs. Existing runs keep their own copies. When a user asks to adopt a change into an existing run, use `anydock adopt-reference <run> <run-relative-path>` and check which stages went stale.

## Alternatives, checks, and delivery

Use `anydock fork <run> <new-run>` for an authorized alternative. Treat the fork as separate work and preserve its review requirements. A request to fork alone leaves execution idle. Compare recorded results with `anydock compare <left> <right>`.

The optional `checks` array in a stage contract accepts objects with `kind` and a declared output `path`. Use `nonempty`, `csv-header` with `columns`, `json-keys` with `keys`, or `markdown-links`. The Markdown check resolves run-local file links. It leaves anchors and remote URLs unchecked. In managed mode, fix failed checks before completion or review. In agent-only mode, describe how you verified each declared check.

For a requested delivery packet, preview selected outputs, disclose any optional metadata, then write the confirmed selection into the run when authorized. Preview and packet creation are local operations. The human can inspect the packet before deciding whether or how to share it.
