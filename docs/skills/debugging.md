---
name: debugging
when: A program returns the wrong value, crashes, or behaves differently from how the code reads — especially when the wrong value only exists at runtime (inside a loop, deep in a call stack, after several mutations) or when a print would have to be added, run, read and removed
---

You have a real debugger. Use it before you reach for print statements: it answers
questions about a *running* program, and it leaves nothing behind to clean up.

The rule of thumb: if the answer is "what is this value, here, right now", that is a
breakpoint. If the answer is "which of these branches runs", that is a breakpoint. If you
would have to edit the file to find out, that is a breakpoint.

## First: is the adapter installed?

`debug action=list_adapters` says which debuggers this machine has. If the one you need is
missing, install it — do not fall back to prints without saying why.

| Language | Install |
| --- | --- |
| Python | `python3 -m pip install --user debugpy` |
| Go | `go install github.com/go-delve/delve/cmd/dlv@latest` |
| Node / TypeScript | `npm i -g js-debug` |
| Rust, C, C++ | `lldb-dap` ships with LLVM and the Xcode command-line tools — install those rather than a package |

Two things that will bite you on Python:

- **Install into the interpreter koda actually probed.** A machine can easily have two
  `python3`s and `debugpy` in the wrong one. The error names the interpreter it tried —
  `Tried: debugpy (found at /opt/homebrew/bin/python3)` — so run `pip` through *that*
  path: `/opt/homebrew/bin/python3 -m pip install --user debugpy`.
- If pip refuses with an **externally-managed-environment** error (PEP 668), add
  `--break-system-packages`, or install into the project's virtualenv if it has one.

Re-run `list_adapters` after installing to confirm, then carry on.

## The loop

```
debug action=launch program=<file>          stops at the first line
debug action=set_breakpoint file=<f> line=<n>
debug action=continue                       runs to the breakpoint
debug action=evaluate expression=<expr>     ask the frame a question
debug action=terminate                      when you have the answer
```

`stack_trace` tells you where you are; `scopes` then `variables` lists what is in scope
when you do not yet know what to ask about.

## Getting the answer in few steps

**Evaluate, do not just continue.** The most common way to waste a session is to press
`continue` over and over and never look at anything. Every stop should be followed by a
question: `evaluate` a variable, or `variables` on a scope. If you have continued twice
without evaluating anything, stop and ask something.

**Watching a value across a loop** is what a logpoint is for. A breakpoint with a
`log_message` prints and carries on, so one run shows you every iteration:

```
debug action=set_breakpoint file=sale.py line=14 log_message="row {i}: total is {total}"
debug action=continue
debug action=output
```

That is usually faster and clearer than stopping at the same line ten times.

**Skipping to the interesting iteration** is what `hit_condition` is for — `">50"` stops
only after fifty hits, counted inside the adapter, so you do not stop forty-nine times to
get to the fiftieth. A `condition` (`"total < 0"`) does the same for a value.

**Breaking on a function you cannot find the line for**: `set_function_breakpoint
name=apply_discount`.

## Reporting what you found

Say the value that was wrong and where it came from, not just the fix. "`earned` goes
0 → 75 → 9, so the loop reassigns instead of adding" is a diagnosis a person can check.
"Fixed the accumulator" is not.

Then `terminate` the session. One runs at a time, and a stopped process left behind is a
process nobody can reach.
