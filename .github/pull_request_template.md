Closes #[issue number]

## Description

Describe in English the changes made in this pull request and the problem it solves.
Please keep **1 PR to solve 1 problem**, and keep **small improvements small** to make the PR easier to review and to merge.

## Screenshot

| Before                       | After                       |
| ---------------------------- | --------------------------- |
| [Put Before Screenshot here] | [Put After Screenshot here] |

## Public API

List every public item this pull request adds, changes or removes, with its signature and one line on what it is for. If none, remove this section.

- `gpui_kit_grid::LiveGridState::example(value: bool) -> Self` — what it does.

## Breaking Changes

Describe any breaking changes introduced by this pull request. If none, remove this section.

- Change 1

```diff
- Old code snippet
+ New code snippet
```

## Performance

Required when the change touches ingestion, the projection engine or painting. Give the benchmark or profile (`MTL_HUD_ENABLED=1`, `samply record`) before and after, with the load profile (rows, updates per second). If not applicable, remove this section.

## How to Test

Please describe the tests that you ran to verify your changes. Provide instructions so we can reproduce.

## Checklist

- [ ] I have read the [CONTRIBUTING](../CONTRIBUTING.md) document and followed the guidelines.
- [ ] Reviewed the changes in this PR and confirmed AI generated code (if any) is accurate, and marked AI-generated portions.
- [ ] Ran the grid examples related to the changes.
- [ ] Tested macOS, Windows and Linux platforms performance (if the change is platform-specific).
