---
title: Tool-call protocols
description: Three ways to ask a model for a tool call, and why koda tries the good one first and falls back without telling you to restart.
---

Local servers vary in how well they implement OpenAI tool calls. Some reject the `tools`
field outright. Some accept it and return malformed calls. Some models have no tool
training at all.

koda has three modes, set with `tool_protocol` in the config or `--protocol` on the command
line.

| Protocol | What it sends | For |
| --- | --- | --- |
| `auto` *(default)* | Advertises native `tools`, and also accepts text blocks. | Everything. It works out which one the server can do. |
| `native` | Native `tool_calls` only. | A server you know implements them correctly. |
| `text` | No `tools` field; the model is told to emit blocks. | Models with no tool support at all. |

## How `auto` works

koda advertises native tools on the first request. If the server rejects the `tools` field,
koda switches to text mode and carries on — same turn, no restart, no error on your
screen.

It also accepts text blocks *while* native tools are advertised, because a model will
sometimes emit one shape when asked for the other. Accepting both costs nothing and turns
a class of malformed-call failures into ordinary tool calls.

## The text protocol

Under `text`, the model is instructed to emit calls as tagged JSON:

```
<tool_call>{"name": "read_file", "arguments": {"path": "src/cart.py"}}</tool_call>
```

The tags are stripped from what you see, even when a tag arrives split across streaming
chunks — a real case, because a 6-byte tag lands in two chunks often enough to matter.

This is what makes koda work with a model that has no tool training. It is not as reliable
as native tool calling: the model has to produce well-formed JSON inside a tag, in prose,
and a small model will sometimes not. But the alternative is a model that cannot act at
all.

## When to change it

Almost never. `auto` handles the cases the setting exists for.

Set `native` if you are debugging and want a server's rejection to surface rather than be
worked around. Set `text` if a server accepts the `tools` field, does not error, and
silently ignores it — that is the one case `auto` cannot detect, because from koda's side
it looks like a model that simply chose not to call anything.

## If tool calls keep failing

Change the model before you change the protocol. Losing the tool format mid-turn is what
models below about 7B do, and no protocol setting fixes it — see
[Model choice](/koda/providers/#model-choice-matters-more-than-anything-else) and
[Troubleshooting](/koda/troubleshooting/).
