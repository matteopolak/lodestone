# GPL-3.0-or-later Migration Design

## What it is

This migration relicenses all Lodestone-owned source, documentation, generated
metadata, tools, web packages, and first-party plugins under
`GPL-3.0-or-later`. It replaces the repository's MIT/Apache dual license and
the two first-party LGPL plugin declarations with one project-wide policy.

## Scope

The root `LICENSE` contains the unmodified GNU General Public License version 3
text. Cargo packages declare the SPDX expression `GPL-3.0-or-later`, either
directly or through `workspace.package.license`. Project notices, runtime
license messages, templates, tests, and developer documentation use the same
identifier and point to the root license.

The migration does not rewrite third-party rights. Attribution records retain
the actual licenses of referenced projects, dependencies, tools, and data.
Historical Git commits remain available under the terms granted when they were
published; the current tree and future contributions use the new policy.

## Migration rules

- Replace `LICENSE-MIT` and `LICENSE-APACHE` with a canonical root `LICENSE`.
- Set every explicit first-party Cargo license to `GPL-3.0-or-later`; inherited
  workspace packages receive the same value from the root manifest.
- Move `lodestone-nav` and `lodestone-autopilot` from their first-party LGPL
  declarations to the project-wide GPL policy and remove their redundant local
  license file.
- Update project-owned explanatory comments, runtime copy, documentation, and
  manifest fixtures without altering third-party attribution statements.
- Generate `docs/README.md` from `docs/legal-notices.md`; never hand-edit it.
- Add a repository control that finds first-party manifest declarations which
  drift from `GPL-3.0-or-later`.

## Verification

Verification is intentionally focused: run the new license-policy control,
Cargo metadata parsing, docs-index generation/checking, comment-voice, and
repository text scans for retired project-license declarations. The scan must
allow MIT, Apache, LGPL, and other identifiers when they describe third-party
material rather than Lodestone's license.

## Authority and dependency boundary

Relicensing assumes the repository owner has authority to relicense every
Lodestone-owned contribution. The canonical license text comes from the Free
Software Foundation, and the SPDX identifier follows the SPDX License List.
Third-party components remain dependencies or references under their own
licenses and continue to be recorded in `NOTICE` where applicable.
