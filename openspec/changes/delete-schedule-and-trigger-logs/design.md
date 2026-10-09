# Design

## Context

当前调度仅支持新建/编辑（`POST /api/schedules`，upsert 语义），无删除入口。仓储层 `ScheduleRepository` 有 `save`/`find`/`list_all`/`exists_by_pipeline_id`/`mark_triggered` 但无 delete 方法；`TriggerLogRepository` 有 `record`/`last_triggered_at`/`list` 但无按 `pipeline_id` 批量删除的方法。流水线删除（`delete_pipeline`）在删除前检查调度绑定（409 `schedule_bound`），但反向"删除调度"不存在。

前端调度卡片有编辑按钮和快捷启停开关，无删除按钮。凭据页已有成熟的 `Modal.confirm` + `client.DELETE` 二次确认模式可复用。

## Goals / Non-Goals

**Goals:**
- 后端新增 `DELETE /api/schedules/{pipeline_id}` 端点，删除调度并联动清理该 `pipeline_id` 的全部触发记录。
- 前端调度卡片新增"删除"按钮 + 二次确认，删除成功后刷新调度列表与触发记录。

**Non-Goals:**
- 不新增独立的触发记录删除接口（清理仅通过删除调度联动完成）。
- 不改动流水线删除的调度绑定检查逻辑。
- 不改动调度引擎的运行时装载逻辑（删除后引擎下次 tick 自然不再触发该流水线）。

## Decisions

### 1. 删除端点：`DELETE /api/schedules/{pipeline_id}`

**决策**：路由路径用 `pipeline_id` 作为路径参数（与调度按流水线一一对应的领域模型一致），而非自增主键 `id`。删除不存在时返回 404。

**理由**：调度的唯一业务键是 `pipeline_id`（`schedule` 表 `pipeline_id` 唯一），前端列表中的每条调度以 `pipeline_id` 标识，用 `pipeline_id` 作为删除键与现有 `exists_by_pipeline_id` 查询口径对称。

**替代**：用自增 `id` —— 需在 `ScheduleResponse` 中暴露 `id`，当前响应不含该字段，改动面更大且无业务收益。

### 2. 联动清理触发记录：仓储层 `delete_by_pipeline` + 处理器编排

**决策**：
- `ScheduleRepository` 新增 `delete(&self, pipeline_id) -> Result<bool>`，删除调度行，返回是否实际删除了行（false = 不存在）。
- `TriggerLogRepository` 新增 `delete_by_pipeline(&self, pipeline_id) -> Result<u64>`，删除该 `pipeline_id` 的全部触发记录，返回删除行数。
- 处理器 `delete_schedule` 先调 `ScheduleRepository::delete`，若返回 false 则 404；若返回 true 则调 `TriggerLogRepository::delete_by_pipeline` 清理孤儿触发记录，返回 `{ "deleted": true }`。

**理由**：两步均在同一处理器内串行执行，不引入跨 crate 事务（当前项目仓储层未使用显式事务）；先删调度再清触发记录，即使清触发记录失败，调度已删除，引擎不再装载该流水线，最差情况是残留少量孤儿日志（不影响功能）。

**替代**：数据库级联删除（外键 `ON DELETE CASCADE`）——需 migration 改表结构，且当前 `trigger_log` 表无到 `schedule` 的外键约束，改动风险高于应用层编排。

### 3. 前端删除交互：复用 `Modal.confirm` 模式

**决策**：调度卡片新增 `Button size="small" danger` "删除"按钮，点击后 `Modal.confirm` 弹出二次确认（标题"删除调度？"，内容提示流水线编号与"删除后无法恢复，关联的触发记录将一并清理"），确认后 `client.DELETE("/api/schedules/{pipeline_id}")`，成功后 `message.success("调度已删除")` 并 `invalidateQueries(["schedules"], ["trigger-logs"])`。

**理由**：与 `CredentialListPage.confirmDelete` 模式完全对齐，用户心智一致。

### 4. 路由注册与 OpenAPI 文档

**决策**：在 `routes()` 中将 `/api/schedules` 路由改为 `.get(list_schedules).post(create_schedule).delete(delete_schedule)` 不适用（delete 需路径参数），改为新增 `.route("/api/schedules/{pipeline_id}", delete(delete_schedule))`。在 `openapi.rs` 新增 `#[utoipa::path(delete, ...)]` 占位函数并加入 `ApiDoc` 的 `paths(...)` 列表。

**理由**：与 `delete_pipeline`（`/api/pipelines/{id}`）和 `delete_credential`（`/api/credentials/{id}`）的路由注册模式一致。

## Risks / Trade-offs

- **无事务包裹**：先删调度再清触发记录，中间若进程崩溃则残留孤儿日志。可接受：孤儿日志不影响功能（引擎不再装载该调度，日志仅为审计），且后续重建同 `pipeline_id` 调度时 `last_triggered_at` 查询会正确返回 `None`（因为日志已部分删除或全删）。
- **不可逆**：删除即物理删除，无软删除。二次确认降低误触风险；与凭据/流水线删除语义一致。
- **引擎缓存**：引擎在 `load` 时一次性装载全部调度到内存，删除后引擎下次 `run_cron_due` tick 仍会查询最新调度列表（当前引擎每轮 `list_all`），不会对已删除调度触发。
