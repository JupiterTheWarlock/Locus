# Locus AssetDb Watcher 卡住诊断报告

日期：2026-06-10  
测试对象：一个大型 Unity 项目  
测试分支：`codex/diagnose-asset-watcher-stall-instrumentation`  
上游基线：`r1n7aro/Locus` `main`，commit `36041d3d33f341ca9092ab6986b40f5500aa0d26`，版本 `v0.3.2`

## 结论

这次卡住不是因为 Locus 单纯读不动一个约 57 MiB 的 Unity scene 文件。该文件已经完成读取、YAML 解析、依赖提取等步骤，真正长时间停住的位置是在 SQLite 数据库更新阶段：

```text
path=large_scene.unity
stage=atomic update delete asset fts
```

该阶段会删除这个资源旧的搜索索引。当前实现会先取出该资源对应的所有 `object_key`，再对每个 `object_key` 单独执行一次 FTS 删除：

```sql
DELETE FROM asset_search_fts WHERE object_key = ?
```

`asset_search_fts` 是 FTS5 虚表，`object_key` 是 `UNINDEXED` 字段。SQLite query plan 显示该删除语句是虚表扫描：

```text
SCAN asset_search_fts VIRTUAL TABLE
```

所以这个路径在大型 Unity scene 上会退化成大量重复扫描 FTS 表。

## 为什么会稳定卡在这个 scene

这个 scene 不是普通的一条资源记录。当前数据库里，它展开成了：

- `35,047` 条 `asset_objects`
- `230,795` 条 `asset_object_type_terms`
- `14,088` 条 outgoing edges
- 文件大小：`59,806,341` bytes，约 `57.0 MiB`

全库 `asset_search_fts` 行数为 `518,699`。如果对该 scene 的每个 `object_key` 都单独删除一次 FTS 行，就可能触发约 35k 次针对 FTS 虚表的删除扫描。

只读统计显示，这个 scene 属于该项目中较大的 scene 之一，但不是唯一的大型 scene。也就是说，问题不应该理解为“某一个 Unity 文件损坏”，而应该理解为：

```text
大型 Unity scene 的 object 数量很高
+ 增量更新时逐个 object_key 删除 FTS 行
+ FTS 删除没有可用索引，只能扫描虚表
= watcher 长时间卡在 DB 更新阶段
```

本次启动重扫时，队列较早处理到了这个 scene，于是它先把 watcher 堵住。其它更大的 scene 如果进入同一路径，理论上也可能更严重。

## 复现情况

本地做了两次有效启动/重扫验证，均复现同一个真实卡点。

第一次有效复现：

```text
worker=0
path=large_scene.unity
stage=atomic update delete asset fts
stage_elapsed_ms=151003
item_elapsed_ms=176120
pending=29736
last_completed=atomic update delete old edges
```

第二次重启后复现：

```text
worker=2
path=large_scene.unity
stage=atomic update delete asset fts
stage_elapsed_ms=211786
item_elapsed_ms=236978
pending=29730
last_completed=atomic update delete old edges
```

注意：控制台原本的 queue summary 里 `current` 不一定显示真实卡住的文件。第二次复现时，summary 里一度显示的是另一个资源：

```text
current=other_asset
pending=29730
recent=0 in 8s
```

但本地临时加的 per-worker stage watchdog 显示，真正长时间占住处理流程的是 `large_scene.unity`。这个 per-worker stage watchdog 是本地临时诊断代码，不是上游已有功能。

## 相关代码路径

当前增量更新路径：

1. `atomic_update_asset` 开启事务。
2. 删除旧 edges。
3. 调用 `asset_fts::delete_by_asset_guid` 删除旧 FTS 搜索索引。
4. 删除旧 object/type terms。
5. 写入新的 asset/object/edge/file 记录。

关键点在 `asset_fts::delete_by_asset_guid`：

```rust
pub fn delete_by_asset_guid(tx: &Transaction, guid: &[u8]) -> Result<(), String> {
    let object_keys = {
        let mut stmt = tx
            .prepare_cached("SELECT object_key FROM asset_objects WHERE asset_guid = ?1")?;
        ...
    };
    for key in object_keys {
        delete_by_object_key(tx, &key)?;
    }
    Ok(())
}
```

而 `delete_by_object_key` 是：

```rust
DELETE FROM asset_search_fts WHERE object_key = ?1
```

这意味着一个有 35k object 的 scene 会触发 35k 次 FTS 删除语句。

## 推荐修复方向

优先建议修 `asset_search_fts` 的删除策略，而不是只跳过某个 scene。

可选方向：

1. 避免对同一个 asset 的所有 object_key 逐条执行 FTS 删除。
2. 让 FTS 表可以按 asset/guid 或 object_key 高效定位删除，而不是每次扫 FTS 虚表。
3. 对大型 asset 的索引更新走批量删除/重建路径。
4. 即使短期不能重构 FTS schema，也建议给 watcher 增加 per-worker stage watchdog 或长事务日志，至少能让卡点直接暴露出来。
5. 避免在持有全局 `graph_state` mutex 时执行可能持续数分钟的 FTS 删除，否则其它 worker 和后续 mtime scan 都会被一起堵住。

## 证据文件

证据摘录：

- `README.zh-CN.md`
- `evidence/first-reproduction-extract.md`
- `evidence/second-reproduction-extract.md`
- `evidence/EVIDENCE_SUMMARY.md`

