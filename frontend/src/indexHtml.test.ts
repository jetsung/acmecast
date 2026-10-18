/**
 * Favicon / PWA 声明回归：`index.html` 的 `<head>` 必须声明 favicon 与
 * manifest，且 `href` 为 `/` 绝对路径——否则深层路由（`/pipelines` 等）下
 * 图标与可安装判定会失效。直接对入口文件内容断言，与路由无关。
 */
import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { describe, expect, it } from "vitest";

const html = readFileSync(resolve(__dirname, "../index.html"), "utf-8");

describe("index.html favicon 与 PWA 声明", () => {
  const requiredLinks: Array<[string, string]> = [
    ['icon', '/favicon.ico'],
    ['apple-touch-icon', '/apple-touch-icon.png'],
    ['manifest', '/site.webmanifest'],
  ];

  it.each(requiredLinks)('存在 <link rel="%s" href="%s">', (rel, href) => {
    const pattern = new RegExp(
      `<link[^>]*rel="${rel}"[^>]*href="${href}"`,
    );
    expect(html).toMatch(pattern);
  });

  it("声明 PNG favicon 32x32 与 16x16", () => {
    expect(html).toMatch(
      '<link rel="icon" type="image/png" sizes="32x32" href="/favicon-32x32.png"',
    );
    expect(html).toMatch(
      '<link rel="icon" type="image/png" sizes="16x16" href="/favicon-16x16.png"',
    );
  });

  it("声明品牌 theme-color #4f63f2", () => {
    expect(html).toMatch('<meta name="theme-color" content="#4f63f2"');
  });

  it("所有 favicon/manifest href 均为 / 开头绝对路径", () => {
    const hrefs = [...html.matchAll(/<link[^>]*href="([^"]*)"/g)].map(
      (m) => m[1],
    );
    expect(hrefs.length).toBeGreaterThan(0);
    for (const href of hrefs) {
      expect(href.startsWith("/")).toBe(true);
    }
  });
});
