# Adding a built-in tool

[extensions.md](extensions.md) covers the three extension points that need no
rebuild — shell-backed `[[tools]]`, skills, and role agents. **Reach for those
first.** This guide is for the remaining case: a tool that has to be Rust,
because it needs koda's own state (the config, the index, a live session) or
has to return a structured `ToolView` the TUI can draw.

Everything below was written by building the example, running it, and reading
the numbers off the result. Follow it top to bottom and you get a working tool
in about twenty minutes.

---

## Before you start: what a tool costs

Every tool's schema is sent on **every request, forever**. The trivial example
in this guide measures at **284 bytes ≈ 71 tokens**:

```
tools=22  total=20705B  word_count=284B
```

That is the price of admission, paid on every turn, whether or not the tool is
ever called. It also enlarges the choice the model has to make — more tools
means more chances to pick the wrong one, which is a real accuracy cost on
small local models.

So the first question is not "can I write this tool" but **"does this need to be
a tool at all?"**

| Instead of a tool | Use |
|---|---|
| Running a fixed command | a `[[tools]]` entry in config ([extensions.md](extensions.md)) |
| Teaching a procedure | a skill (`koda skills --init`) |
| A variation on reading files | an argument on `read_file`, not a new tool |
| Something heavy and rarely used | a tool, but **deferred** (see step 7) |

`word_count` — the example here — is a *bad* candidate by this test: `wc` via
`run_command` already does it. It is used precisely because it is small enough
to show the whole path at once. It is **not** in koda's shipped tool list, and
that is deliberate.

---

## The complete example

A `word_count` tool: takes a path, returns lines, words and characters.

Everything lives in `src/tools.rs` unless stated otherwise.

### Step 1 — Declare the spec

In `build_specs()` (around `src/tools.rs:201`), add a `Spec` to the returned
`vec![]`. Put it next to a tool of similar shape so the list stays readable.

```rust
Spec {
    name: "word_count",
    desc: "Count the lines, words and characters in a UTF-8 text file.",
    params: json!({
        "type": "object",
        "properties": {
            "path": str_prop("File to measure, relative to the workspace root."),
        },
        "required": ["path"],
    }),
    mutating: false,
},
```

Four fields, and each one matters:

- **`name`** — what the model calls. Snake case, and a *verb or noun phrase the
  model will guess*. This is matched literally.
- **`desc`** — the only thing the model sees besides the parameter names. Say
  what it does **and when to reach for it**. If two tools could plausibly
  handle the same request, say here which one wins; that sentence is what
  stops the model dithering.
- **`params`** — JSON Schema. `str_prop(..)` is the helper for a described
  string; integers and booleans are written out as `{ "type": "integer",
  "description": ... }`. Mark only genuinely required arguments as `required` —
  every required argument is another thing a small model can omit and fail on.
- **`mutating`** — `false` means read-only: no approval prompt, and eligible
  for plan mode. **Get this right.** `true` on a read-only tool nags the user;
  `false` on something that writes lets it run unapproved in plan mode.

### Step 2 — Dispatch to it

In `run_sync()` (around `src/tools.rs:1643`), add one arm:

```rust
"word_count" => word_count(args, ctx),
```

The name must match the `Spec` exactly. An unmatched name falls through to
`unknown tool` — a silent-looking failure that is really a typo.

> Blocking or CPU-bound work belongs in `run_sync`, which `run()` already puts
> on `spawn_blocking`. Only add an arm to the async path above it if you need
> to `.await` something.

### Step 3 — Implement it

```rust
fn word_count(args: &Value, ctx: &ToolCtx) -> Result<Outcome> {
    let path = arg_str(args, "path")?;
    // `resolve` rejects paths outside the workspace root when the sandbox is
    // on. Every tool that takes a path must go through it.
    let full = resolve(ctx, &path)?;
    let text = match std::fs::read_to_string(&full) {
        Ok(t) => t,
        // A missing file is an ordinary answer, not a crash: report it as a
        // failed outcome so the model can correct itself and carry on.
        Err(e) => return Ok(Outcome::err(format!("cannot read {path}: {e}"))),
    };
    let lines = text.lines().count();
    let words = text.split_whitespace().count();
    let chars = text.chars().count();
    Ok(Outcome::ok(
        format!("{path}: {lines} lines, {words} words, {chars} characters"),
        format!("{lines} lines, {words} words"),
    ))
}
```

Three rules this small function is demonstrating:

1. **Always `resolve()` a path.** It is what enforces the sandbox. Reading
   `args["path"]` straight into `std::fs` is how a tool escapes the workspace.

2. **`Ok(Outcome::err(..))`, not `Err(..)`, for expected failures.** `Err` is
   for *the tool itself* breaking. A missing file, a bad argument, an empty
   result — those are answers. Returned as `Outcome::err` the model reads the
   reason and adapts; returned as `Err` it gets a generic failure.

3. **`content` is for the model, `summary` is for the human.** `content` goes
   on the wire verbatim; `summary` is the one line in the transcript. Keep
   `content` small — it is charged to the context window on every subsequent
   turn of the conversation, not just this one.

