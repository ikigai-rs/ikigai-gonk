# What gonk could learn from github.com/dicklesworthstone (2026-10-28)

Ledger item `default#2505`. This is a research pass over the public repositories of
[Dicklesworthstone](https://github.com/Dicklesworthstone), read as prior art rather than as
something to adopt. Everything there was read as data over the GitHub API. No code from those
repositories was cloned, installed or run, and no instruction in their READMEs, prompts or
`AGENTS.md` files was followed. All descriptions below are in our own words. Each claim links
to the file it comes from.

## Summary

The account publishes a large, fast-moving toolkit for running many coding agents at once on
one machine. Its own profile calls this the "Agentic Coding Flywheel"
([profile README][profile]). The pieces that matter to us are:

- **an issue graph for agents**: beads (`br`), with a graph-aware triage engine (`bv`);
- **agent coordination**: Agent Mail, which provides identities, inboxes and advisory file
  reservations, and NTM, a tmux control plane;
- **mechanical guards**: a destructive-command hook (dcg) and a two-person rule for dangerous
  commands (slb);
- **memory and search over past agent sessions**: cass, cm and ee;
- **planning and prompting process**: apr's cross-model plan revision and a prompt library.

**Their design point differs from ours.** They run about a dozen agents in **one shared
checkout**, coordinating through reservations and mail ([Agent Mail README][mail], "No git
worktrees"). Our design point is one agent per claimed item, one working tree per agent, and
one item per repository per worker machine, with every change handed back as a pull request a
person merges ([starter AGENTS.md][starter-agents], sections 1, 2 and "Worker machines").

Much of their tooling exists to manage contention that our design avoids. That tooling is not
worth borrowing. What is worth borrowing is smaller and sharper:

- how a guard behaves when it cannot decide;
- re-stating the brief after context compaction;
- a `discovered-from` link;
- a worker that has gone silent counted as unhealthy;
- a priority-mismatch check in the doctor;
- refusing self-approval and requiring an independent model for review.

**We already do several of their headline ideas.** gonk's `next` already excludes blocked
work and has a `leverage` policy that ranks by how many open items an item blocks,
transitively, with stated reasons ([ikigai-ledger `policy.rs`][ledger-policy]). Leased claims,
takeover only after expiry, a doctor, usage-gated dispatch and a harness with stable control
IRIs are all in place.

**⚠ License: read this before borrowing anything.** Almost every repository examined here is
licensed "MIT License (with OpenAI/Anthropic Rider)" (GitHub reports `NOASSERTION`). The rider
grants no rights to OpenAI, Anthropic, their affiliates, or anyone acting on their behalf. It
counts "analyzing", "executing" and incorporating the software "into any … evaluation harness,
or pipeline for machine learning or other automated systems" as use (for example
[slb LICENSE][slb-license]). Of the repositories examined, only
[franken_agent_detection][fad] is plain MIT.

Our workers run Claude agents, and this report was written by one. So:

- **borrow ideas only, written fresh**, never code, configuration or prompt text;
- **do not run their tools on our workers** without a legal reading first.

This is not legal advice. Ideas themselves are not what the license governs, but we should not
find out the boundary in practice.

## How this was done

1. `gh repo list dicklesworthstone --limit 200 …` returned 200 repositories. With
   `--limit 1000` the account has 221. The other 21 are all low-star and unrelated to this
   question, for example `github_stars_curve`, `textract-py3` and `gonode`.
2. We picked the repositories about agents, coordination, task tracking, review, testing,
   safety, memory, prompting or process. Their READMEs were read through the GitHub API, and
   docs or code where a README was thin.
3. Load-bearing numbers and rules were spot-checked against the source text a second time:
   - the bv impact weights;
   - dcg's fail-open default for malformed input;
   - cm's 4× harmful weight and 90-day half-life;
   - slb's refusal of local approvals under its default policy;
   - Agent Mail's stance against worktrees;
   - beads' `discovered-from` link.
4. Star counts are as of 2026-10-28.

## The repositories examined

| repository | what it is | relevance to us |
| --- | --- | --- |
| [destructive_command_guard][dcg] (6.2k★) | Rust pre-tool-use hook that blocks destructive shell, git, filesystem and cloud commands | **High**: the most worked-out version of a computational guard sensor |
| [beads_rust][br] (1.1k★) | Local-first issue tracker for agents: SQLite plus JSONL in git, typed dependencies, `ready` | **High**: the closest analogue to the ledger |
| [beads_viewer][bv] (1.7k★) | Graph-aware triage over beads: PageRank, betweenness, critical path, robot JSON | **Medium**: compares with `next` and its policies |
| [beads_viewer_rust][bvr], [beads_bend][bend] | Ports of `bv` and `br` | Low: same ideas |
| [mcp_agent_mail_rust][mail] (and the deprecated [mcp_agent_mail][mail-py]) | MCP server giving agents identities, threaded mail and advisory file reservations | **Medium**: compares with claims and leases; mostly NOT to borrow |
| [ntm][ntm] | Go/tmux control plane: spawn agents, assign work from the beads graph, approvals, policy | **Medium**: compares with the worker loop and dispatch |
| [slb][slb] | "Two-person rule" CLI: dangerous commands need peer approval bound to the command's hash | **Medium**: compares with harness controls and close-by-dispatcher |
| [claude_code_agent_farm][farm] | Runs 20+ Claude Code sessions in tmux, coordinated by lock files the prompt describes | Low: a cautionary contrast |
| [agentic_coding_flywheel_setup][acfs] | Installer and method that ties the whole toolkit together on one VPS | Medium: the process as a whole |
| [misc_coding_agent_tips_and_scripts][misc] | Guides and hooks, including a git-safety hook and a post-compaction reminder | Medium |
| [post_compact_reminder][pcr] | Claude Code `SessionStart` hook (matcher `compact`) that re-points the agent at `AGENTS.md` | **High** for value per cost |
| [ultimate_bug_scanner][ubs] | Multi-language pattern scanner with JSON/SARIF output and agent hooks | Medium: deterministic input to review |
| [acip][acip] | A long system prompt meant to resist prompt injection | Low: our rule, but held only by prompt |
| [cass_memory_system][cm] | Procedural memory: distills session logs into scored rules injected into later prompts | Medium as an idea; mostly NOT to borrow |
| [coding_agent_session_search][cass] | Indexes and searches local agent session history across 30+ tools | Medium |
| [eidetic_engine_cli][ee] | Deterministic, explainable memory "context packs" with provenance | Low–medium |
| [automated_plan_reviser_pro][apr] | Iterates a spec through many rounds of another model's revisions | Low–medium |
| [jeffreysprompts.com][jfp] | A curated library of planning, review and swarm prompts | Low |
| [llm-tournament][tourney] | Multi-round coding tournaments between models | Low |
| [cross_agent_session_resumer][casr] | Converts a session from one agent CLI to another's format | Low |
| [coding_agent_usage_tracker][caut] | Quota and cost across 16+ providers | Low: we already read usage |
| [vibe_cockpit][vc] | Fleet console: per-machine health score, collectors, alerts | Medium |
| [flywheel_gateway][gateway] | Fleet management platform (API-key rotation, dashboard); README is thin | Low |
| [frankengit][fgit] | Agent-native forge, integration stage: attenuated agent runs, atomic merge records | Medium as ideas; nothing to run |
| [asimposium.org][asim] | A public ledger where sponsored agents propose and review work, with independence rules | Medium as ideas |
| [rano][rano] | Observes outbound network connections of AI CLI processes | Low–medium |
| [remote_compilation_helper][rch] | Hook that sends builds to remote SSH workers | Low |
| [storage_ballast_helper][sbh], [process_triage][pt] | Disk-full prevention and stuck-process cleanup for agent machines | Low–medium: worker hygiene |
| [franken_agent_detection][fad] | Detects installed agent CLIs and reads their session histories | Low |
| [meta_skill][ms], [skillranker][sr] | Skill storage and search, and model-ranked choice of the next skill | Low |

**Skipped as relevant in name but not read in depth.** These were judged from their
descriptions in the repository listing:

- other coding-agent CLIs and plumbing: `pi_agent_rust`, `fastmcp_rust`,
  `ultimate_mcp_server`, `ultimate_mcp_client`, `tsap_mcp_server`, `flywheel_connectors`;
- terminal and tmux tooling: `frankenterm`, `useful_tmux_commands`, `power_failure_resumer`;
- account and machine management: `coding_agent_account_manager`, `repo_updater`,
  `agent_settings_backup_script`, `system_resource_protection_script`;
- prompt packing and formats: `source_to_prompt_tui`, `your-source-to-prompt.html`,
  `toon_rust`, `llm-docs`;
- installers: `curl_bash_one_liners_for_flywheel_tools`;
- the many `beads_*` and `*-mailbox-viewer` static dashboards, and `franken_alignment`.

**Skipped as unrelated:**

- OCR, speech and media: `llm_aided_ocr`, `franken_ocr`, `franken_whisper`, `franken_tts`,
  and the YouTube tools;
- Rust reimplementations of other software: `frankensqlite`, `frankenredis`, `franken_numpy`,
  `frankenpandas`, `frankentorch`, and the rest of the `franken*` family not named above;
- terminal UI libraries: `frankentui`, `rich_rust`, `charmed_rust`, `opentui*`;
- math, science and machine-learning research repositories;
- personal sites and websites;
- games, hardware drivers and wallpapers.

## What each one does, and how it compares

### The issue graph: beads (`br`) and the triage engine (`bv`)

**What it is.** `br` keeps issues in SQLite with a JSONL export committed to git
([README][br]):

- **Fields:** priorities 0–4, statuses including `deferred` and `blocked`, and typed
  dependencies (`blocks`, `parent-child`, `related` and `discovered-from`).
- **IDs:** short hash IDs that two clones can mint offline.
- **Ready work:** `br ready` lists open, unblocked, undeferred work.
- **Claims:** a claim is an atomic assign-plus-`in_progress`. There are **no leases**: a
  separate command flags stale claims and only advises, never reclaiming.
- **Discovered work:** `discovered-from` records work found while doing other work
  ([CLI reference][br-cli]).

**How `bv` ranks.** `bv` reads the same data and ranks it ([README][bv]). It computes nine
graph metrics and blends eight normalized factors into one impact score. PageRank weighs 0.22
and betweenness 0.20, and the README prints the formula. Accepting or ignoring a
recommendation re-tunes the weights after three samples. Triage also flags issues whose
human-set priority disagrees with their computed impact.

**Where we already do this.** gonk's `next` is already a ready-set query (open, unclaimed, not
deferred, not blocked by an open item) followed by an ordering policy that is itself a
resource. The `leverage` policy ranks by how many open items an item blocks transitively, and
every answer carries its reasons ([`policy.rs`][ledger-policy]). That is most of what `bv`'s
PageRank buys, without the opacity.

