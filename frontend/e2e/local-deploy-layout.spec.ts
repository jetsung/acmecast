/**
 * cert.deploy「本地文件系统」变体的布局端到端验证。
 *
 * local 变体的九个字段按注入的 x-field-order 渲染成「三行两列」：key /
 * fullchain / ca 各占一行，每行左路径右权限。字段顺序与半宽（ant-col-12）在
 * PipelineEditorPage.test.tsx 里已断言，但「两个半宽字段真的落在同一行」取决于
 * 栅格换行，jsdom 没有布局引擎——只能在真实浏览器里量。后端不参与，/api 全部
 * 用 route 喂假数据。
 */
import { expect, test } from "@playwright/test";

const TASK_TYPE = "e2e.deploy";

/**
 * cert.deploy 输入形态的桩 schema，properties 键序故意打乱：渲染顺序应来自
 * 前端注入的 x-field-order，而不是后端 schema 的键序（真实后端按字母序产出）。
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
        local: {
          display_name: "本地文件系统",
          example: {},
          schema: {
            type: "object",
            required: ["fullchain_path", "key_path"],
            properties: {
              key_mode: { type: ["string", "null"] },
              uid: { type: ["integer", "null"] },
              ca_mode: { type: ["string", "null"] },
              fullchain_path: { type: ["string", "null"] },
              key_path: { type: ["string", "null"] },
              cert_mode: { type: ["string", "null"] },
              ca_path: { type: ["string", "null"] },
              gid: { type: ["integer", "null"] },
              reload_command: { type: ["string", "null"] },
            },
          },
        },
      },
    },
  },
};

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
    route.fulfill({ json: { data: [{ type_id: TASK_TYPE, display_name: "E2E 部署" }] } }),
  );
  await page.route("**/api/tasks/*/schema", (route) =>
    route.fulfill({ json: { data: DEPLOY_SCHEMA } }),
  );
  await page.route("**/api/credentials*", (route) =>
    route.fulfill({ json: { data: { items: [], total: 0, page: 1, page_size: 100 } } }),
  );
});

test("local 变体三行两列：路径与权限成对同行，左路径右权限", async ({ page }) => {
  await page.goto("/pipelines/new");
  await page.getByRole("button", { name: "添加步骤" }).click();

  // 显式选任务类型（不依赖任务列表已就绪时的默认值）。
  await page.getByLabel("任务类型").first().click();
  await page.locator('.ant-select-item-option[title="E2E 部署"]').click();

  // 选出部署目标后 config 子表单才渲染 local 变体字段。
  await page.getByLabel("target").click();
  await page.locator('.ant-select-item-option[title="本地文件系统"]').click();

  // 成对字段：每行左路径右权限。
  const pairs: [string, string][] = [
    ["key_path", "key_mode"],
    ["fullchain_path", "cert_mode"],
    ["ca_path", "ca_mode"],
  ];

  const boxes = new Map<string, { x: number; y: number }>();
  for (const [left, right] of pairs) {
    for (const label of [left, right]) {
      const control = page.getByLabel(label, { exact: true });
      await expect(control).toBeVisible();
      const box = await control.boundingBox();
      expect(box, `${label} 应有布局盒`).not.toBeNull();
      boxes.set(label, { x: box!.x, y: box!.y });
    }
  }

  let previousRowTop = Number.NEGATIVE_INFINITY;
  for (const [left, right] of pairs) {
    const leftBox = boxes.get(left)!;
    const rightBox = boxes.get(right)!;
    // 同一行：控件顶边对齐（同一栅格行）。不同行时相差一整个表单项高度。
    expect(Math.abs(leftBox.y - rightBox.y), `${left} 与 ${right} 应在同一行`).toBeLessThanOrEqual(2);
    // 左路径右权限。
    expect(leftBox.x, `${left} 应在 ${right} 左侧`).toBeLessThan(rightBox.x);
    // key / fullchain / ca 三行自上而下依次排列。
    expect(leftBox.y, `${left} 一行应在上一行之下`).toBeGreaterThan(previousRowTop);
    previousRowTop = leftBox.y;
  }
});
