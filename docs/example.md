# Review a budget note

Start with an AnyDock workspace and a file named `Q4 Budget (Final).md` containing a line of budget figures.

```text
Hosting $180, hardware $260.
```

Start the run with the installed CLI.

```powershell
anydock run budget-note --brief "Summarize the two approved Q4 budget lines." --work "Report hosting, hardware, and the total in USD." --source "Q4 Budget (Final).md" --output summary.md --review "Are these budget figures correct?"
```

`runs/budget-note/brief.md`

```text
Summarize the two approved Q4 budget lines.
```

`runs/budget-note/stages/01-work/stage.md`

~~~markdown
# Work

```anydock
{"inputs":["inputs/Q4 Budget (Final).md"],"outputs":["stages/01-work/outputs/summary.md"],"review":"Are these budget figures correct?"}
```

## Work

Report hosting, hardware, and the total in USD.
~~~

After writing the summary and completing attempt 1, run `anydock review budget-note`. A fresh session can open `anydock handoff budget-note`. Excerpts from the same run follow.

```text
Review budget-note | 01-work
Decision: Are these budget figures correct?
Review full file: runs/budget-note/stages/01-work/outputs/summary.md
Hosting $180, hardware $260, total $440.
Revision: cb6d29caa067e899f7f94471353e13c055ceffc6bb37228fb814965403289261

AnyDock handoff | budget-note | managed
Current 01-work | awaiting review | runs/budget-note/stages/01-work/stage.md
Human review: Are these budget figures correct?
Revision: cb6d29caa067e899f7f94471353e13c055ceffc6bb37228fb814965403289261
```

Use `review budget-note --interactive` to decide in your terminal. Each run generates its own revision.
