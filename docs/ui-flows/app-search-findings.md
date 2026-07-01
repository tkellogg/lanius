# App search findings (spec + working-with-tim lens)

## Summary

A methodical walk through the web dashboard's front-door and configure flows
turned up seven real problems that an actual person would hit. They fall into
two buckets.

The larger and more common bucket is plain-language failures: the interface
is meant to be a product with its own clear vocabulary, but internal plumbing
words have leaked into the surfaces a newcomer sees first. The welcome screen,
the always-visible top-corner light, the always-visible footer, the configure
pane, and the entire kits/review destination all show words like "stage",
"grant", "pending", "ledger", "algedonic", and "MQTT", plus a raw broker URL
and a raw mailbox string. These are exactly the words the design says must
never appear in the product, and they greet a person before they have done
anything.

The smaller but more dangerous bucket is one data-loss bug. The "raw
profile.toml" editor lets a person save a broken file, tells them "saved" as
if it worked, and then the agent silently disappears from the list — and one
broken file takes down the listing for every agent, with no error shown and no
way back through the interface to fix it. The structured form right next to it
validates correctly and refuses bad input; only the raw editor skips the check.

Everything else in both flows behaved correctly and is listed at the end so it
is clear what was exercised and found sound.

## Problems by severity

### High

#### 1. Saving a broken raw profile file says "saved", then the agent silently vanishes with no way back

**What a person hits.** I open the "raw profile.toml" editor, make an edit that
happens to be malformed, and click save. The note says "saved" — so I believe
it worked. After a reload my agent has disappeared from the left-hand list
entirely, there is no error anywhere on the page explaining why, and there is
no way through the interface to get back to that agent to fix what I broke. I
was told my change succeeded, then my agent silently evaporated and I am stuck.
Worse, one bad file takes down the listing for all my agents, not just the one
I edited.

