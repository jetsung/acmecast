# Tasks

## 1. 调度弹窗组件实现

- [x] 1.1 在 `frontend/src/pages/schedules/ScheduleConfigPage.tsx` 中抽取 `ScheduleEditorModal` 组件：`Modal` 容器、`Form` 实例、`saving` 状态、`useEffect` 回填/重置、`destroyOnHidden`、编辑时 `pipeline_id` 禁用，保留全部字段（流水线 Select、cron Input、启用/补跑 Switch、续期域名 Select tags）、cron 预览与留空语义文案、保存校验与 `POST /api/schedules` 调用
- [x] 1.2 替换页面状态：将 `editingPipelineId`/`form`/`saving` 提升或迁移至弹窗内，页面保留 `editorOpen` + `editingSchedule` 状态及 `openEditor(schedule | null)` 入口；移除顶部常驻 `Card` 表单区域
- [x] 1.3 `PageHeader` 新增 `extra` "新建调度"按钮（`type="primary"`），已有调度卡片"编辑"按钮改为调用 `openEditor(schedule)`；保存成功回调中关闭弹窗并 `invalidateQueries(["schedules"])` / `["trigger-logs"]`

## 2. 测试更新

- [x] 2.1 更新 `frontend/src/pages/schedules/ScheduleConfigPage.test.tsx`：新增"新建按钮打开弹窗"用例、"编辑回填至弹窗"用例、"取消关闭不保留修改"用例、"保存成功关闭弹窗"用例；原有内联表单回填断言改为弹窗内断言
- [x] 2.2 回归既有能力：cron 预览、留空拦截、快捷启停、触发记录分页/过滤、流水线悬浮与跳转等用例保持通过（按需调整选择器以适配弹窗 DOM）

## 3. 验收与清理

- [x] 3.1 手动验收：页面默认无表单、新建/编辑弹窗开关与回填、保存与刷新、取消重置均符合预期
- [x] 3.2 运行 `pnpm test` / `pnpm build` 确认无类型与构建错误
