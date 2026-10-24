# ADR 0018: Retain rights in stripped exports

- Status: Accepted (maintainer decision, 2026-10-24, CX8 task)

## Decision

The maintainer explicitly requested creator/copyright, plus contact details and
usage terms when present, in every recipe that strips metadata. This includes
Fast (Web) and existing user recipes. This is a change to export defaults;
existing recipe files must not be silently migrated or rewritten.

The requested contact retention includes the photographer's email, telephone,
website and full postal address (street, city, region, postal code, country).
**Accepting this default means existing strip recipes can publish those personal
contact details where they previously published none.** Copyright and licensing
contacts remain with client deliverables by default. This is the maintainer's
explicit product choice, rather than a claim that contact information is safe
for every publication or that stripping means anonymous output.

Export discloses this default before export. Its Metadata section offers an
explicit Remove creator contact details option, saved in recipes; both CLIs
accept `--strip-creator-contact`. That option removes contacts when other
metadata is stripped, while retaining creator, copyright and usage terms.
PRIVACY and CHANGELOG must describe the default and its effect on existing
recipes. GPS, camera identifiers, serials and other descriptive metadata remain
stripped. Mandatory generated-content marking remains present.

## Constraints

- No original files are changed.
- Metadata changes do not change exported image samples or encoded image data.
- LUT identifiers and pixel rendering are independent of this decision.
