# Spec Delta — console-schedules

## ADDED Requirements

### Requirement: 调度可删除并联动清理触发记录

已有调度列表的每一项 SHALL 提供"删除"操作：点击后弹出二次确认对话框，确认后删除该调度的全部配置以及该流水线的全部触发记录，删除成功后刷新调度列表与触发记录列表。

- 删除操作 SHALL 通过 `DELETE /api/schedules/{pipeline_id}` 端点完成；`pipeline_id` 为路径参数，标识待删除调度绑定的流水线。
- 调度不存在时 SHALL 返回 404（`not_found`）；删除成功 SHALL 返回 `{ "deleted": true }`。
- 删除调度 SHALL 同时删除该 `pipeline_id` 的全部触发记录（`trigger_log`），避免残留孤儿审计数据。
- 前端 SHALL 在已有调度卡片上提供"删除"按钮（danger 样式），点击后弹出二次确认对话框；确认后才发出删除请求。
- 删除成功后 SHALL 刷新调度列表（`GET /api/schedules`）与触发记录列表（`GET /api/schedules/trigger-logs`），并提示"调度已删除"。
- 删除失败时 SHALL 展示错误信息，不刷新列表。

#### Scenario: 删除已有调度

- **WHEN** 用户对流水线 1 的调度点击"删除"并在确认对话框中确认
- **THEN** 页面发出 `DELETE /api/schedules/1` 请求，成功后提示"调度已删除"，调度列表不再包含流水线 1 的调度

#### Scenario: 删除调度联动清理触发记录

- **WHEN** 用户删除流水线 1 的调度且该流水线有 3 条触发记录
- **THEN** 后端删除调度配置后同时删除该 `pipeline_id` 的全部触发记录，触发记录列表不再包含流水线 1 的记录

#### Scenario: 删除不存在的调度返回 404

- **WHEN** 用户对已不存在的调度（`pipeline_id=999`）发起删除请求
- **THEN** 后端返回 404（`not_found`），前端展示错误提示，列表不变

#### Scenario: 取消删除不发出请求

- **WHEN** 用户点击"删除"后在确认对话框中点击取消
- **THEN** 不发出删除请求，列表不变

#### Scenario: 删除失败展示错误

- **WHEN** 删除请求返回错误
- **THEN** 页面展示错误信息，调度列表与触发记录列表不变
