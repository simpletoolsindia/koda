---
title: Debugger
description: koda drives a real debug adapter — breakpoints, stepping, variables, logpoints and hit conditions — so the model reads a running program instead of guessing at it.
---

`print` is how you ask a program a question when you cannot ask it directly. koda can ask
it directly.

The `debug` tool drives a real debugger over the
[Debug Adapter Protocol](https://microsoft.github.io/debug-adapter-protocol/) — the same
adapters your editor uses — so the model can stop a program on a line and look at what is
actually there.

## What it can do

```
launch / attach          start a program, or join one already running
set_breakpoint           file + line — with a condition, a hit count, or a
                         log_message, which prints instead of stopping
set_function_breakpoint  break on a function by name, no line needed
continue / step_over / step_in / step_out / pause
stack_trace → scopes → variables      look around where it stopped
evaluate                 ask the program a question in that frame
breakpoints, output, status, terminate
```

One session at a time. `list_adapters` says which debuggers this machine has.

## Two things worth knowing about

**A logpoint is a print statement you never had to add.** Give `set_breakpoint` a
`log_message` and it prints instead of stopping — `"adding {r['amount']} to {total}"` on
the line inside a loop prints the running total every pass and stops nothing. Nothing to
add to the file, and nothing to remember to remove afterwards.

**A hit condition is counted inside the adapter.** `>5` or `%10` on a breakpoint means the
skip happens in the debugger, not in a thousand round trips. Getting to the thousandth
iteration costs one stop rather than a thousand.

## A worked example

A bug that reading the code will not reliably catch — the value is right at every
individual line, and wrong across the loop.

```python
# orders.py — prints 80, should print 164
def total_points(orders):
    earned = 0
    for o in orders:
        earned = points_for(o)      # note the missing +
    return earned
```

Ask for the debugger, and say what you want to watch:

```
orders.py prints 80 but should print 164. Use the debug tool: launch it,
set a breakpoint on the loop in total_points, continue, and evaluate
'earned' each time round so we can see what happens to it.
```

What comes back, verbatim from a run against `debugpy`:

```
Launched orders.py under debugpy. It is stopped (entry) at orders.py:1 in <module>.
Set breakpoints with action=set_breakpoint, then continue.

Breakpoint at orders.py:13. 1 of 1 breakpoints in this file are verified.

continue: it is stopped (breakpoint) at orders.py:13 in total_points.

earned = 0
points_for(ORDERS[0]) = 75
earned = 75
earned = 9
```

`earned` goes **0 → 75 → 9**. It does not accumulate — it is reassigned, so only the last
order survives. One value, watched across three iterations, and the bug names itself.

Two things in that transcript are worth pointing at:

- `points_for(ORDERS[0]) = 75` is `evaluate`. It ran a call that the program had not
  reached yet, inside the stopped frame, against the live values. No edit, no re-run.
- Nothing was printed to find this. The program was never modified, so there is no
  `print` to remember to remove afterwards.

## What a short session looks like

```
Launched buggy.py under debugpy. It is stopped (entry) at buggy.py:1 in <module>.
Breakpoint at buggy.py:5. 1 of 1 breakpoints in this file are verified.
continue: it is stopped (breakpoint) at buggy.py:5 in average.
Stack (innermost first):
- #3 average at buggy.py:5
- #4 main at buggy.py:9
total = 49
```

## The adapters

koda ships the registry, not the debuggers. Each language's community already ships one:

| Language | Adapter | Install |
| --- | --- | --- |
| Python | `debugpy` | `pip install debugpy` |
| Rust, C, C++ | `lldb-dap` | Ships with LLVM / Xcode command-line tools. |
| Go | `dlv` | `go install github.com/go-delve/delve/cmd/dlv@latest` |
| Node, TypeScript | `js-debug-adapter` | `npm i -g js-debug` |

All of them over stdio. That is why `codelldb` is absent despite covering the same
languages as `lldb-dap`: it is a TCP adapter, and an entry that starts fine and then never
answers is worse than no entry at all.

:::caution[Which python has debugpy?]
For Python, koda asks whichever `python3` is first on your PATH. A machine with two of
them — a system one and a Homebrew one, say — can easily have `debugpy` installed in the
other, and then the adapter is genuinely missing however sure you are that you installed
it. koda names the interpreter it probed, so the mismatch is visible:

```
no debug adapter for a `.py` program is installed.
Tried: debugpy (found at /opt/homebrew/bin/python3)
```

`python3 -m pip install debugpy` — with that exact `python3` — is the fix.
:::

## Approval

Reading a stopped program does not stop to ask. `stack_trace`, `scopes`, `variables`,
`evaluate` and `output` count as reads.

Running one does ask, like any other command: `launch`, `attach`, `continue`, the four
step operations, and `terminate` — unless your [autonomy tier](/koda/modes/) is turned up.

## Asking for it

You do not call the tool yourself. Describe the problem the way you would to a colleague,
and be specific about wanting the debugger rather than a print:

```
average() returns the wrong number for the second row —
break on it and tell me what total actually is
```

```
put a logpoint inside the retry loop that prints the attempt
number and the delay, then run it
```

A smaller model may need the steps spelled out, as in the example above, and may keep
pressing `continue` rather than stopping to look. If a session runs away, koda's step
budget ends it and says so; `debug action=terminate` closes it, and the next `launch`
starts a fresh one either way.

## Teaching koda to reach for it

There is a ready-made skill in the repository at
[`docs/skills/debugging.md`](https://github.com/simpletoolsindia/koda/blob/master/docs/skills/debugging.md).
Copy it once and every project gets it:

```sh
mkdir -p ~/.config/koda/skills
curl -fsSL https://raw.githubusercontent.com/simpletoolsindia/koda/master/docs/skills/debugging.md \
  -o ~/.config/koda/skills/debugging.md
```

`koda skills` will list it. A skill is loaded only when it applies, so it costs nothing on
turns that are not about debugging.

It carries the parts that are easy to get wrong: **installing the adapter when it is
missing** (including running `pip` through the interpreter koda actually probed, and the
PEP 668 `--break-system-packages` case), using a `log_message` to watch a value across a
loop instead of stopping ten times, `hit_condition` to skip to the interesting iteration,
and the rule that stops sessions running away — *every stop should be followed by a
question*, so two `continue`s with nothing evaluated in between means stop and look.
