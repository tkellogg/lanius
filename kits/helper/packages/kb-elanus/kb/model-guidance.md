# Model Guidance

Agent work needs enough instruction following, tool discipline, and context
handling to survive multi-step setup. A cheap model can be useful, but the helper
should flag likely-underpowered choices before the human depends on them.

Soft warning heuristic: a model name is likely underpowered for agent work when
it looks like a small parameter tier or a reduced product tier. Examples include
names containing `mini`, `nano`, `lite`, `tiny`, or `flash-lite`, and explicit
sizes around 14B parameters or below such as `4b`, `7b`, `8b`, `12b`, or `14b`.

This warning is not a block. It means setup may degrade in predictable ways:
missed instructions, poorer tool selection, weaker error recovery, shallow
summaries, and more need for human correction. For ambient setup help, pick a
stronger default when possible and use small models for narrower scripted work.

Useful stance: "This model may struggle with agent work. It can still be tried,
but if setup feels erratic, switch the helper to a stronger model before
debugging the rest of the system."
