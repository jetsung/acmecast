/**
 * 新建/编辑流水线页（/pipelines/new）的端到端验证。
 *
 * 覆盖两类只能在真实浏览器里验证、或必须在真实页面走通的场景：
 * - 布局：jsdom 没有布局引擎，antd autoSize 的行高在那里恒为单行，量不出 minRows；
 * - 保存往返：任务输入经表单 store → 解析 → POST body 的完整链路。
 * 后端不参与，/api 全部用 route 喂假数据。
 */
import { expect, test } from "@playwright/test";

const TASK_TYPE = "e2e.json";

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
  await page.route("**/api/credentials*", (route) =>
    route.fulfill({
      json: {
        data: {
          items: [
            { id: 7, name: "le-prod", type_id: "acme.account" },
            { id: 8, name: "some-dns", type_id: "cloudflare" },
            { id: 9, name: "ali-dns", type_id: "aliyun" },
          ],
          total: 3,
          page: 1,
          page_size: 100,
        },
      },
    }),
  );
  await page.route("**/api/tasks/*/schema", (route) =>
    route.fulfill({
      json: {
        data: {
          type: "object",
          required: ["rules"],
          properties: {
            rules: {
              type: "array",
              items: { type: "object" },
              description: "要下发的规则列表",
            },
            account_credential_id: { type: "integer", description: "ACME 账号凭据标识。" },
            dns_credential_id: { type: "integer", description: "DNS 提供商凭据标识。" },
            dns_provider: { type: "string", description: "DNS 提供商标识。" },
          },
        },
      },
    }),
  );
});

/** 打开流水线编辑器并添加一个带 JSON 对象数组输入的步骤。 */
async function openJsonEditor(page: import("@playwright/test").Page) {
  await page.goto("/pipelines/new");
  await page.getByRole("button", { name: "添加步骤" }).click();

  // 显式选任务类型（不依赖任务列表已就绪时的默认值）。
  // 用 first()：任务类型是步骤里第一个下拉，任务输入自己也可能有下拉。
  await page.getByRole("combobox").first().click();
  await page.locator('.ant-select-item-option[title="E2E JSON"]').click();

  const editor = page.getByLabel("rules");
  await expect(editor).toBeVisible();
  return editor;
}

test("JSON 编辑器最小高度约等于 6 行", async ({ page }) => {
  const editor = await openJsonEditor(page);

  const metrics = await editor.evaluate((el) => {
    const style = getComputedStyle(el);
    const minHeight = Number.parseFloat(style.minHeight);
    return {
      lineHeight: Number.parseFloat(style.lineHeight),
      minHeight,
      height: el.getBoundingClientRect().height,
    };
  });

  expect(metrics.lineHeight).toBeGreaterThan(0);
  // 最小高度应落在大约 6 行（行高不含 padding，所以用比例区间而非精确相等）。
  expect(metrics.minHeight / metrics.lineHeight).toBeGreaterThan(5.5);
  expect(metrics.minHeight / metrics.lineHeight).toBeLessThan(8);
  expect(metrics.height).toBeGreaterThanOrEqual(metrics.minHeight - 1);
});

test("失焦后把 minified JSON 重排为缩进格式", async ({ page }) => {
  const editor = await openJsonEditor(page);

  await editor.fill('[{"key":"v"}]');
  await editor.blur();

  await expect(editor).toHaveValue('[\n  {\n    "key": "v"\n  }\n]');
});

test("必填任务输入为空时提示必填，而不是字段说明", async ({ page }) => {
  await openJsonEditor(page);

  // antd 会在两个汉字之间插空格（"保 存"），用正则匹配。
  await page.getByRole("button", { name: /保\s*存/ }).click();

  const error = page.getByText("请填写 rules");
  await expect(error).toBeVisible();
  // 说明文字只作 tooltip，不该出现在红字报错里（否则像「填了还报错」）。
  await expect(page.locator(".ant-form-item-explain-error", { hasText: "要下发的规则列表" })).toHaveCount(0);
});

test("按字段语义选控件：凭据按类型过滤、DNS 提供商为固定枚举", async ({ page }) => {
  await openJsonEditor(page);

  const account = page.locator('[id$="_account_credential_id"]');
  await expect(account).toHaveAttribute("role", "combobox");
  await account.click();
  await expect(page.locator('.ant-select-item-option[title="le-prod（id=7）"]')).toBeVisible();
  // cloudflare 凭据不是 ACME 账号，不该出现在这里。
  await expect(page.locator('.ant-select-item-option[title="some-dns（id=8）"]')).toBeHidden();
  await page.keyboard.press("Escape");

  const dns = page.locator('[id$="_dns_credential_id"]');
  await expect(dns).toHaveAttribute("role", "combobox");
  await dns.click();
  await expect(page.locator('.ant-select-item-option[title="ali-dns（id=9）"]')).toBeVisible();
  // acme.account 的凭据不是 DNS 提供商，不该出现在这里。
  // 用可见性判断：antd 关闭过的下拉 DOM 仍在，只是隐藏。
  await expect(page.locator('.ant-select-item-option[title="le-prod（id=7）"]')).toBeHidden();
  await page.keyboard.press("Escape");

  // dns_provider 是固定枚举，选项不来自凭据列表。
  const provider = page.locator('[id$="_dns_provider"]');
  await expect(provider).toHaveAttribute("role", "combobox");
  await provider.click();
  await expect(page.locator('.ant-select-item-option[title="Cloudflare"]')).toBeVisible();
  await expect(page.locator('.ant-select-item-option[title="阿里云"]')).toBeVisible();
});

test("保存时把任务输入按类型解析后提交", async ({ page }) => {
  interface Body {
    name?: string;
    steps?: { type_id: string; input: unknown }[];
  }
  const posted: Body[] = [];

  // 覆盖兜底 404：POST /api/pipelines 记录请求体并返回成功；其余方法交回兜底。
  await page.route("**/api/pipelines", async (route) => {
    if (route.request().method() === "POST") {
      posted.push(route.request().postDataJSON() as Body);
      await route.fulfill({ json: { data: { id: 1 } } });
      return;
    }
    await route.fallback();
  });

  const editor = await openJsonEditor(page);
  await page.getByLabel(/名称/).fill("e2e 流水线");
  await editor.fill('[{"key":"v"}]');

  // antd 会在两个汉字之间插空格（"保 存"），用正则匹配。
  await page.getByRole("button", { name: /保\s*存/ }).click();

  await expect.poll(() => posted.length).toBe(1);
  expect(posted[0].name).toBe("e2e 流水线");
  expect(posted[0].steps).toHaveLength(1);
  expect(posted[0].steps?.[0].type_id).toBe(TASK_TYPE);
  // 文本框里是 JSON 字符串，提交的必须是解析后的结构。
  expect(posted[0].steps?.[0].input).toEqual({ rules: [{ key: "v" }] });
});
