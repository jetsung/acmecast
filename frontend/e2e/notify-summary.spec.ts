/**
 * 运行简讯（notify.summary）步骤的渠道多选端到端验证（本变更 5.2/5.3）：
 * - `channels` 字段经 `x-options` 注入渲染为多选下拉，选项列出全部已声明渠道
 *   （含 `enabled = false` 的），标签带 provider 与启用状态；
 * - 保存时提交选中的字符串数组（`input.channels = ["lark", "ntfy"]`）；
 * - 重新打开编辑页回显选中状态。
 *
 * 后端不参与，/api 全部用 route 喂假数据。
 */
import { expect, test } from "@playwright/test";

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
    route.fulfill({
      json: { data: [{ type_id: "notify.summary", display_name: "运行简讯" }] },
    }),
  );
  await page.route("**/api/tasks/notify.summary/schema", (route) =>
    route.fulfill({
      json: {
        data: {
          type: "object",
          properties: {
            channels: {
              type: "array",
              items: { type: "string" },
              // 后端 schemars 对 `#[serde(default)]` 的产物。
              default: [],
              description: "接收简讯的渠道；选项来自已声明的通知渠道列表",
            },
          },
        },
      },
    }),
  );
  await page.route("**/api/notifications/channels", (route) =>
    route.fulfill({
      json: {
        data: [
          { name: "lark", provider: "feishu", enabled: true },
          { name: "dingtalk", provider: "dingtalk", enabled: false },
          { name: "ntfy", provider: "generic", enabled: true },
        ],
      },
    }),
  );
  await page.route("**/api/credentials*", (route) =>
    route.fulfill({
      json: { data: { items: [], total: 0, page: 1, page_size: 100 } },
    }),
  );
});

test("channels 多选下拉展示全部渠道并提交选中的字符串数组", async ({ page }) => {
  interface Body {
    name?: string;
    steps?: { type_id: string; input: Record<string, unknown> }[];
  }
  const posted: Body[] = [];

  await page.route("**/api/pipelines", async (route) => {
    if (route.request().method() === "POST") {
      posted.push(route.request().postDataJSON() as Body);
      await route.fulfill({ json: { data: { id: 1 } } });
      return;
    }
    await route.fallback();
  });

  await page.goto("/pipelines/new");
  // 任务列表只声明 notify.summary，添加步骤即默认该类型。
  await page.getByRole("button", { name: "添加步骤" }).click();

  // channels 渲染为多选下拉；选项含全部渠道（含已停用），标签带 provider 与状态。
  const channels = page.getByLabel("channels");
  await expect(channels).toHaveAttribute("role", "combobox");
  await channels.click();
  await expect(page.locator('.ant-select-item-option[title="lark（feishu，已启用）"]')).toBeVisible();
  await expect(
    page.locator('.ant-select-item-option[title="dingtalk（dingtalk，已停用）"]'),
  ).toBeVisible();
  await expect(page.locator('.ant-select-item-option[title="ntfy（generic，已启用）"]')).toBeVisible();

  // 多选下拉勾选后不收起，可连续勾选。
  await page.locator('.ant-select-item-option[title="lark（feishu，已启用）"]').click();
  await page.locator('.ant-select-item-option[title="ntfy（generic，已启用）"]').click();
  await page.keyboard.press("Escape");

  await page.getByLabel(/名称/).fill("简讯流水线");
  // antd 会在两个汉字之间插空格（"保 存"），用正则匹配。
  await page.getByRole("button", { name: /保\s*存/ }).click();

  await expect.poll(() => posted.length).toBe(1);
  expect(posted[0].name).toBe("简讯流水线");
  expect(posted[0].steps).toHaveLength(1);
  expect(posted[0].steps?.[0].type_id).toBe("notify.summary");
  expect(posted[0].steps?.[0].input.channels).toEqual(["lark", "ntfy"]);
});

test("编辑既有流水线时回显已选渠道（含已停用渠道的标注）", async ({ page }) => {
  await page.route("**/api/pipelines/5", (route) =>
    route.fulfill({
      json: {
        data: {
          id: 5,
          name: "简讯流水线",
          description: null,
          enabled: true,
          steps: [
            { type_id: "notify.summary", input: { channels: ["lark", "dingtalk"] }, enabled: true },
          ],
        },
      },
    }),
  );

  await page.goto("/pipelines/5");

  await expect(
    page.locator('.ant-select-selection-item[title="lark（feishu，已启用）"]'),
  ).toBeVisible();
  await expect(
    page.locator('.ant-select-selection-item[title="dingtalk（dingtalk，已停用）"]'),
  ).toBeVisible();
});
