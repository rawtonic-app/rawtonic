# ADR 0018: Retain rights in stripped exports

- Status: Accepted (maintainer clarification, 2026-10-24, CX8 task)

## Decision

Every recipe that strips metadata retains creator, copyright notice and
rights/usage terms by default. This includes Fast (Web) and existing user
recipes. Existing recipe files must not be silently migrated or rewritten.

Creator contact details are **opt-in**: email, telephone, website and full
postal address (street, city, region, postal code, country). A web deliverable
must not gain those fields unless the user explicitly chooses to include them.
This clarification supersedes the original, broader CX8 request to retain
contacts alongside copyright in every stripping recipe.

Export discloses the copyright-only default. Its Metadata section offers
Include creator contact details, saved in recipes; both CLIs accept
`--keep-creator-contact`. An absent option in an old recipe means contacts are
omitted. Creator, copyright and usage terms remain regardless of this option.
PRIVACY and CHANGELOG must describe the default and its effect on existing
recipes. GPS, camera identifiers, serials and other descriptive metadata remain
stripped. Mandatory generated-content marking remains present.

## Constraints

- No original files are changed.
- Metadata changes do not change exported image samples or encoded image data.
- LUT identifiers and pixel rendering are independent of this decision.
