# Second Reproduction Extract

日期：2026-06-10  
对象：大型 Unity 项目  
目的：验证重启/重扫后是否稳定复现同一个真实卡点。

## Queue Summary 现象

第二次重启/重扫后，普通 queue summary 表面显示的是另一个资源：

```text
queue summary: pending=29730
current=other_asset
recent=0 in 8s
reasons=[none]
```

这进一步说明：queue summary 的 `current` 不一定是真正卡住的文件。

## Per-worker Stage Watchdog 时间线

本地临时加的 per-worker stage watchdog 显示，第二次仍然卡在同一个大型 Unity scene、同一个阶段：

```text
worker=2
path=large_scene.unity
stage=atomic update delete asset fts
stage_elapsed_ms=10592
item_elapsed_ms=35785
pending=29730
last_completed=atomic update delete old edges
```

之后仍未推进：

```text
worker=2
path=large_scene.unity
stage=atomic update delete asset fts
stage_elapsed_ms=30747
item_elapsed_ms=55940
pending=29730
last_completed=atomic update delete old edges
```

继续等待后仍未推进：

```text
worker=2
path=large_scene.unity
stage=atomic update delete asset fts
stage_elapsed_ms=91337
item_elapsed_ms=116530
pending=29730
last_completed=atomic update delete old edges
```

最终观察窗口内仍然停在同一阶段：

```text
worker=2
path=large_scene.unity
stage=atomic update delete asset fts
stage_elapsed_ms=211786
item_elapsed_ms=236978
pending=29730
last_completed=atomic update delete old edges
```

## 同时被阻塞的其它 worker

其它 worker 当时在等待共享 asset DB 状态：

```text
stage=wait for graph_state mutex reading script type metadata
stage=wait for graph_state mutex resolving refs
```

## 数据库文件规模

第二次复现时 AssetDb 文件规模仍为：

```text
locus.db:     4,135,923,712 bytes
locus.db-wal: 4,160,495,512 bytes
locus.db-shm:        32,768 bytes
```

## 解释

第二次复现证明：

1. 这不是单次偶发日志误判。
2. 重启/重扫后仍然命中同一个真实卡点。
3. 表面 queue summary 的 `current` 可以是其它资源，但真实持锁 worker 仍在大型 scene 的 FTS 删除阶段。

