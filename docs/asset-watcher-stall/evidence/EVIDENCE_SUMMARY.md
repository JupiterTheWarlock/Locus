# Evidence Summary

This file collects the evidence needed to locate the Locus-side failure.

## Environment

- Date: 2026-06-10
- Locus baseline: upstream `r1n7aro/Locus` `main`
- Commit: `36041d3d33f341ca9092ab6986b40f5500aa0d26`
- Version observed in build: `v0.3.2`
- Reproduction project: `large-unity-project`

## Large Scene Database Shape

The stuck asset is a large Unity scene, tracked as:

```text
large_scene.unity
```

Read-only SQLite counts for that asset:

```text
asset_objects:            35,047
asset_object_type_terms: 230,795
outgoing edges:           14,088
asset file size:      59,806,341 bytes
```

Database-wide FTS row count at the time:

```text
asset_search_fts rows: 518,699
```

SQLite query plan for per-object FTS deletion:

```text
EXPLAIN QUERY PLAN DELETE FROM asset_search_fts WHERE object_key = ?
SCAN asset_search_fts VIRTUAL TABLE
```

## First Reproduction

The normal queue summary continued to repeat while the queue stopped making progress. The temporary local per-worker stage watchdog showed the real stuck worker:

```text
worker=0
path=large_scene.unity
stage=atomic update delete asset fts
stage_elapsed_ms=151003
item_elapsed_ms=176120
pending=29736
active_workers=3
recent=0 in 8s
last_completed=atomic update delete old edges
last_error=-
```

Other workers were waiting behind the same shared asset DB state:

```text
stage=wait for graph_state mutex before DB update
stage=wait for graph_state mutex resolving refs
```

## Second Reproduction

After restart/re-scan, the same true stuck asset and stage repeated:

```text
worker=2
path=large_scene.unity
stage=atomic update delete asset fts
stage_elapsed_ms=10592
item_elapsed_ms=35785
pending=29730
last_completed=atomic update delete old edges
```

Later in the same run:

```text
worker=2
path=large_scene.unity
stage=atomic update delete asset fts
stage_elapsed_ms=211786
item_elapsed_ms=236978
pending=29730
last_completed=atomic update delete old edges
```

The visible queue summary's `current` value was not the true stuck asset:

```text
current=other_asset
pending=29730
recent=0 in 8s
reasons=[none]
```

This is why the issue should not claim that queue summary `current` is the real stuck file. The real stuck worker was visible only after adding the temporary per-worker stage watchdog.

## Interpretation

The observed hang is consistent with the incremental asset update path deleting FTS rows one `object_key` at a time. For a large scene with 35k object keys and an FTS5 table where `object_key` is `UNINDEXED`, this can become thousands of virtual table scans while the shared asset DB mutex is held.

