# Design

## Context

当前 `ScheduleConfigPage` 将新建/编辑表单以内联 `Card + Form` 形式常驻页面顶部，通过 `editingPipelineId` 区分新建/编辑、`startEdit`/`exitEditMode` 控制回填与重置。凭据页 `CredentialListPage` 已采用 `Modal + Form` 弹窗模式（`CredentialEditorModal`），通过 `editorOpen`/`editingId` 控制开关、`useEffect` 回填、`destroyOnHidden` 清理，进入编辑时按需拉取详情。调度页需向后者对齐。

约束：仅前端变更，不改后端 API；保留所有字段、校验、cron 预览、留空语义、保存与快捷启停逻辑。

## Goals / Non-Goals

**Goals:**
- 调度新建/编辑统一收敛到弹窗交互，页面默认仅展示"已有调度"列表与"触发记录"。
- 弹窗复用现有表单字段与校验行为，编辑回填与新建重置语义保持一致。

**Non-Goals:**
- 不新增或修改调度数据模型、API 契约、路由结构。
- 不改动触发记录区域与 `toggleEnabled` 快捷启停交互。
- 不引入新的表单校验规则或字段。

## Decisions

### 1. 抽取 `ScheduleEditorModal` 独立组件（同文件内或独立文件）

**决策**：在 `ScheduleConfigPage.tsx` 同文件内新增 `ScheduleEditorModal` 函数组件，接收 `open`、`editingSchedule`（`ScheduleRow | null`）、`pipelines`、`onClose`、`onSaved` 等 props，内部持有 `Form` 实例与 `saving` 状态，复用现有 `onSave` 逻辑。

**理由**：与 `CredentialEditorModal` 结构对齐，保持单文件可读性；调度表单无跨类型 schema 驱动，无需拆分为独立文件即可维护。

**替代**：独立文件 `ScheduleEditorModal.tsx` —— 收益有限，调度表单字段固定，拆分反而增加引用成本；若后续字段扩张再考虑拆分。

### 2. 状态模型：`editorOpen` + `editingSchedule`

**决策**：以 `editorOpen: boolean` 与 `editingSchedule: ScheduleRow | null` 替代现有的 `editingPipelineId: number | null`。`openEditor(schedule | null)` 统一入口：`null` 为新建，传入行数据为编辑。

**理由**：与凭据页 `editorOpen`/`editingId` 对称；`editingSchedule` 携带完整行数据，弹窗可直接回填无需额外请求（调度列表已含全量字段，区别于凭据需详情接口）。

### 3. 回填与重置：`useEffect` 监听 `open` + `editingSchedule`

**决策**：`useEffect` 在 `open` 为真时：若 `editingSchedule` 非空则 `form.setFieldsValue(…)`，否则 `form.resetFields()`。`Modal` 设 `destroyOnHidden`，关闭即销毁表单状态。

**理由**：复用凭据页已验证模式，避免新建/编辑切换时的字段残留。

### 4. 按钮布局：`PageHeader extra` 新建 + 卡片内编辑

**决策**：`PageHeader.extra` 置"新建调度"主按钮；每张调度卡片保留"编辑"按钮，触发同一弹窗。

**理由**：与 `CredentialListPage` 的 `PageHeader.extra` 新建按钮一致，用户路径统一。

## Risks / Trade-offs

- **表单残留**：`Form` 实例跨弹窗复用时需确保 `resetFields` 时机正确；`destroyOnHidden` 可兜底，但需验证 `initialValue`（如 `enabled: true`）在新建时正确回退。
- **测试回归**：现有"编辑回填"用例直接查询页面内表单元素，改为弹窗后需 `fireEvent.click(编辑)` 再断言 `Modal` 内表单；需同步更新用例选择器。
- **无后端风险**：保存逻辑（`POST /api/schedules` upsert）不变，无数据一致性风险。

## Migration Plan

无数据迁移。前端一次性替换，灰度不适用。

## Open Questions

- 无。
