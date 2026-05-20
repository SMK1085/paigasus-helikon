# Security Policy

## Supported Versions

| Version             | Supported          |
| ------------------- | ------------------ |
| `0.x` (latest minor)| :white_check_mark: |
| older `0.x`         | :x:                |

Once a `1.x` line ships, this table will track the latest `1.x` line and the most recent `0.x` for one minor cycle.

## Reporting a Vulnerability

Please open a private security advisory at <https://github.com/SMK1085/paigasus-helikon/security/advisories/new>.

Do **not** file a public GitHub issue or post in any public forum until we have had a chance to investigate and ship a fix. Using GitHub Private Security Advisories keeps the report off public search engines while we work the fix, and gives us a full audit trail.

### What to include

- The version of `paigasus-helikon` (or the commit SHA) you were running.
- The version of `rustc` you were running.
- A minimal reproduction (a snippet, a test, or a description of the failing operation).
- Your estimate of the impact.
- A suggested remediation, if any.

### Process and timing

- **Acknowledgement:** within 5 business days of report.
- **Initial status update:** within 14 days of acknowledgement.
- **Coordinated disclosure target:** 90 days from the initial report. Complex issues may extend this window by mutual agreement.
- **On fix:** we request a CVE through GitHub Security Advisories and file a [RustSec](https://rustsec.org/) advisory so downstream consumers pick it up via `cargo audit`.

## Out of scope

The following are not handled through this channel:

- Denial-of-service via malformed prompts to upstream LLM providers — those are the provider's responsibility, not the SDK's. Report directly to the provider.
- Supply-chain advisories already tracked by `cargo audit`. The repository runs `cargo audit` daily via `.github/workflows/audit.yml` and auto-files issues for new advisories. If you want to discuss an already-published advisory, open a regular issue with the `area:security` label.
