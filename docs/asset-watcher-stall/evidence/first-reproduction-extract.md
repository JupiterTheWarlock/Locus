# First Reproduction Extract

日期：2026-06-10  
对象：大型 Unity 项目  
目的：确认 AssetDb watcher 队列停止推进时，真实卡点是否在某个 worker 内部。

## Queue Summary 现象

控制台持续输出 queue summary，队列 pending 不下降，recent 变为 0：

```text
queue summary: pending=29736
current=queue_summary_current_asset
recent=0 in 8s
reasons=[none]
```

注意：这个 `current` 不是最终确认的真实卡点，只是普通 queue summary 当时显示的 current。

## Per-worker Stage Watchdog 关键摘录

本地临时加的 per-worker stage watchdog 显示，真实长时间占住处理流程的是一个大型 Unity scene：

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

同一时刻，其它 worker 在等待同一个共享状态：

```text
stage=wait for graph_state mutex before DB update
stage=wait for graph_state mutex resolving refs
```

## 数据库文件规模

复现时 AssetDb 文件规模约为：

```text
locus.db:     4,135,923,712 bytes
locus.db-wal: 4,160,495,512 bytes
locus.db-shm:        32,768 bytes
```

## 解释

第一次复现证明：

1. 卡点不是读取 scene 文件。
2. 卡点不是 YAML parse。
3. 卡点发生在增量数据库更新里的 `atomic update delete asset fts`。
4. 普通 queue summary 的 `current` 可能误导，需要 per-worker stage 才能看到真实持锁 worker。

