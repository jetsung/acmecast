/**
 * cert.deploy「SSH 远程主机」变体的布局与跳板清空端到端验证。
 *
 * SSH 变体按注入的 x-field-order 渲染成「两行两列」：key/fullchain 一行、
 * ca/reload_command 一行（主机档案与跳板各独占一行）。字段顺序与半宽
 * （ant-col-12）在 PipelineEditorPage.test.tsx 里已断言，但「两个半宽字段真的
 * 落在同一行」取决于栅格换行，jsdom 没有布局引擎——只能在真实浏览器里量；
 * 跳板的 allowClear 清空同理是真实交互。后端不参与，/api 全部用 route 喂假数据。
 */
import { expect, test } from "@playwright/test";

const TASK_TYPE = "e2e.deploy-ssh";

/**
 * cert.deploy 输入形态的桩 schema，SSH 变体 properties 键序故意打乱：渲染顺序
 * 应来自前端注入的 x-field-order，而不是后端 schema 的键序（真实后端按字母序
 * 产出）。连接/认证/权限字段保留在桩里，前端收紧后不应渲染。
 */
const DEPLOY_SCHEMA = {
  type: "object",
  required: ["target"],
  properties: {
    force: { type: "boolean", description: "强制重写已部署的证书。" },
    target: { type: "string", description: "部署目标。" },
    config: {
      // 与真实后端一致：`serde_json::Value` 没有 type，靠 x-target-schemas 触发
      // 结构化变体子表单（有 type 会走普通对象分支）。
      description: "结构与所选目标一致。",
      "x-target-schemas": {
        ssh: {
          display_name: "SSH 远程主机",
          example: {},
          schema: {
            type: "object",
            properties: {
              key_mode: { type: ["string", "null"] },
              cert_mode: { type: ["string", "null"] },
              key_path: { type: ["string", "null"] },
              fullchain_path: { type: ["string", "null"] },
              ca_path: { type: ["string", "null"] },
              cert_path: { type: ["string", "null"] },
              port: { type: ["integer", "null"] },
              host: { type: ["string", "null"] },
              user: { type: ["string", "null"] },
              auth: { description: "登录远程主机的方式。" },
              reload_command: {
                type: ["string", "null"],
                description: "写入成功后在远程执行的重载命令。",
              },
              credential_id: {
                type: ["integer", "null"],
                description: "SSH 主机档案凭据的标识。",
              },
              bastion_credential_id: {
                type: ["integer", "null"],
                description: "跳板机凭据的标识。",
                default: null,
              },
            },
          },
        },
      },
    },
  },
};

const SSH_HOSTS = [
  { id: 10, name: "web-01", type_id: "deploy.ssh" },
  { id: 41, name: "jump-01", type_id: "deploy.ssh" },
];

test.beforeEach(async ({ page }) => {
  // 路由守卫只看令牌是否存在，塞一个即可通过。
  await page.addInitScript(() => {
    localStorage.setItem(
      "acmecast-auth",
      JSON.stringify({ state: { token: "e2e-token", username: "admin" }, version: 0 }),
    );
  });

  // 兜底：任何未显式桩掉的 /api 都返回 404，避免打到不存在的后端。
  await page.route("**/api/**", (route) =>
    route.fulfill({ status: 404, json: { error: { code: "not_found", message: "e2e stub" } } }),
  );
  // 后注册的优先级更高。
  await page.route("**/api/tasks", (route) =>
    route.fulfill({ json: { data: [{ type_id: TASK_TYPE, display_name: "E2E SSH 部署" }] } }),
  );
  await page.route("**/api/tasks/*/schema", (route) =>
    route.fulfill({ json: { data: DEPLOY_SCHEMA } }),
  );
  await page.route("**/api/credentials*", (route) =>
    route.fulfill({
      json: { data: { items: SSH_HOSTS, total: SSH_HOSTS.length, page: 1, page_size: 100 } },
    }),
  );
});

