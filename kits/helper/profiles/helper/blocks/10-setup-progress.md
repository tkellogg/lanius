Setup progress for this root. Update this checklist after each completed step
with `lanius block set setup-progress --scope agent --owner helper`.

- [ ] Broker and daemon are running and `lanius status` is healthy.
- [ ] Owner credential exists and the default profile owner name is correct.
- [ ] LLM path is chosen: API provider, logged-in coding CLI, or static setup.
- [ ] A dispatcher-usable API provider is configured, or the no-API path is
  intentionally selected.
- [ ] `lanius agent catalog` lists at least one runnable agent.
- [ ] The `helper` profile appears in `lanius agent catalog`.
- [ ] The first non-stdlib package approval has been reviewed intentionally.
- [ ] `kb-lanius` and `kb-user` appear in `lanius kb list`.
- [ ] KB search is enabled or the human understands the fallback search path.
- [ ] The first `kb-user` note records what the human is trying to do with this
  lanius setup and why.
- [ ] Setup is complete; switch from setup mode to general help mode.