**What it violates.** docs/config.md ("the result is validated before it
lands") and the server's own stated contract that the kernel validates the
edit before writing it. Working-with-tim lens: an action gives a false
confirmation that it worked, and the interface reaches a dead state (agent
gone, list erroring) with no way for the person to recover or even reach it.

**Suggested fix.** Validate the raw file in the save handler before writing —
run it through the same parse-and-validate path the structured save already
uses, and return the parse error so the note shows the real reason instead of
"saved". Separately, make the agent list resilient to a single unreadable file:
skip and report the broken one instead of failing the whole list, and show a
visible banner naming the broken file so the person can still navigate to it
and repair it.

**Confidence.** Certain. The raw save path wrote `garbage = = [[[ not valid`
verbatim to disk and returned success with the note reading "saved"; the agent
list then failed entirely and the agent was gone from the nav with no error
shown. The structured form, by contrast, correctly refused the same kind of bad
value. The save handler writes the file with no parse check (line 320 of the
Node relay at the time of this finding; the relay is now src/web.rs).

#### 2. The first sentence and a main button on the welcome screen use the internal word "stage"

**What a person hits.** The very first line a newcomer reads is "talk to your
agents, watch the bus, stage capabilities." and one of the three big buttons is
"stage a kit". "Stage" is an internal mechanism word — a person has no idea
what it means to "stage" something. An app store says "Get" or "Install", not
"stage". This is the orientation screen, the one place that is supposed to be
in plain language.

**What it violates.** docs/layering.md ("the internal words must not appear in
the interface"; "a person adding something should see 'Add' or 'Install', not a
two-step 'stage, then approve'") and the working-with-tim lens, which names
"stage" as a banned word.

**Suggested fix.** Reword the lead in plain language, e.g. "talk to your agents,
watch what's happening, add capabilities." Rename the button to "add a kit" or
"browse kits". Drop "stage" everywhere in the product surface.

**Confidence.** Certain. Confirmed in source: welcome lead and the kits button
(index.html lines 60 and 64).

#### 3. The kits/review destination — one click from the front door — shows the forbidden "stage, then approve" two-step

**What a person hits.** Clicking "stage a kit" on welcome, or "kits & review" in
the nav, lands on a view whose subtitle reads "kits & grants — stage, then
approve". The body then says "staging lands every grant as pending; commit
below" and "requests are not grants until you commit them". A person is hit with
grant, stage, pending, and commit all at once — exactly the intimidating
two-step approval queue the design says should never appear in the product. They
cannot tell what they are supposed to do.

**What it violates.** docs/layering.md ("one action to add something"; the old
"first stage, then approve" no longer belongs in the interface) and docs/config.md
("What the interface shows" — "never the words 'stage', 'grant', or 'pending'";
"not a separate, intimidating queue"). Lens banned words: stage, grant, pending.

**Suggested fix.** Per docs/config.md, collapse to a single Add/Install action
with an app-store-style permission preview, and rewrite the copy without stage,
grant, pending, commit, or ledger. That is the larger config redesign, but at
minimum these user-facing strings must be translated at the boundary now.

**Confidence.** Certain. Confirmed in source: the setup view subtitle and the
two "dim-note" blocks plus the "pending review" heading (index.html lines 148,
152–153; app.js sets the subtitle).

#### 4. "algedonic" leaks into the always-visible warning light and the signals subtitle

**What a person hits.** Hovering the light in the top-right corner — visible on
every screen, including welcome — shows the tooltip "algedonic channel — click
to acknowledge". Opening signals (a button on the welcome screen) shows the
subtitle "the global rail — orange is algedonic, nothing else is". "Algedonic"
is a specialist term no ordinary person knows; it tells them nothing about what
the orange light means.

**What it violates.** docs/layering.md (internal vocabulary must not appear in
the interface) and the working-with-tim lens, which names "algedonic"
explicitly as a banned word.

**Suggested fix.** Replace with plain language describing what the light means,
e.g. tooltip "urgent alerts — click to acknowledge" and subtitle "orange means
something needs your attention; everything else is routine."

**Confidence.** Certain. Confirmed in source: the light's tooltip
(index.html line 22) and the signals subtitle (set in app.js).

### Medium

#### 5. The configure pane shows a raw mailbox string and the word "ledger"

**What a person hits.** On the configure screen, right under the save button, a
person reads: "renaming changes the mailbox to in/agent/<name> going forward;
history under the old name stays in the ledger." A non-specialist has no idea
what "in/agent/<name>" is (it looks like a file path or code) or what a "ledger"
is — these are the system's internal plumbing. It makes a simple "rename your
agent" action feel like it requires understanding the system's guts.

**What it violates.** docs/layering.md ("a word that only makes sense once you
understand how elanus works inside does not belong in the product"). Both
"ledger" and the raw "in/agent/<name>" string leak. The lens names "ledger"
and "topic" as banned.

**Suggested fix.** Reword in product language, e.g. "Renaming takes effect on
the agent's next run. Messages and history recorded under the old name are
kept." Drop "in/agent/<name>" and "ledger" entirely — the person does not need
the address string or the storage mechanism to understand what a rename does.

**Confidence.** Certain. Confirmed in source: this static text is always visible
in the configure pane for every agent (index.html line 108).

#### 6. The always-visible footer shows "MQTT" and the raw broker address

**What a person hits.** The footer strip, shown on every view including the
welcome front door, reads "everything you see arrived over plain MQTT" and
displays "mqtt://127.0.0.1:23100". MQTT is a transport-protocol name and the URL
is wiring detail; a person using the product to talk to agents has no reason to
see either.

**What it violates.** docs/layering.md (internal vocabulary must not appear in
the interface). The lens names "mqtt" as banned.

**Suggested fix.** Drop the "plain MQTT" line and the raw broker address from
the footer, or move them behind a builder/diagnostics affordance. The existing
"connected" indicator already covers live-connection status in plain language.

**Confidence.** Certain. Confirmed in source: footer strip
(index.html lines 177 and 179).

#### 7. A duplicate or invalid agent name shows raw error text using the word "profile"

**What a person hits.** If a person creates a second agent with a name they
already used, the note under the Create button reads literally: `error: profile
"kestrel" already exists` — with the raw "error:" prefix and a trailing
newline. The product calls these things "agents", so being told a "profile"
already exists is confusing. Trying a natural name like "my agent" (with a
space) shows the bare phrase "bad profile name" — again "profile", and with no
hint that the real rule is "letters, numbers, dashes, no spaces". The person is
left guessing what they did wrong.

**What it violates.** docs/layering.md (the interface must use its own clear
language, not internal words like "profile"; here raw error text and jargon both
leak). The create note is the only feedback for a rejected attempt, so it must
be plain and actionable.

**Suggested fix.** Translate at the boundary. For the duplicate case: "an agent
named 'kestrel' already exists — pick another name." For the invalid case:
"names can use letters, numbers, dashes and underscores (no spaces)." Strip the
raw "error:" prefix and trailing newline.

**Confidence.** Certain. Confirmed in source and live run: the duplicate path
surfaces the command's raw error verbatim, and the spaces case returns the bare
"bad profile name" string (line 309 of the Node relay at the time of this
finding; the relay is now src/web.rs).

## Internal-vocabulary leaks

Every distinct internal word found leaking into a user-facing surface. This is
the docs/layering.md hard rule — these must each be translated at the boundary.

- **stage** — welcome lead, "stage a kit" button, kits/review subtitle and body
- **grant** / **grants** — kits/review subtitle and body
- **pending** — kits/review body and the "pending review" heading
- **commit** — kits/review body ("commit below", "commit them")
- **ledger** — configure pane rename note; kits/review body ("ledger trail")
- **decided_by** (decided_by=ui) — kits/review body
- **algedonic** — top-corner warning light tooltip; signals subtitle
- **MQTT / mqtt** — footer line and the raw broker URL `mqtt://127.0.0.1:23100`
- **in/agent/<name>** (raw mailbox address) — configure pane rename note
- **profile** — raw error text on duplicate-name and invalid-name create

## What was checked and found sound

These flows were exercised in a live isolated stack and behaved correctly; they
are not problems.

- Welcome is a true front door and routes to converse, configure, kits, and
  signals — never a dead end. The "home" masthead returns to welcome.
- Creating an agent lands on the new agent's configure tab with a durable note
  ("created kestrel — set its identity below, then converse"), and the agent
  appears immediately in the left-hand list.
- A blank name is rejected on the spot with a "name it first" note and no
  request fired.
- Nav items route correctly and update the view title.
- The structured configure form validates: a bad value is refused with a clear
  message ("refusing to write: the result would not load as a profile…") rather
  than silently corrupting the file. (Only the raw editor skips this — see
  problem 1.)
- History was serving normally, so the degraded-history hint correctly did not
  appear.
- The stack came up and tore down cleanly with no unexpected page or console
  errors (the only console errors were the expected rejections from the
  deliberate bad-name create attempts, which are the intended feedback).
