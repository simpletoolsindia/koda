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

## What a session looks like

Verbatim from a run against `debugpy`:

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