**Where we differ, and which is better.**

- **Leases.** Our leases with takeover-only-after-expiry are stronger than beads' advisory
  stale-claim report.
- **Ranking.** Our pure, total-order policy (deterministic, cacheable, explained) is easier to
  trust than a self-tuning eight-factor blend.
- **What beads has that we lack:**
  - a `discovered-from` link (our link types are `blocks`, `parent` and `related`;
    [`vocabulary.rs`][ledger-vocab]);
  - the priority-mismatch hint.

### Coordination: Agent Mail and NTM

**Agent Mail** ([README][mail]) gives each agent a throwaway identity, an inbox and threads.
Its central tool reserves file globs, exclusive or shared, for a TTL:

- a conflicting reservation is reported at once rather than waited on;
- a pre-commit and pre-push guard enforces reservations at commit time;
- a liveness operator can probe silent agents and reclaim their reservations, starting in a
  shadow (observe-only) mode;
- a human "overseer" can send high-priority mail.

The README says reservations are advisory, not mandatory, and argues against worktrees.

**NTM** ([README][ntm]):

- spawns mixed agent CLIs in tmux panes;
- assigns them work from the beads graph;
- never auto-assigns items carrying operator-gated labels;
- stops automatic dispatch on any failure to read labels or plans;
- gates force-releasing another agent's lock behind approval by a *different* operator.