/** 打开编辑器并选中 SSH 变体：主机档案选定后跳板字段才渲染。 */
async function openSshVariant(page: import("@playwright/test").Page) {
  await page.goto("/pipelines/new");
  await page.getByRole("button", { name: "添加步骤" }).click();

  // 显式选任务类型（不依赖任务列表已就绪时的默认值）。
  await page.getByLabel("任务类型").first().click();
  await page.locator('.ant-select-item-option[title="E2E SSH 部署"]').click();

  // 选出部署目标后 config 子表单才渲染 SSH 变体字段。
  await page.getByLabel("target").click();
  await page.locator('.ant-select-item-option[title="SSH 远程主机"]').click();

  // 主机档案选定后跳板字段出现（未选档案时隐藏，由 x-visible-when 控制）。
  await page.getByLabel("SSH 主机档案").click();
  await page.locator('.ant-select-item-option[title="web-01（id=10）"]').click();
  await expect(page.getByLabel("跳板机")).toBeVisible();
}

test("SSH 变体两行两列：key/fullchain 与 ca/reload_command 成对同行", async ({ page }) => {
  await openSshVariant(page);

  // 成对字段：第一行 key+fullchain，第二行 ca+reload_command。
  const rows: [string, string][] = [
    ["key_path", "fullchain_path"],
    ["ca_path", "reload_command"],
  ];

  const boxes = new Map<string, { x: number; y: number }>();
  for (const [left, right] of rows) {
    for (const label of [left, right]) {
      const control = page.getByLabel(label, { exact: true });
      await expect(control).toBeVisible();
      const box = await control.boundingBox();
      expect(box, `${label} 应有布局盒`).not.toBeNull();
      boxes.set(label, { x: box!.x, y: box!.y });
    }
  }

  let previousRowTop = Number.NEGATIVE_INFINITY;
  for (const [left, right] of rows) {
    const leftBox = boxes.get(left)!;
    const rightBox = boxes.get(right)!;
    // 同一行：控件顶边对齐（同一栅格行）。不同行时相差一整个表单项高度。
    expect(Math.abs(leftBox.y - rightBox.y), `${left} 与 ${right} 应在同一行`).toBeLessThanOrEqual(2);
    // 左路径/中间链在左，重载命令在右。
    expect(leftBox.x, `${left} 应在 ${right} 左侧`).toBeLessThan(rightBox.x);
    // 两行自上而下依次排列。
    expect(leftBox.y, `${left} 一行应在上一行之下`).toBeGreaterThan(previousRowTop);
    previousRowTop = leftBox.y;
  }

  // 连接/认证/权限字段不渲染（ tightened 剔除），旧字段名 cert_path 同样不出现。
  for (const banned of ["host", "port", "user", "cert_mode", "key_mode", "cert_path"]) {
    await expect(page.getByLabel(banned, { exact: true })).toHaveCount(0);
  }
});

test("跳板机选错可清空：退回未选", async ({ page }) => {
  await openSshVariant(page);

  // 选跳板 jump-01（id=41）：用 aria-owns 锁定跳板下拉自己的面板——残留的
  // 档案下拉面板（同样含 jump-01 选项）还留在 DOM 里，全局选会撞重。
  const bastionTrigger = page.getByLabel("跳板机");
  await bastionTrigger.click();
  const listId = await bastionTrigger.getAttribute("aria-owns");
  const bastionPanel = page
    .locator(".ant-select-dropdown")
    .filter({ has: page.locator(`#${listId}`) });
  await bastionPanel.locator('.ant-select-item-option[title="jump-01（id=41）"]').click();

  // 选中值回显在跳板下拉里（用「含跳板 combobox 的 .ant-select」精确定位，
  // 不按表单项文本过滤——config 组包装也含「跳板机」字样，会撞重）。
  const bastionSelect = page.locator(".ant-select").filter({ has: page.getByLabel("跳板机") });
  await expect(bastionSelect).toContainText("jump-01（id=41）");

  // 清空：退回未选（回显消失）。清空按钮悬停才可见，先 hover 再点。
  await bastionSelect.hover();
  await bastionSelect.getByRole("button", { name: "Clear" }).click();
  await expect(bastionSelect).not.toContainText("jump-01（id=41）");
});
