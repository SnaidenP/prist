# Prist

A Flutter version manager written **100% in Rust**, distributed as a static binary with zero runtime dependencies. Install and switch Flutter versions faster than Puro, using less disk, without the symlink instabilities Puro suffers from.

## Differentiators vs. Puro / FVM

- **Git object deduplication via `git alternates`** — no deep symlinks inside the SDK.
- **Directory Junctions on Windows** instead of symlinks — no developer mode or admin required.
- **Async parallel downloads** with integrity verification.
- **Atomic state-file writes** — never leaves the environment half-corrupted mid-operation.

## Install

```sh
cargo install prist
# or: curl -fsSL https://prist.dev/install.sh | sh
# or: powershell -c "irm https://prist.dev/install.ps1 | iex"
```

## Usage

```sh
prist create music_app 3.0.1     # create env "music_app" pointing at Flutter 3.0.1
prist create beta_test beta      # create env "beta_test" on the beta channel
prist use music_app              # activate "music_app" in the current project
prist ls                         # list installed environments
prist releases                   # paginated remote release feed
prist flutter run                # transparent proxy to the active env's flutter
prist doctor                     # verify bare repo + alternates integrity
prist rm music_app               # remove a local env (shared bare repo untouched)
prist clean                      # remove Prist config from the current project
prist update                     # self-update the binary
```

## Layout on disk

```
~/.prist/                       (Unix) | %LOCALAPPDATA%\prist\ (Windows)
├── shared/
│   ├── git_bare.git/           # central bare repo, alternates source for all envs
│   └── engines/
│       └── <engine_hash>/      # engine artifacts, indexed by commit hash
├── envs/
│   ├── <env_name>/             # deduplicated clone (uses alternates toward git_bare.git)
│   └── default/                # junction/symlink to the env marked as global
└── config.json                # global Prist config
```

Per-project config lives in a `.pristrc` file at the repo root (equivalent to `.fvmrc`).

## Status

Phase 1 (MVP) and Phase 2 (dedup + concurrent engine) are implemented. Windows junctions and IDE integration land in Phase 3; self-update, completions, and distribution scripts in Phase 4. See `prist-spec-construccion.md` for the full roadmap.

## License

MIT