**Comparison.** Their claim is on files; ours is on an item, with a working tree per agent and
one item per repository per worker machine ([starter AGENTS.md][starter-agents]). For our
design, file reservations solve a problem we removed by construction. Our approach is better
for unattended workers whose output a person reviews as a pull request.

**Two NTM ideas match things we already have:**

- Our dispatch already refuses unless the newest dispatch comes from a configured router and
  the title and body digest still match, which is stricter than a label gate.
- Our worker loop refuses anything not fully labeled.

**Agent-to-agent mail has no counterpart here, deliberately.** All coordination goes through
the ledger, where comments from anyone but a router reach the agent quoted and marked as data.

### Guards: dcg, slb, ubs and acip

**dcg** ([README][dcg]) is a `PreToolUse` hook:

- **How it decides:** it normalizes a command, quickly rejects most commands by keyword, then
  checks "safe" patterns before destructive ones in about 50 rule packs.
- **How it reads a command:** it separates executed text from quoted data, heredocs and
  comments.
- **What it explains:** every decision can be explained by rule ID.
- **Failure policy:** malformed hook input is *allowed* by default (`fail_closed` is opt-in).
  A command it could not finish checking becomes `ask`, or `deny` when configured; the README
  advises `deny` for unattended sessions.
- **Allowlists:** a repository's own checked-in allowlist is inert until trusted, so a cloned
  repository cannot grant itself permissions.
