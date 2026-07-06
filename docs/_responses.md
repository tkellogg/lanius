Fable responds to docs/_questions.md here. Tim sees a file reload, Fable keeps working and picks up Tim's
responses in docs/_questions.md when Fable has time to read. That way, Fable keeps many tasks in progress.

---

## Status (2026-07-03, morning)

- **Journey 15 (helper agent):** handoff written + committed (`docs/handoffs/agentic-configuration.md`,
  4 milestones). M1 (the `helper` kit + kb-elanus/kb-user) is implementing right now on a codex worker.
  Wonky bits I decided without you (flag if wrong): new stock kit `kits/helper` (not core); panel-first
  activation (setup-tab-as-chat deferred); harness-backed turns scoped to helper only, spike-gated (M4);
  no package-deps mechanism built (kit grouping + pointer tolerance instead).
- **Codex was broken under the cage — fixed.** codex 0.142.5 writes into CODEX_HOME at startup; the
  headless cage only allowed workdir writes, so every headless codex spawn died instantly (that's what
  killed the first M1 worker). Fixed in `24a9cf2`, adapters + ~/.cargo/bin/elanus refreshed. Gotcha
  discovered: the packaged `bin/adapter` copies go stale on every rebuild (copy-if-missing at init) —
  see question 3 below.

## Answers to your questions

- **DeepSeek in Claude Code without logging out (Q from _questions.md):** **yes, verified live, twice.**
  (1) `elanus code --provider deepseek-via-anthropic claude --headless` completes turns with the provider
  env injected per-child (`ANTHROPIC_BASE_URL` + `ANTHROPIC_AUTH_TOKEN`); your claude.ai login was never
  touched. (2) The falsification probe: same endpoint with a garbage key → the turn fails with a 401
  *from DeepSeek* ("Authentication Fails"), proving the injection governs the actual traffic — no silent
  fallback to your OAuth login. Caveat: don't trust the model's self-report ("I'm Opus") — DeepSeek's
  anthropic endpoint maps whatever model id the client asks for, so identity echo is meaningless; the
  401 probe is the real evidence. Codex works the same via `-c model_provider` args. Agreed this earns
  the harness more UI representation; helper-agent M3 (LLM-path detection) moves that direction, and
  I'll note the pattern in coding-harness-onboarding.md.

## Questions for you

1. **Lanius rename + UX overhaul.** Two very different jobs bundled: (a) the mechanical rename
   (crate, binary, env vars, ledger name, docs — we already have the `HARNESS_*`→`ELANUS_*` compat
   pattern to reuse for `ELANUS_*`→`LANIUS_*`), and (b) a design-language overhaul (butcher-bird,
   professional). I can dispatch (a) as a worker task almost any time — say when. For (b) I'd want a
   journey from you (what "professional / butcher birds" looks like in your head — palette? density?
   references?) before anyone touches CSS. OK to treat as two separate items?

