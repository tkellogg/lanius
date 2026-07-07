# Setup Checklist

Use this longer checklist behind the helper's `setup-progress` block.

1. Confirm the daemon and broker are running.
2. Confirm the owner credential exists and the default profile owner is correct.
3. Identify the LLM world: API provider, logged-in coding CLI, or neither.
4. If using API billing, create a provider and test it.
5. If using a coding CLI login, confirm the CLI exists and is logged in.
6. Confirm `lanius agent catalog` lists the available profiles and coding tools.
7. Install or approve the helper kit so the helper profile and KB packages exist.
8. Confirm exactly one `helper` profile appears in UI or catalog output.
9. Confirm `kb-lanius` and `kb-user` appear in `lanius kb list`.
10. Enable KB search when available, or document the fallback raw search path.
11. Review the first non-stdlib package approval with the human.
12. Create the first `kb-user` page explaining the human's purpose and setup
    preferences.
13. Mark setup complete and switch to general help.

Keep the progress block terse. Use this page when the human asks why an item is
needed or what remains.
