# AGENTS.md — sonagram working rules

**Authority:** `AGENTS.md` is this repo's agent instructions; there is no
`CLAUDE.md` (Claude Code loads `AGENTS.md` in its place). `.agents/skills/`
holds the skills; `.claude/skills` is a symlink to it so Claude Code finds them.

sonagram scans a music library and structures it into a queryable
[kglite](https://github.com/kkollsga/kglite) knowledge graph, so AI agents can
interact with a music collection directly through the graph. It is a **graph
builder over two upstreams**: `sonara` supplies per-track analysis (typed
`TrackAnalysis` — mood, rhythm, tonality), `kglite` supplies the graph engine
(schema storage, Cypher, MCP exposure). sonagram owns the **mapping and the
schema** between them and nothing else.

Workspace: `sonagram` (pure-Rust core, PyO3-free) + `sonagram-python` (PyO3
bindings) + `python/sonagram/` (wrapper), shipped to PyPI as abi3 wheels plus
an sdist. This file holds the standing rules; the dev-workflow system they wire
into is documented in `dev-docs/README.md` and `inbox/README.md` (both
gitignored, local-only).

> **Status (2026-07-17): v0.1.0 SHIPPED** — PyPI (abi3 wheels + sdist), tag
> v0.1.0, docs on Read the Docs, publish via `release.yml` (trusted publisher;
> ci.yml is tests-only). The gate is real and standing:
> `cargo test -p sonagram --test golden_graph` (TWO goldens now — plain +
> enriched; see `GRAPH-GATE.md`). Layout divergences from the original sketch:
> fixtures at `sonagram/tests/fixtures/{analyses,lastfm}/`, goldens at
> `sonagram/tests/goldens/`; there is also `sonagram/src/cli.rs` (shared CLI),
> `config.rs` (~/.sonagram), `enrich/` (Last.fm), `skill.rs` (bundled agent
> skill). Backlog: `dev-docs/todos.md`.

## Working style
- **Evidence over assertion.** For a bug, reproduce it and confirm the **root
  cause with evidence** (a failing test, a digest diff, a measured number)
  before planning the fix. For a behaviour-preserving refactor, probe the
  *actual* graph output first — don't trust your mental model.
- **No bugs left behind.** A defect you notice mid-task gets fixed (in scope) or
  filed via `add-todo` (out of scope) — never silently stepped over. Mapping and
  schema bugs are **ours**. A bug that traces to the graph engine (`kglite::api`,
  Cypher, storage) or to analysis (`sonara`'s detection) belongs to that
  upstream — reproduce it, then route it via `notify`. Both upstreams are
  read-only from here.
- **Offload, don't print.** Write long output (graph dumps, Cypher results,
  bench tables, big diffs) to `dev-docs/temp/` (>1-day purge) or
  `dev-docs/bench/out/` and **report the path**. Keep responses under ~400
  tokens. Never paste a big generated artifact into the conversation.
- **Determinism is a feature, not an accident.** The same library scanned twice
  must produce a byte-identical graph. Iterate a `BTreeMap`/sorted key, never a
  `HashMap`, anywhere node identity, ordering, or the canonical digest is
  derived. An unstable node ID is a bug even when every test is green.
- **Never commit audio.** Music files are copyright- and size-encumbered. The
  gate's fixtures are **frozen `TrackAnalysis` records** (see below); the real
  music library is a maintainer-machine-only scale check, excluded from CI.
- **The two layers are tested separately.** A change to `#[pymethods]` /
  `#[pyfunction]` in `sonagram-python/src/` needs matching Python API-script
  coverage **and** a Rust-side unit test — a Python-only run misses pure-Rust
  regressions and vice-versa.
- **A comment is a claim (R17).** A change that falsifies a nearby comment
  corrects it in the same change. Remove next-line/signature restatements and
  compress repetition while preserving invariants, safety preconditions,
  format lifecycle, regression rationale and planner/executor bail reasons.
  Future-predicting comments expire; after moving adjacent items, verify
  published docs in their rendered surface.
- **Some comments are tooling inputs (R18).** In sonagram, PyO3 `///` docs
  become runtime Python `__doc__`, clap-derived `///` docs become CLI help,
  public docs feed rustdoc, and a conditional comment can suppress clippy's
  collapsible-if lint. Re-run matching rendered surfaces and clippy after such
  edits. `clean-comments` holds the maintained reader list.

## Review — report what is broken, not what you would have written
*This section is addressed to review agents and overrides any default reviewer
instinct to produce a list of improvements. Adapted from doctrine `R15`
(`../doctrine/rules/RULES.md`, read at `0d128df`).*

A review's output is not a to-do list. It answers one question: **what here is
wrong?** If nothing is wrong, **"no findings" is the correct review** — and a
good one, not a lazy one. A reviewer that always returns an action list is
measuring its own appetite for restructuring, and it trains the reader to skim
past real defects sitting in the same list as preferences.

**A finding names a concrete failure** — the input, state or sequence, and the
wrong outcome it produces. In this repo the failure classes that matter most,
because the whole regression net is built on them, are:
- a **digest change nobody explained** — `golden_graph` moved and the diff has
  no stated cause;
- a **non-deterministic node ID** — a `HashMap` iteration, a timestamp, a path
  ordering leaking into identity, ordering, or the canonical digest (a bug even
  when every test is green);
- a **golden regenerated to reach green** — the one move that makes the gate
  worthless;
- plus the general set: a wrong result, a crash, data loss, a broken contract
  with a caller or a persisted `.kgl`, a *measured* perf regression, a gate that
  cannot fail, or a claim the code contradicts.

**Structure, naming, "consider using X", "could be simplified", "this won't
scale" are not findings** — at any confidence. They are not worthless, they are
**mis-staged**: their venue is **planning**, where `phased-plan` Phase 1
explicitly invites them. Raise them there, or hold them for the next plan.

**A finding that cannot state its failure case is removed, not downgraded.**
"Minor" and "nit" are not a lower tier — they are the laundering mechanism a
preference uses to ship unchanged. Severity is not a workaround.

**One narrow exception:** citing a rule this repo declared *before* the diff
existed — determinism, never-commit-audio, the goldens-regenerated-consciously
  rule, the two-layers-tested-separately rule, the version-source inheritance rule, upstream
discipline — is enforcement, not taste, **provided you name both the rule and
the violating line**. The three sonagram failure classes above are all declared
rules, so they land in this exception rather than the taste bucket.

## The graph gate (sonagram's regression net)
sonagram's output is a graph, so the net is **digest fidelity + determinism**,
not audio fidelity. The gate is `cargo test -p sonagram --test golden_graph`;
`GRAPH-GATE.md` is the committed record. Three parts:
- **`golden_graph`** — build the graph from the frozen `TrackAnalysis` fixtures
  (`tests/fixtures/analyses/*.json`) and assert its canonical digest matches the
  committed golden. Fixtures are captured sonara output, so the gate is
  deterministic, needs no audio, and doesn't re-run analysis.
- **`determinism`** — the same fixture set built twice yields an identical
  digest, and node IDs stay stable across input reordering. Catches
  iteration-order and identity leaks that goldens alone miss.
- **`contract`** — compiles and asserts against the **real** `sonara` +
  `kglite` APIs, so an upstream bump that breaks the mapping fails loudly here
  instead of drifting silently. See `dev-docs/designs/upstream-contracts.md`.

**A red `golden_graph` after a mapping/schema change is a conscious decision.**
If the graph change is intended, regenerate the goldens
(`cargo test -p sonagram --test golden_graph -- --ignored capture_goldens`) **in
the same commit** and say why in the commit body. Never regenerate to silence a
diff you can't explain — that's the one move that makes the whole gate
worthless.

## Scale protocol (perf)
A music library is the scale problem: 10k+ tracks, and **incremental rescan** is
the operation users feel. Perf claims come from a **release build only**, `min`
over `median` for sub-ms work. Track scan throughput (tracks/sec), full-build
wall time, and incremental-rescan cost of an unchanged library (which should
approach zero — if a no-op rescan rebuilds, that's a bug, not a slow path).
Ad-hoc runs → `dev-docs/bench/results/results.csv`; heavy artifacts →
`dev-docs/bench/out/`. The large local music library is the maintainer-machine
scale check, never CI.

`min` is the right statistic only for a **steady-state repeated** op. A
once-per-event cost (a cold full build, the first scan of a library) has no
steady state and its min is a warm cache no user sees — report the **mean of
first events**; judge a heavy-tailed cell whose min sits far below its own
median by median/mean instead. Every comparison carries an **unchanged-path
control cell**: if the control moved too, you measured the machine, not the
code — no control, no verdict. And measure the headline number **two
independent ways** (wall clock vs the harness's timer; counts vs bytes on
disk) — the routes diverge exactly when the instrument is wrong, which one
route reports as a clean result.

**A control cell needs margin over its own resolution, and immunity from what it
anchors** (doctrine `R11` corollary). Two shapes both leave the gate looking
green while measuring nothing. **Too small to resolve:** require the control's
median at **≥2×** the capture's noise floor, not merely above it — a control one
tick above the floor is decoration. **Not immune to what moves:** a control is
chosen because *this* work cannot touch it, and that premise silently expires
when the thing that moves is a **dependency** — a `kglite` bump is exactly this
case for us, since the graph build we would anchor on runs inside their engine.
Prefer the slowest op *measured* stable across the last dependency move; an op
the upstream's release notes describe as improved is disqualified on sight. And
when a control moves **deterministically** — the same delta across repeated
re-measures — "the instrument wandered, re-measure" is the wrong reading and
repetition cannot resolve it: re-measuring returns the same number forever. A
deterministic control move means the control's premise is void, which is a
finding about the gate, not about the machine.

## Build / test / ship (the canonical commands)
- Before a release binding build, run `make prune-target` then
  `make check-free-space`. The prune verifies the
  repo's cargo target symlink and prunes the owned cache below 40 GiB free or
  above 40 GiB measured size. The guard refuses builds below 15 GiB free.
- Rust core: `cargo test -p sonagram`. Lint:
  `cargo clippy -p sonagram --all-targets -- -D warnings`.
- Graph gate: `cargo test -p sonagram --test golden_graph`.
- Bindings: `maturin develop --release -m sonagram-python/Cargo.toml` (in a venv).
- Python API tests run as **scripts**, not pytest:
  `.venv/bin/python tests/python/test_<name>.py`. **Always the venv
  interpreter** — bare `python` is a shell alias on this machine, so it reports
  the alias's failure as the checker's verdict. Likewise, never verify a step
  with `cmd >/dev/null 2>&1; echo $?`, which masks *which* command failed; read
  the target command's own status.
- Version lives in **two declarations**: root `Cargo.toml`
  `[workspace.package].version` (inherited by both Rust crates) and
  `pyproject.toml` `[project].version`; `Cargo.lock` records both resolved crate
  versions. Three tests pin the current value and move with a release:
  `sonagram/src/lib.rs`, `tests/python/test_import.py`, and
  `tests/python/test_cli.py`. Publish gates on `pyproject.toml`. **Never bump the version, edit
  `CHANGELOG.md`, or push `main` outside the `release` skill.**
- **No push without approval** — local commits only; the sole carve-out is the
  `release` skill's own `main` push (invoking `/release` is that authorization).

## Code analysis
- Prefer the code-graph MCP (`graph_overview` → `cypher_query`) for structural
  questions (where a symbol is defined, call graphs, what implements a trait) —
  `set_root_dir` to this repo first. Point it at `../sonara` or `../KGLite` to
  answer an upstream question. Use `grep`/`read_source` only for literal text
  search, not to rediscover structure the graph already encodes.
- Fan out **read-only `Explore` agents** for broad multi-file investigation;
  keep the conclusion, not the file dumps.

## Skill mandates
The dev-workflow is operated by the skills under `.agents/skills/` — use them;
don't reinvent their steps inline:
- **Large feature / non-trivial refactor → `phased-plan`.** Demand it for such
  work. It investigates read-only first (no plan mode), builds a gated phased
  plan, opens a branch + draft PR, and runs the code→test→commit→push loop.
- **Capture a task → `add-todo`.** The single authority on todo-entry shape:
  lean one-line backlink in `dev-docs/todos.md`, detail in a `plans/` doc.
- **Tidy the working folder → `dev-docs-cleanup`.** Auto-purges the time-boxed
  dirs, reconciles `todos.md`. Run before a new `phased-plan`.
- **Incoming mail → `read-inbox`; outgoing → `notify`.** See "Inbox hygiene".
- **Ship → `release`.** Goal-check, run the gate, bump both version declarations
  + changelog, push (triggers PyPI), verify, tidy. Never ship by hand.
- **Comment cleanup → `clean-comments`.** Measure first, preserve R17/R18
  contracts, and verify actual diffs plus rendered readers.

## Inbox hygiene
`inbox/` is the cross-project channel (gitignored; layout in `inbox/README.md`).
sonagram sits **downstream of two active siblings**, so this channel carries
real load — it's how a `sonara` schema need or a `kglite` engine bug reaches the
party who can act.
- `inbox/unread/` holds **only what still needs action** — a stale note must not
  hide a genuinely open one. Triage via `read-inbox`; it lifts durable info into
  `dev-docs/` + a `todos.md` backlink, then moves the note to `read/`.
- **Route to the party who can act.** A note belongs in another project's inbox
  only if it carries an actionable task *for them* (`notify`). Our two standing
  targets: **sonara** (analysis output shape — missing time/provenance fields,
  detection bugs) and **kglite** (engine, Cypher, storage, MCP). If there's
  nothing for them to do, don't file it.
- Send + receive share one filename schema
  (`YYYY-MM-DD-from-<sender>-<topic>.md`), so coordination needs no per-pair
  wiring. `mcp-servers/` is **one** project with one inbox — never target a
  subdir.

## Upstream discipline
sonagram consumes `sonara` and `kglite` as cargo libraries; both are read-only
here. The boundary — what we consume, what we must never depend on, and what we
owe each upstream in notification — is
**`dev-docs/designs/upstream-contracts.md`**. Two standing rules:
- **Stay a mapper.** Analysis belongs in sonara; storage, Cypher, and embeddings
  belong in kglite. If a fix wants to live upstream, route it there rather than
  growing a shadow implementation here.
- **Tell them before you need them to change.** An upstream API need discovered
  mid-plan goes out via `notify` when you find it, not at release.
