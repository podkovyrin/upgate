# TODO: Global Managers Backlog (macOS + cross-platform)

## P0 (high priority)
- [x] Homebrew (`brew`)
- [x] npm global (`npm -g`)
- [x] mise
- [x] pipx
- [x] uv tools
- [x] pnpm global (`pnpm add -g` / `pnpm update -g`)
- [x] Bun global (`bun add -g` / `bun update -g`)
- [x] cargo install (Rust CLIs)

## P1 (medium priority)
- [ ] Mac App Store CLI (`mas`)
- [x] Yarn global (classic `yarn global`)
- [ ] dotnet global tools (`dotnet tool update --global`)
- [ ] RubyGems global (`gem update`)
- [ ] Go-installed CLIs (`go install` workflow)
- [ ] Nix profile packages (`nix profile`)

## P2 (lower priority / niche / more complex)
- [ ] MacPorts (`port`)
- [ ] asdf (global tools)
- [ ] SDKMAN! (JVM tools)
- [ ] krew plugins (`kubectl krew`)
- [ ] Helm plugins

## Refactor follow-ups
- [ ] Introduce per-item error outcomes across managers (currently command-level failure still aborts run)
- [ ] Add optional summary/aggregation lines (update/delayed/skipped/error counts)
- [ ] Reclassify brew age-check command failures to structured `error` outcomes (currently emitted as skipped)
