# Asset Watcher Stall 诊断资料目录

本目录整理了可直接附在上游 issue 中的诊断资料。

## 推荐阅读顺序

1. `ROOT_CAUSE_ANALYSIS.zh-CN.md`
   - 人类可读的主报告。
   - 解释为什么卡住、为什么是大型 scene、以及推荐修复方向。

2. `evidence/first-reproduction-extract.md`
   - 第一次有效复现的提取日志。

3. `evidence/second-reproduction-extract.md`
   - 第二次重启/重扫复现的提取日志。

4. `evidence/EVIDENCE_SUMMARY.md`
   - 两次复现和数据库统计的合并摘要。

5. `../ASSET_WATCHER_STALL_DIAGNOSIS_PLAN.md`
   - 执行诊断前使用的检查计划。

## 重要说明

`per-worker stage watchdog` 是本地临时诊断 instrumentation，不是上游已有功能。issue 中应明确写成“本地临时加了 per-worker stage watchdog 后发现”，避免让维护者误以为这是当前上游版本已有日志。