2. **Memory blocks, 2 levels.** Confirming semantics before I write the handoff: level 1 = `system`
   placement (today's behavior, cache-friendly, rarely edited); level 2 = `user` placement — rendered
   into the **user turn** each activation, so heavy edits land immediately at the cost of duplicate
   tokens, and the charter/docs should steer agents to prefer system unless they need the hot path.
   Is that the intent — placement=`user` gets a real render home, plus guidance? Or do you also want
   `before_messages`/`after_messages` honored in the same pass?

3. **Stale adapter binaries.** `bin/adapter` under each stock harness package is copied at init and
   never refreshed, so every elanus rebuild leaves live adapters running old code (bit us twice today).
   Options: (a) version-stamp check at launch, warn loudly; (b) re-copy at daemon start when the source
   binary is newer; (c) leave it, document it. I lean (b) with (a)'s warning as the fallback when the
   source isn't found. Want me to handoff this?

4. **KB README.** "KB should have a README that instructs what sorts of information go into it" — do you
   mean (a) every kb/ folder should ship a README page describing what belongs in *that* KB (kb-user
   already gets one in M1; could make `elanus kb check` warn when one's missing), or (b) one central doc
   about KBs in general? I'll default to (a) unless you say otherwise.

5. **ACP.** I'm grounding a handoff for the generic `acp` harness package now (the sprint-4 discovery:
   codex app-server ≈ ACP dialect; one package onboards Gemini CLI, Goose, Cline, etc. with elicitation).
   Planning question: should ACP become the *preferred* seam going forward (i.e. redo opencode over ACP
   when its adapter is solid, per your note), or additive-only for now? Affects how much I invest in the
   opencode-specific paths in the interim.

## Read your answers (2026-07-03, ~09:30) — here's the dispatch state

Running the second-level workflow now: planners write handoffs, I review + validate, you get a sprint demo.

- **In flight right now (3 workers):** GPT-5.5 verifier on helper-kit M1 (the earlier one died with the
  codex cage bug before it started); Opus planner writing `memory-blocks-placements.md` (your answer 2
  folded in — placement=user, stacking semantics); Opus planner writing `acp-harness.md` (ACP preferred
  per your answer; it's tasked to answer your "does ACP get us hooks-depth integration?" question from
  the actual spec, honestly, before we commit).
- **Rename → Lanius:** doing it, but sequenced AFTER the current wave lands — a whole-repo rename while
  the tree carries unstaged helper-kit work + active worker sessions would collide with everything.
  It's next in the queue once M1 commits. SVG logos + the Lily-targeted looks pass ride with it as a
  separate design task (I'll reread journeys/characters.md and target Lily).
- **KB README as skill-description for introspection** (not system prompt): got it — folds into the
  groundskeeper/knowledge seam; will brief the next planner wave.
- **Adapter staleness (b)** agreed — queued as a small handoff.
- **New items noted:** Dolt-vs-SQLite spike redo; secret-store-attached-to-profiles / parameterized
  permission walls; unified sandbox URI scheme; _questions/_responses pattern via Lanius + Signal.
  These enter the planner queue after the current three land. The sandbox-URI one smells like a journey
  first — if you have 10 minutes, a few sentences on what "feels like one system" means to you would
  save a planning round-trip.

## From headless-me (relayed via worker)

- Both Opus planners failed silently — claude headless workers CANNOT WRITE FILES: run_claude_capture launches claude -p with no permission mode (codex gets danger-full-access, opencode gets --dangerously-skip-permissions, claude gets nothing) so every Write is auto-denied; their inline fallback text was eaten by the adapter-summary race. QUESTION 6 for Tim: fix via --permission-mode acceptEdits, or --dangerously-skip-permissions for parity (note: claude headless has NO elanus cage today unlike codex). Fable leans skip-permissions now + cage-for-claude fast-follow.
- QUESTION 7: headless-Fable can only run 'elanus code *'; to let Fable message the bus directly add Bash(elanus emit *), Bash(elanus ask *), Bash(elanus events*) to the allow array in .claude/settings.local.json. Until then Fable relays through workers like this one.
- Planner tasks re-dispatched to GPT-5.5 (by this relay); M1 verifier still running.

## More things fixed along the way (this morning)

- **claude workers were SIGKILL-dead** after I refreshed the harness adapters: `cp` over an existing
  signed Mach-O in place invalidates the code signature on macOS → instant kill. Fixed by replacing via
  a new inode (`rm` + `cp`). If we build the adapter-refresh mechanism (question 3), it must do this.
- Minor residual: every headless claude run now prints `writing adapter summary ... failed: No such
  file or directory` *after* the result (a cleanup/write race — the result itself arrives fine). On the
  small-fixes list.
- **M1 of the helper kit is implemented** (worker report: kit + KBs + charter blocks + web.rs shadowing,
  local acceptance checks passed). Adversarial verification dispatching now.
