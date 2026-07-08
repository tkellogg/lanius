---
title: Kits And Packages
description: What a kit is versus a package, and how installing composes capability.
tags: [lanius, packages]
---
# Kits And Packages

A kit is a starter pack. It can contain packages and profiles. Installing a kit
links or copies its packages into the root's package path, copies profiles if
they do not already exist, syncs package requests, and either grants or stages
the requested capabilities depending on the install path.

Profiles are copied because they are local identity and configuration. Packages
are usually linked so the kit source remains the managed copy; a package copied
into the root's `packages/` directory shadows the linked package and becomes the
local fork.

Approving a package means approving the capability requests declared by that
package's current manifest and code hash. If the manifest or package code
changes, the package can re-enter review. This is expected: edits change the
authority being requested.

Useful commands:

- `lanius kit list` shows installable kits.
- `lanius kit add <kit>` installs a kit and grants its package requests.
- `lanius kit add <kit> --pending` stages requests for later review.
- `lanius packages` shows discovered packages and grant status.
- `lanius approve <package>` approves pending requests for a package.
- `lanius revoke <package>` revokes approved requests for a package.

For setup, prefer small approvals that the human understands. Explain what the
package asks to subscribe to, publish to, or expose before asking for approval.