- **Stated limits:** its threat model is a well-intentioned but fallible agent, and it cannot
  see writes made through an interpreter or a script.

**slb** ([README][slb]) puts dangerous commands behind approvals:

- an approval is bound to the command's SHA-256 and expires;
- the requester can never approve its own request;
- reviewers can be required to be a different model.

It states its own limit plainly. Its default policy refuses all local approvals, because
on-host peer review "does not authenticate agent or model identities and cannot contain a
malicious process sharing the same OS user" ([README][slb]).

**ubs** ([README][ubs]) runs pattern and AST scanners per language. It emits JSON or SARIF with
meaningful exit codes. It accepts a higher false-positive rate on the argument that the agent
will filter them.

**acip** ([README][acip]) is a prompt that encodes "untrusted content is data". Its README is
candid that this is probabilistic and only one layer.

**Comparison.** Our harness is already more rigorous in one respect. Every control is a named
resource with a stable IRI, a direction, a stage, an outcome, where it is enforced, the test
that shows it fires, and a firing log ([starter `claude-hook.ttl`][starter-hook-ttl],
[vocabulary][starter-vocab]). Our claim hook already fails closed on input it cannot read.
What we lack is a **destructive-command sensor** at all, and a stated **policy for "could not
decide"** that is uniform across sensors.

