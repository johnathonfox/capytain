# QSL — starter bundle

This archive contains everything I authored for the QSL project. Drop the
contents at the root of an empty repo and follow the setup guide below.

## What's included

```
DESIGN.md                         Full design specification
PHASE_0.md                        Six-week execution plan
TRAITS.md                         Core Rust interface signatures
COMMANDS.md                       Tauri IPC surface between UI and core
README.md                         Project introduction (public-facing)
CONTRIBUTING.md                   Contributor guide with DCO process
.gitignore                        Baseline Rust/IDE/OS ignores
.github/
  ISSUE_TEMPLATE/
    bug_report.yml                Structured bug report form
    feature_request.yml           Structured feature request form
    config.yml                    Routes questions to Discussions
  PULL_REQUEST_TEMPLATE.md        PR checklist with DCO reminder
  workflows/
    ci.yml                        fmt + clippy + test on macOS/Windows/Linux
```

## What's NOT included (and why)

You need three more files to complete the first commit. I didn't include them
because they should come from their authoritative upstreams rather than
from me:

- `LICENSE`           — fetch verbatim from apache.org/licenses/LICENSE-2.0.txt
                        then fill in the copyright line in the appendix
- `NOTICE`            — write a minimal starting one; regenerate with
                        cargo-about once dependencies exist
- `CODE_OF_CONDUCT.md` — fetch verbatim Contributor Covenant 2.1 from
                        contributor-covenant.org/version/2/1

See DESIGN.md §10.2 for the detailed licensing setup steps.

## First commit flow

```sh
# Extract this archive into an empty directory
cd qsl
curl -o LICENSE https://www.apache.org/licenses/LICENSE-2.0.txt
# (fill in [yyyy] and [name of copyright owner] in the LICENSE appendix)
curl -o CODE_OF_CONDUCT.md https://raw.githubusercontent.com/EthicalSource/contributor_covenant/master/content/version/2/1/code_of_conduct.md
# (edit [INSERT CONTACT METHOD] in CODE_OF_CONDUCT.md)
# (write NOTICE by hand or leave minimal for now)

git init
git branch -M main
git add .
git commit -s -m "chore: initial commit"
git remote add origin git@github.com:johnathonfox/qsl.git
git push -u origin main
```

Then follow the GitHub configuration steps (branch protection, DCO app,
Discussions, private vulnerability reporting) from the setup guide.

## Handing off to Claude Code

Once the repo is pushed and configured, start Claude Code in the repo
directory and give it Phase 0 Week 1 Days 1–2 as the first task. See
PHASE_0.md for the full six-week breakdown.

The CI workflow will fail on the first push because there's no Cargo
workspace yet. That's intentional — Claude Code's first job is to make
it green.
