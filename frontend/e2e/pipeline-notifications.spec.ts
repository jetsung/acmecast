/**
 * 流水线「通知」多选（运行结束简讯渠道）的端到端验证：
 * - 「通知」下拉只列通知类凭据（`notify.feishu` / `notify.dingtalk` /
 *   `notify.generic` / `notify.email`），下拉列表文字为「名称（id=N）— 渠道类型」，
 *   选中后选择框内标签为「名称（id=N）」（不含渠道类型）；
 * - 保存时提交数字 ID 数组（`notification_channels`）；
 * - 重新打开编辑页回显选中状态；复制流水线时预填。
 *
 * 后端不参与，/api 全部用 route 喂假数据。
 */
import { expect, test } from "@playwright/test";

const TASK_TYPE = "e2e.json";

const CREDENTIALS = [
  { id: 7, name: "le-prod", type_id: "acme.account", remark: "" },
  { id: 8, name: "some-dns", type_id: "cloud.cloudflare", remark: "" },
  { id: 50, name: "ops-feishu", type_id: "notify.feishu", remark: "" },
  { id: 51, name: "ops-mail", type_id: "notify.email", remark: "" },
];

const CREDENTIAL_TYPES = [
  { type_id: "notify.feishu", display_name: "飞书机器人", schema: {} },
  { type_id: "notify.email", display_name: "邮件", schema: {} },
  { type_id: "cloud.cloudflare", display_name: "Cloudflare", schema: {} },
];

const FEISHU_LABEL = "ops-feishu（id=50）";
const MAIL_LABEL = "ops-mail（id=51）";
/** 下拉列表展开时的完整文字（含渠道类型，optionRender 渲染）。 */
const FEISHU_FULL_LABEL = `${FEISHU_LABEL}— 飞书机器人`;
const MAIL_FULL_LABEL = `${MAIL_LABEL}— 邮件`;

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
    route.fulfill({ json: { data: [{ type_id: TASK_TYPE, display_name: "E2E JSON" }] } }),
  );
  await page.route("**/api/tasks/*/schema", (route) =>
    route.fulfill({
      json: { data: { type: "object", properties: { note: { type: "string" } } } },
    }),
  );
  await page.route("**/api/credentials*", (route) =>
    route.fulfill({
      json: { data: { items: CREDENTIALS, total: CREDENTIALS.length, page: 1, page_size: 100 } },
    }),
  );
  await page.route("**/api/credential-types", (route) =>
    route.fulfill({ json: { data: CREDENTIAL_TYPES } }),
  );
});

test("通知下拉只列通知类凭据，保存时提交 ID 数组", async ({ page }) => {
  interface Body {
    name?: string;
    notification_channels?: number[];
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
  await page.getByLabel(/名称/).fill("通知流水线");

  // 「通知」多选：只列通知类凭据，非通知类（账号 / DNS）不出现。
  const channels = page.getByLabel("通知");
  await expect(channels).toHaveAttribute("role", "combobox");
  await channels.click();
  // 下拉列表文字仍为完整格式（option 的 title 取自短格式 label，内容由 optionRender 渲染）。
  const feishuOption = page.locator(`.ant-select-item-option[title="${FEISHU_LABEL}"]`);
  const mailOption = page.locator(`.ant-select-item-option[title="${MAIL_LABEL}"]`);
  await expect(feishuOption).toBeVisible();
  await expect(feishuOption).toContainText(FEISHU_FULL_LABEL);
  await expect(mailOption).toBeVisible();
  await expect(mailOption).toContainText(MAIL_FULL_LABEL);
  await expect(page.locator('.ant-select-item-option[title="le-prod（id=7）"]')).toHaveCount(0);
  await expect(page.locator('.ant-select-item-option[title="some-dns（id=8）"]')).toHaveCount(0);

  // 多选下拉勾选后不收起，可连续勾选。
  await feishuOption.click();
  await mailOption.click();
  await page.keyboard.press("Escape");

  // 选中后标签为短格式，不含渠道类型。
  const feishuTag = page.locator(`.ant-select-selection-item[title="${FEISHU_LABEL}"]`);
  await expect(feishuTag).toBeVisible();
  await expect(feishuTag).not.toContainText("—");
  await expect(page.locator(`.ant-select-selection-item[title="${MAIL_LABEL}"]`)).toBeVisible();

  // antd 会在两个汉字之间插空格（"保 存"），用正则匹配。
  await page.getByRole("button", { name: /保\s*存/ }).click();

  await expect.poll(() => posted.length).toBe(1);
  expect(posted[0].name).toBe("通知流水线");
  expect(posted[0].notification_channels).toEqual([50, 51]);
});

test("编辑既有流水线时回显已选通知渠道", async ({ page }) => {
  await page.route("**/api/pipelines/5", (route) =>
    route.fulfill({
      json: {
        data: {
          id: 5,
          name: "通知流水线",
          description: null,
          enabled: true,
          notification_channels: [50, 51],
          steps: [{ type_id: TASK_TYPE, input: { note: "kept" }, enabled: true }],
        },
      },
    }),
  );

  await page.goto("/pipelines/5");

  // 选中项标签为短格式，不含渠道类型。
  const feishuTag = page.locator(`.ant-select-selection-item[title="${FEISHU_LABEL}"]`);
  await expect(feishuTag).toBeVisible();
  await expect(feishuTag).toContainText(FEISHU_LABEL);
  await expect(feishuTag).not.toContainText("—");
  await expect(page.locator(`.ant-select-selection-item[title="${MAIL_LABEL}"]`)).toBeVisible();
});

test("复制流水线时预填通知渠道", async ({ page }) => {
  // 列表请求带 query（`?page=1&page_size=15`），glob 需用 `*` 吞掉 query；
  // `*` 不匹配 `/`，因此不会抢后注册的 `**/api/pipelines/5` 详情路由。
  await page.route("**/api/pipelines*", (route) =>
    route.fulfill({
      json: {
        data: {
          items: [{ id: 5, name: "通知流水线", enabled: true }],
          total: 1,
          page: 1,
          page_size: 100,
        },
      },
    }),
  );
  await page.route("**/api/pipelines/5", (route) =>
    route.fulfill({
      json: {
        data: {
          id: 5,
          name: "通知流水线",
          description: null,
          enabled: true,
          notification_channels: [50],
          steps: [{ type_id: TASK_TYPE, input: { note: "kept" }, enabled: true }],
        },
      },
    }),
  );

  await page.goto("/pipelines");
  // antd 会在两个汉字之间插空格（"复 制"），用正则匹配。
  await page.getByRole("button", { name: /复\s*制/ }).click();

  // 新建页预填源流水线的通知渠道：选中项同样只显示短格式。
  const feishuTag = page.locator(`.ant-select-selection-item[title="${FEISHU_LABEL}"]`);
  await expect(feishuTag).toBeVisible();
  await expect(feishuTag).not.toContainText("—");
});
