# stdlib — the always-on essentials the product depends on

The standard library. These packages are installed and turned on in every
root, and they are protected: removing one takes a deliberate `--force`,
the way a shell refuses to delete the root of the filesystem without a fight
(docs/config.md, "Stdlib: the configuration that is always there").

It exists so the product never depends on something a person has to discover
and turn on. The first member is the transcript view (`history`) — the web
interface reads it for the sessions tab, so it must always be present.
