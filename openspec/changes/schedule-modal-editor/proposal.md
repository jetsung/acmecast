# Proposal

## Why

调度页当前的新建/编辑表单常驻页面顶部（Card 内联展示），占用首屏空间且与凭据等页面"点击新建/编辑弹出 Modal"的交互范式不一致，造成操作路径割裂、页面视觉冗余。统一为弹窗交互可收敛心智模型、释放列表视口、并与现有 `CredentialListPage` 的 `CredentialEditorModal` 模式对齐。

## What Changes

- 移除 `ScheduleConfigPage` 顶部常驻的表单 Card（含 `Form`、`Space` 布局、流水线/ cron /启用/补跑/续期域名等字段及 cron 预览文案），改为封装为独立的 `ScheduleEditorModal` 组件（`antd Modal`）。
- `PageHeader` 新增 `extra` 区域"新建调度"按钮（`type="primary"`），点击打开弹窗进入新建模式；已有调度卡片的"编辑"按钮改为打开同一弹窗并回填该行调度数据。
- 弹窗内保留全部现有表单字段、校验规则、cron 预览、留空语义文案与保存逻辑；`pipeline_id` 在编辑模式下禁用（与现有一致）。
- 弹窗支持取消关闭（`onCancel` / `destroyOnHidden`）、保存成功后自动关闭并刷新调度/触发记录列表。
- `toggleEnabled` 快捷启停开关保持在卡片上，不移入弹窗。
- 触发记录区域保持不变。

## Capabilities

### New Capabilities

- 无新增能力。

### Modified Capabilities

- `console-schedules`: 调度配置页的新建/编辑交互由"页面内联表单"改为"弹窗表单"，涉及需求"已有调度可回填编辑"的交互载体变更及新增的弹窗开关/回填/关闭行为。

## Impact

- 仅前端 `frontend/src/pages/schedules/ScheduleConfigPage.tsx` 变更，不涉及后端 API、路由或数据模型。
- 现有测试 `ScheduleConfigPage.test.tsx` 需同步更新：原有"回填到表单"的断言改为"弹窗打开后回填"。

## Risks

- 弹窗表单的受控状态与 `Form` 实例生命周期需正确处理，避免新建/编辑切换时残留值；可复用 `CredentialEditorModal` 已验证的 `useEffect` + `destroyOnHidden` 模式规避。
