# Archive: 2026-06-07 cleanup

Run via `/cleanup` skill on 2026-06-07. Permanent provenance — do not delete.

| Original path | Why retired | Where its live content went | Restore command |
|---|---|---|---|
| `prompt.md` | Session-start handoff document (untracked) for the routing/volume/clips fixes session that landed as `c1e6dec` + `33ac696`. Its objective (4 fixes, manual verification, worktree cleanup, CLAUDE.md exclude decision) was fully discharged across this and earlier sessions. All durable outcomes are now reachable from `main` history and project memory; no live reference remains. | Outcomes are on `main` (merges + subsequent reworks: clips game-detection removal `7265219`, clip-save fixes `7409c15`/`0713644`, library live-refresh `632c585`, save-wedge timeout `03e7561`/`f42d4af`, clips page redesign `91dd8f3`/`60f7f11`/`09dbfd0`). Architecture facts updated in `CLAUDE.md`. Memory entry `project_clipping_in_progress.md` updated to reflect post-rework state. | `mv .claude/archive/2026-06-07-cleanup/prompt.md prompt.md` (it was untracked; restore with plain `mv`, not `git mv`) |

## Verification at archive time

- `grep -rn "prompt.md" --exclude-dir=target --exclude-dir=.git --exclude-dir=.claude` returned no live references in the project tree before the move.
- After the move: `prompt.md` no longer appears at the repo root.

## Flagged but NOT touched (out of /cleanup scope — unrelated staleness)

Three project docs were modified in the working tree at the start of this cleanup session and have been uncommitted since before this session began (the modifications are pre-existing). They are pure mechanical path-case normalizations (`SteelseriesFlatpak` → `steelseries-flatpak`, matching the actual on-disk repo dir name) — 39 insertions / 39 deletions across:

- `docs/superpowers/plans/2026-05-08-clipping-system.md` (4 ± lines)
- `docs/superpowers/plans/2026-05-14-routing-volume-clips-fixes.md` (68 ± lines)
- `docs/superpowers/specs/2026-05-13-routing-volume-clips-fixes-design.md` (6 ± lines)

These are not retired content — they're a low-risk path correction the project owner can commit separately whenever they choose. The /cleanup skill explicitly says "fix only references to the retired system. Unrelated staleness you notice → flag, don't touch."