Argument helpers: `arg_str`, `arg_usize`, `arg_bool` (`src/tools.rs:1147`).
`arg_str` errors on a missing key; the other two return `Option`/`false`, so
apply your own default.

### Step 4 — Give it a verb in the status row

In `activity_label()` in **`src/tui.rs`** (around line 3975):

```rust
"word_count" => "counting",
```

Skip this and the status row says "working on word_count". One line, and the
running tool reads as a sentence.

### Step 5 — Test it

In `mod tests` in `src/tools.rs`. Call the function directly — no model, no
network:

```rust
#[test]
fn word_count_measures_a_file_and_reports_a_missing_one() {
    let dir = std::env::temp_dir().join(format!("koda-wc-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("a.txt"), "one two\nthree\n").unwrap();
    let ctx = ToolCtx {
        root: dir.clone(),
        cfg: Arc::new(Config::default()),
        progress: None,
    };

    let ok = word_count(&json!({ "path": "a.txt" }), &ctx).unwrap();
    assert!(ok.ok);
    assert!(ok.content.contains("2 lines"), "{}", ok.content);
    assert!(ok.content.contains("3 words"), "{}", ok.content);

    // A missing file is a failed outcome, not an Err: the model has to be
    // able to read the reason and try something else.
    let missing = word_count(&json!({ "path": "nope.txt" }), &ctx).unwrap();
    assert!(!missing.ok);
    assert!(missing.content.contains("cannot read"), "{}", missing.content);

    std::fs::remove_dir_all(&dir).ok();
}
```

Test the **failure path** as well as the happy one. The failure path is what
the model actually has to recover from, and it is the half that rots silently.

Construct `ToolCtx` directly — that is the whole reason tools take a context
struct rather than reaching for globals.

### Steps 6 and 7 — the two optional ones

**Subagents.** If the tool is read-only and useful to a delegate, add its name
to `SUBAGENT_TOOLS` (`src/tools.rs:977`). Omit it and subagents cannot see it.
Never add a mutating tool here.

**Deferral.** If the tool is heavy or niche, hold its schema back until a
session asks for it. Add it to `DEFERRED` (`src/tools.rs:705`):

```rust
pub const DEFERRED: &[(&str, &[&str])] =
    &[("browser", &["browse"]), ("debugger", &["debug"]), ("stats", &["word_count"])];
```

The model then sees a one-line summary of the group and loads it with
`load_tools` when it needs it. This is how `browse` and `debug` avoid costing
every session their schema. Use it for anything expensive that most turns
never touch.

**If your tool mutates**, also add a `preview()` arm (`src/tools.rs:1515`) so
the approval dialog shows what is about to change — `write_file` renders a
unified diff there. Without it the user is asked to approve a bare tool name.

---

## Validating it

Four gates, in the order that fails fastest:

```bash
cargo test --bin koda word_count      # 1. the unit test
cargo clippy --all-targets            # 2. must be clean
cargo test                            # 3. nothing else broke
```

Step 3 matters more than it looks: several tests assert over the *whole* tool
list, so a new tool can fail a test that never mentions it.

**4. Then prove a model can actually find and call it** — the gate the other
three cannot cover. A tool can compile, pass its tests, and still never be
chosen because the description does not match how anyone would ask:

```bash
mkdir -p /tmp/wc && cd /tmp/wc
printf 'alpha beta gamma\ndelta epsilon\n' > sample.txt
koda -p -y "Use the word_count tool on sample.txt and report the numbers."
```

Which prints:

```
· word_count
sample.txt has 2 lines, 5 words, and 31 characters.
```

The `· word_count` line is the proof — that is the tool being dispatched, not
the model guessing from the filename. Then run it once **without** naming the
tool ("how many words are in sample.txt?"). If the model does not reach for it,
the `desc` is wrong, not the model.

---

## Checklist

| # | Change | File | Required |
|---|---|---|---|
| 1 | `Spec` in `build_specs()` | `src/tools.rs` | yes |
| 2 | arm in `run_sync()` | `src/tools.rs` | yes |
| 3 | the function | `src/tools.rs` | yes |
| 4 | verb in `activity_label()` | `src/tui.rs` | recommended |
| 5 | test in `mod tests` | `src/tools.rs` | yes |
| 6 | `SUBAGENT_TOOLS` | `src/tools.rs` | if read-only + useful to delegates |
| 7 | `DEFERRED` | `src/tools.rs` | if heavy or niche |
| 8 | `preview()` arm | `src/tools.rs` | if mutating |

## Mistakes that cost the most time

- **`Spec.name` and the `run_sync` arm disagree.** Compiles fine, and the tool
  is invisible: the model calls it and gets `unknown tool`.
- **`mutating` backwards.** `true` on a reader means an approval prompt every
  call; `false` on a writer means it runs in plan mode, which is supposed to
  guarantee nothing changes on disk.
- **Not calling `resolve()`.** The tool works, and quietly ignores the sandbox.
- **`Err` where `Outcome::err` belonged.** The model loses the reason and
  cannot recover.
- **A `desc` that describes the implementation.** The model matches on intent.
  "Count lines, words and characters in a text file" beats "wraps wc(1)".
- **Returning everything.** `content` is re-sent on every later turn of the
  conversation. Cap it, and say what you cut.
