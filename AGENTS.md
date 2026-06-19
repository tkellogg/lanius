# Instructions
- `cargo run -- dev` is running, which restarts on file changes. It keeps `elanus daemon`, web ui BE & FE all running. You generally should be able to assume your file changes are automatically picked up. 
- logs are in target/elanus-dev.log all processes combined
- docs/ — design docs, useful for reasoning about system design & goals. Start at docs/README.md so you only load the files relevant to the task, and keep the indexes updated.
- docs/journeys/ — user profiles and journeys, useful for reasoning about UI changes. Start at docs/journeys/README.md.
- docs/ui-flows/ — browser-flow catalogs and findings for web UI work. Start at docs/ui-flows/README.md, and use the web-qa skill for real UI verification.
- Use .claude/skills/docs-disclosure-indexer/SKILL.md when adding or auditing docs indexes or docs/code cross-references.
- Use skills
