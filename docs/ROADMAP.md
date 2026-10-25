# Roadmap

Outstanding work, split by kind. Finished items are deleted, not ticked off.

| List | Holds |
|---|---|
| [FEATURES.md](FEATURES.md) | Functionality still to build |
| [BUGS.md](BUGS.md) | Known defects |
| [PERFORMANCE.md](PERFORMANCE.md) | Latency, throughput and quality work on paths that already function |

## Item format

```markdown
# Category
- [M] `item-id`: what needs doing
- [S, blocked by: item-id, other-id] `next-id`: what needs doing after those
```

Sizes: `S` is under a day, `M` a few days, `L` about a week or two, `XL` needs splitting before
work starts. `blocked by` lists the ids of items that have to land first, and is omitted when
nothing blocks the item.
