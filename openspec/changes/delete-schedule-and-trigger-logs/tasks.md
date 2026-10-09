# Tasks

## 1. 仓储层删除方法

- [x] 1.1 `ScheduleRepository` 新增 `delete(&self, pipeline_id: i64) -> Result<bool>`：按 `pipeline_id` 删除调度行，返回是否实际删除（false = 不存在）
- [x] 1.2 `TriggerLogRepository` 新增 `delete_by_pipeline(&self, pipeline_id: i64) -> Result<u64>`：删除该 `pipeline_id` 的全部触发记录，返回删除行数

## 2. 后端 HTTP 处理器

- [x] 2.1 在 `handlers.rs` 新增 `delete_schedule` 处理器：`Path(pipeline_id)` 取参，先调 `ScheduleRepository::delete`，false 返回 404 `not_found`，true 再调 `TriggerLogRepository::delete_by_pipeline` 清理触发记录，返回 `{ "deleted": true }`，记 `tracing::info!`
- [x] 2.2 在 `routes()` 新增 `.route("/api/schedules/{pipeline_id}", delete(delete_schedule))`
- [x] 2.3 在 `openapi.rs` 新增 `#[utoipa::path(delete, path = "/api/schedules/{pipeline_id}", ...)]` 占位函数并加入 `ApiDoc` 的 `paths(...)` 列表

## 3. 前端删除交互

- [x] 3.1 在 `ScheduleConfigPage` 新增 `confirmDelete(schedule)` 方法：`Modal.confirm` 二次确认 → `client.DELETE("/api/schedules/${pipeline_id}")` → 成功 `message.success("调度已删除")` + `invalidateQueries(["schedules"], ["trigger-logs"])`；失败展示错误信息
- [x] 3.2 在调度卡片 `编辑` 按钮旁新增 `Button size="small" danger` "删除"按钮，点击调用 `confirmDelete(schedule)`

## 4. 测试

- [x] 4.1 后端新增集成测试：删除已有调度（含触发记录）后调度与触发记录均被清除、返回 `{ deleted: true }`；删除不存在的调度返回 404
- [x] 4.2 前端新增测试：点击删除并确认后发出 `DELETE` 请求、成功后列表刷新；取消确认不发请求；删除失败展示错误
- [x] 4.3 回归既有调度页用例：新建/编辑弹窗、cron 预览、快捷启停、触发记录分页等保持通过

## 5. 验收

- [x] 5.1 运行 `cargo test` 与 `pnpm test` 确认全量通过
- [x] 5.2 运行 `pnpm --dir frontend exec tsc --noEmit` 确认无类型错误