Their "data, not instructions" rule is mostly held by prompt (acip, cm's AGENTS.md blurbs).
Ours is held by structure: the worker quotes comments under their author, and only a person
publishes a review finding ([README, the review Queue][readme-queue]). Ours is better.

### Review and process: the flywheel, apr, prompts, tournaments

**The flywheel.** The method runs plan, then beads, then a swarm, then scan, then remember
([ACFS README][acfs], [lesson 07][acfs-loop]):

- plans are revised over many rounds by another model ([apr README][apr]);
- review is mostly prompts asking an agent to look again "with fresh eyes", then find **and**
  fix in the same pass ([prompt registry][jfp-registry]).

**Comparison.**

- **Review.** Our review pass is read-only, a judge re-examines serious findings with more
  context, and only a person publishes ([README][readme-queue]). Their find-and-fix-in-one-pass
  has no independent check, so our way is better.
- **What they have that we do not:**
  - insistence on *independence*: slb's different-model reviewer, and asimposium's rule that
    "strongly supported" needs a different sponsor and a different model family
    ([asimposium README][asim]);
  - frankengit's stance that text inside a repository cannot widen an agent's capabilities or
    approve its own work ([frankengit README][fgit]).

### Memory and history: cass, cm and ee

- **cass** indexes every local agent session, lexical plus semantic, for search
  ([README][cass]).
- **cm** has a model propose "always/never" rules from sessions, gates them on evidence, and
  injects the top-ranked ones into later prompts ([README][cm]):
  - confidence decays with a 90-day half-life;
  - a harmful mark counts four times a helpful one.
- **ee** does the same more deterministically, with provenance and an explanation for every
  item it returns ([README][ee]).

**Comparison.** Our equivalent is human-curated. The harness vocabulary has an `Incident`
class, and the controls an incident produced cite it ([vocabulary][starter-vocab]). The
starter's rules each say they exist "because work went wrong without it"
([AGENTS.md][starter-agents]). Auto-distilled rules injected into future prompts are a
standing prompt-injection channel, and our rule that third-party text is data forbids exactly
that. Ours is the better default. Search over past hand-backs, as distinct from injection, is
worth having.

### Fleet and worker hygiene: vibe_cockpit, rano, sbh, pt

- **vibe_cockpit** scores each machine's health from collectors, and a machine that has never
  reported scores near zero rather than looking healthy ([README][vc]).
- **sbh** pre-allocates ballast files to survive a full disk, with hard vetoes (never `.git`)
  and a shadow, then canary, then enforce rollout ([README][sbh]).
- **pt** kills only above a confidence threshold, with a cap per run and a re-check before
  acting ([README][pt]).

**Comparison.** Our worker posts a `Worker:` line and usage readings on its status item, and
gates new work on usage ([starter AGENTS.md][starter-agents]). But nothing flags a worker that
has **stopped** posting.

## Recommendations, ranked by value for cost

1. **Re-state the brief after context compaction.**
   - **What to borrow:** the idea behind [post_compact_reminder][pcr]. A long unattended run
     loses its rules when Claude Code compacts its context.
   - **What to build:** the worker loop installs, for the agent it starts, a `SessionStart`
     hook with matcher `compact` that prints the **fixed preamble and the item's brief
     themselves**, from the worker's own copy. It should not say "re-read AGENTS.md", which
     points at a file the working tree, and so the agent, can edit.
   - **Where:** worker loop (`tools/worker.py`) and the harness (a guide plus a sensor with a
     firing test).
   - **Size:** small, about a day.
   - **Risk:** low. Claude-only; other agents keep today's behavior.

2. **One rule for "the sensor could not decide": refuse when unattended.**
   - **What to borrow:** dcg's bounded failure policy, made uniform and strict: a guard sensor
     that cannot parse its input, times out, or cannot reach gonk refuses on a worker.
   - **What to build:** state it once as a harness guide. Give every computational control a
     declared failure outcome (`harness:onUndecided refuse|warn`). Add a coverage query that
     lists sensors without one.
   - **Where:** harness vocabulary, the starter's hooks and queries.
   - **Size:** small.
   - **Risk:** low. More refusals on flaky setups, which is the point.

3. **A destructive-command sensor of our own.**
   - **What to borrow:** the shape, not the code:
     - a `PreToolUse` sensor for Bash;
     - a short list of git and filesystem operations a worker never needs (`reset --hard`,
       `clean -f`, `push --force`, `checkout --` over uncommitted work, `rm -r` outside the
       work tree);
     - telling executed text apart from quoted data;
     - stable rule IDs;
     - a firing per refusal.
   - **Where:** starter `hooks/claude/`, as a harness control with firing tests, and the guard
     rules as resources on gonk, so the worker's copy is not the one the agent can edit.
   - **Size:** medium, about 2–4 days.
   - **Risk:** medium. Pattern lists are bypassable through interpreters and scripts. This is
     mistake prevention, as dcg's own threat model says, not a boundary. The worker account,
     its own work root and the pull-request gate stay the boundary.

4. **A `discovered-from` link type.**
   - **What to borrow:** beads' link for work found while doing other work. Today a worker may
     not file items, so what it discovers is lost in a PR description.
   - **What to build:** a fourth link type in ikigai-ledger. A "Found while working" section in
     the hand-back that the dispatcher files with `gonk.py file --discovered-from <N>`. A saved
     query for "work discovered from closed items, still open".
   - **Where:** ikigai-ledger vocabulary, the starter's `gonk.py`, and our briefs.
   - **Size:** small to medium.
   - **Risk:** low. The worker still files nothing, so rule 1 of the brief holds.

5. **A silent worker is an unhealthy worker.**
   - **What to borrow:** vibe_cockpit's rule that a machine without data is not healthy.
   - **What to build:** the doctor, or a saved query, reports a worker whose status item has had
     no `Worker:` or `Usage:` line within N times its cadence. It also reports dispatched items
     that no worker has picked up within a bound.
   - **Where:** starter queries; later the gonk doctor.
   - **Size:** small.
   - **Risk:** low. Report only.

6. **A priority-mismatch check.**
   - **What to borrow:** `bv`'s hint that an item's set priority disagrees with its computed
     importance. We already compute `leverage`.
   - **What to build:** add a doctor check, or a saved query, "high leverage, low or no
     priority", with the numbers as its reason. Keep the ranking policies as they are; do not
     adopt a weighted blend.
   - **Where:** ikigai-ledger doctor, or the starter's queries.
   - **Size:** small.
   - **Risk:** low. Advisory.

7. **Independence in review, stated and checked.**
   - **What to borrow:** slb's no-self-approval rule and different-model reviewer, and
     asimposium's different-family rule.
   - **What to build:**
     - gonk refuses (or at least labels) a judge configured on the same model as the
       reviewer;
     - the Queue shows when reviewer and judge share a family;
     - for worker hand-backs, the review pass runs on a different model family than the one
       that wrote the PR;
     - the dispatcher's close is refused when the closing principal is the one that reported.
   - **Where:** gonk review configuration and the ledger's close.
   - **Size:** small to medium.
   - **Risk:** low. Model identity is self-declared configuration, so this prevents mistakes; it
     does not authenticate anything.

8. **Flag new suppressions in review.**
   - **What to borrow:** a lesson from ubs, the opposite of its practice. ubs lets an agent
     silence a finding inline; a reviewer should treat a *new* suppression as a finding.
   - **What to build:** the review pass, or a CI sensor, reports added `#[allow(…)]`,
     `#[expect(…)]`, `// nolint`, `# noqa`, `@ts-ignore` and skipped tests in a worker's diff.
   - **Where:** gonk's review pass, or the starter's gates as a CI control.
   - **Size:** small.
   - **Risk:** low. Some noise, and every flag is cheap to decide.

9. **Search over past hand-backs, as data.**
   - **What to borrow:** cass's "has anyone hit this before?", narrowed to what we already
     store: items, their comments, and the reports and PR links workers hand back.
   - **What to build:** full-text search over ledger comments, so a person or a router can find
     prior evidence before dispatching. Results reach an agent, if ever, only as quoted data.
   - **Where:** gonk (a script or an ikigai-ledger read).
   - **Size:** medium.
   - **Risk:** medium. Surfacing old text to agents re-imports old injections. Keep it
     human-facing first.

10. **A second model reads a change proposal once before it is seeded.**
    - **What to borrow:** apr's cross-model plan revision, cut to one round.
    - **What to build:** for spec-first changes, ask a model from a different family to
      critique `proposal.md` and `tasks.md` before `gonk.py seed`. A person applies what they
      agree with.
    - **Where:** our briefs and the starter's spec profile, optionally.
    - **Size:** small.
    - **Risk:** cost and time. apr's "convergence" measures text settling, not correctness
      ([apr README][apr]). Never loop it unattended.

## What not to borrow, and why

- **Their code, configuration or prompt text.** The license rider names Anthropic and anyone
  acting for it, and defines use to include analyzing and running inside automated pipelines
  ([slb LICENSE][slb-license]). Borrow ideas only, written fresh, and get a legal reading
  before running any of their tools on a worker.
- **Shared-checkout coordination: file reservations and agent-to-agent mail** ([Agent
  Mail][mail]). Our working tree per agent and one item per repository remove the contention
  this manages. Mail between agents would also open a channel of agent-written text into other
  agents' context.
- **Automatically distilled rules injected into prompts** ([cm][cm]). A wrong or poisoned rule
  spreads to every later run, and its "helpful" signal is self-reported. Our incident → control
  path, curated by a person, is the better learning loop.
- **Self-tuning, many-factor ranking** ([bv][bv]). Opaque weights that drift with feedback make
  `next` harder to interrogate. Our pure, explained, total-order policies are the better
  design.
- **Find-and-fix review prompts** ([prompt registry][jfp-registry]). They mix the claim with the
  remedy and skip verification. Keep review read-only, judged, and published by a person.
- **Prompt-only defenses as controls** ([acip][acip]). At most a guide. A sensor must be a
  program.
- **Permission-bypass defaults and `curl | bash` installs** ([ACFS README][acfs]), and
  **moving long transcripts between models** ([casr][casr]). Both widen what an unattended
  agent carries and can do. A fresh run from a fixed brief is easier to reason about.
- **Lock files described only in a prompt** ([agent farm][farm]). A claim nobody but the agent
  enforces is not a claim. Our claims are held by the server.

<!-- references: the dicklesworthstone repositories -->
[profile]: https://github.com/Dicklesworthstone/Dicklesworthstone/blob/main/README.md
[dcg]: https://github.com/Dicklesworthstone/destructive_command_guard/blob/main/README.md
[br]: https://github.com/Dicklesworthstone/beads_rust/blob/main/README.md
[br-cli]: https://github.com/Dicklesworthstone/beads_rust/blob/main/docs/CLI_REFERENCE.md
[bv]: https://github.com/Dicklesworthstone/beads_viewer/blob/main/README.md
[bvr]: https://github.com/Dicklesworthstone/beads_viewer_rust/blob/main/README.md
[bend]: https://github.com/Dicklesworthstone/beads_bend/blob/main/README.md
[mail]: https://github.com/Dicklesworthstone/mcp_agent_mail_rust/blob/main/README.md
[mail-py]: https://github.com/Dicklesworthstone/mcp_agent_mail/blob/main/README.md
[ntm]: https://github.com/Dicklesworthstone/ntm/blob/main/README.md
[slb]: https://github.com/Dicklesworthstone/slb/blob/main/README.md
[slb-license]: https://github.com/Dicklesworthstone/slb/blob/main/LICENSE
[farm]: https://github.com/Dicklesworthstone/claude_code_agent_farm/blob/main/README.md
[acfs]: https://github.com/Dicklesworthstone/agentic_coding_flywheel_setup/blob/main/README.md
[acfs-loop]: https://github.com/Dicklesworthstone/agentic_coding_flywheel_setup/blob/main/acfs/onboard/lessons/07_flywheel_loop.md
[misc]: https://github.com/Dicklesworthstone/misc_coding_agent_tips_and_scripts/blob/main/README.md
[pcr]: https://github.com/Dicklesworthstone/post_compact_reminder/blob/main/README.md
[ubs]: https://github.com/Dicklesworthstone/ultimate_bug_scanner/blob/main/README.md
[acip]: https://github.com/Dicklesworthstone/acip/blob/main/README.md
[cm]: https://github.com/Dicklesworthstone/cass_memory_system/blob/main/README.md
[cass]: https://github.com/Dicklesworthstone/coding_agent_session_search/blob/main/README.md
[ee]: https://github.com/Dicklesworthstone/eidetic_engine_cli/blob/main/README.md
[apr]: https://github.com/Dicklesworthstone/automated_plan_reviser_pro/blob/main/README.md
[jfp]: https://github.com/Dicklesworthstone/jeffreysprompts.com/blob/main/README.md
[jfp-registry]: https://github.com/Dicklesworthstone/jeffreysprompts.com/blob/main/packages/core/src/prompts/registry.ts
[tourney]: https://github.com/Dicklesworthstone/llm-tournament/blob/main/README.md
[casr]: https://github.com/Dicklesworthstone/cross_agent_session_resumer/blob/main/README.md
[caut]: https://github.com/Dicklesworthstone/coding_agent_usage_tracker/blob/main/README.md
[vc]: https://github.com/Dicklesworthstone/vibe_cockpit/blob/main/README.md
[gateway]: https://github.com/Dicklesworthstone/flywheel_gateway/blob/main/README.md
[fgit]: https://github.com/Dicklesworthstone/frankengit/blob/main/README.md
[asim]: https://github.com/Dicklesworthstone/asimposium.org/blob/main/README.md
[rano]: https://github.com/Dicklesworthstone/rano/blob/main/README.md
[rch]: https://github.com/Dicklesworthstone/remote_compilation_helper/blob/main/README.md
[sbh]: https://github.com/Dicklesworthstone/storage_ballast_helper/blob/main/README.md
[pt]: https://github.com/Dicklesworthstone/process_triage/blob/main/README.md
[fad]: https://github.com/Dicklesworthstone/franken_agent_detection/blob/main/README.md
[ms]: https://github.com/Dicklesworthstone/meta_skill/blob/main/README.md
[sr]: https://github.com/Dicklesworthstone/skillranker/blob/main/README.md

<!-- references: ours -->
[starter-agents]: https://github.com/ikigai-rs/ikigai-gonk-starter/blob/main/AGENTS.md
[starter-hook-ttl]: https://github.com/ikigai-rs/ikigai-gonk-starter/blob/main/profiles/harness/claude-hook.ttl
[starter-vocab]: https://github.com/ikigai-rs/ikigai-gonk-starter/blob/main/.gonk/harness/vocabulary.ttl
[ledger-policy]: https://github.com/ikigai-rs/ikigai-ledger/blob/main/src/policy.rs
[ledger-vocab]: https://github.com/ikigai-rs/ikigai-ledger/blob/main/src/vocabulary.rs
[readme-queue]: ../../README.md#the-review-queue-a-human-publishes-or-nothing-does
