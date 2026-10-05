import { cleanup } from "@testing-library/react";
import { afterEach } from "vitest";

// jsdom 没有实现 antd 依赖的若干浏览器 API，渲染组件前补上。
// 只补缺失的，不覆盖 jsdom 自带的实现。

if (typeof window.matchMedia !== "function") {
  window.matchMedia = (query: string): MediaQueryList =>
    ({
      matches: false,
      media: query,
      onchange: null,
      addListener: () => {},
      removeListener: () => {},
      addEventListener: () => {},
      removeEventListener: () => {},
      dispatchEvent: () => false,
    }) as unknown as MediaQueryList;
}

// antd 内部用 rc-resize-observer，会实例化 ResizeObserver。
if (typeof globalThis.ResizeObserver !== "function") {
  globalThis.ResizeObserver = class {
    observe() {}
    unobserve() {}
    disconnect() {}
  } as unknown as typeof ResizeObserver;
}

// 每个用例后卸载已渲染的组件，避免 DOM 与全局状态互相串味。
afterEach(() => {
  cleanup();
});
