# Mention file finder core

`grapt_client::file_finder` provides repository file suggestions without a widget.
`SearchController::begin` accepts an absolute root, a query after `@`, and bounded
limits. It does no filesystem work. Every new query cancels the previous stamp;
`cancel` closes a mention, and dropping the controller cancels outstanding work.

Run `SearchRequest::search` on the caller's bounded worker. It reads names and ignore rules,
respects repository `.gitignore`, nested ignore rules and negation, includes ordinary hidden
files, excludes Git metadata, and follows no child symlinks. Ignore and traversal
errors refuse rather than returning an apparently complete partial list. Results
are relative `PathBuf`s, not permission to read a file; the eventual attachment
flow must still resolve and authorize its own read.

Git ignore rules apply only within a Git repository and stop at its `.git`
boundary, so a parent home `.gitignore` cannot hide a child repository. Outside
a repository, `.gitignore` does not filter suggestions; `.ignore` still applies
with the walker's standard ancestor and nested rules.

Nucleo scores paths with fuzzy matching, smart case and normalization. The best
matches sort by descending score and ascending path for ties. `rank_paths` offers
the same bounded, cancellable ranking without filesystem access. Display code
must escape path text and preserve the native path for selection.

Production bounds are 64 results, 10,000 entries delivered by the ignore walker,
depth 32, 256 query
bytes and 4,096 path bytes. Configured limits remain capped at 100 results,
100,000 entries and depth 64. Entry/depth cutoffs and omitted non-UTF8 or oversized
paths remain visible. A cutoff means best matches only among the visited paths.
An empty query lists bounded suggestions. No directory index, result cache,
background thread, unbounded channel, clock or rendering policy lives here.

Check `SearchResults::is_current()` immediately before publishing results: new
input can arrive after a worker finishes. Cancellation is checked before each
walk/ranking step and after completion. The ignore library may enumerate
ignored names internally; ignored directories are pruned. A walker step,
filesystem operation or bounded matcher call cannot be interrupted. A future widget owns scheduling and
terminal feedback. The current slice adds the core and its tests only.
