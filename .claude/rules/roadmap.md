# Roadmap

- `docs/ROADMAP.md` is only an index of the list files and the item format. The lists are
  `docs/FEATURES.md`, `docs/BUGS.md` and `docs/PERFORMANCE.md`. A new kind of work gets a new
  list file and an index row.
- Each list is a set of `# Category` headings, and under each one bullet per item:

  ```markdown
  # Capture
  - [M] `capture-pipewire`: consume the portal's PipeWire node into `Frame`s
  - [S, blocked by: capture-pipewire] `capture-cursor-metadata`: forward cursor metadata
  ```

  The bracket holds the size (`S`, `M`, `L`, `XL`) and optionally `blocked by:` with the
  comma-separated ids of the items it waits on. The id is a unique kebab-case slug in backticks.
- **An implemented item is deleted** from its list in the same commit, along with its id in any
  other item's `blocked by`. The lists hold only outstanding work and never get "done" markers.
- A bug found while working goes into `docs/BUGS.md` in the same commit that finds it, unless that
  commit fixes it.
