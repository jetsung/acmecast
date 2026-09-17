import { create } from "zustand";
import { persist } from "zustand/middleware";

export interface AuthState {
  token: string | null;
  username: string | null;
  login: (token: string, username: string) => void;
  logout: () => void;
}

/**
 * 登录态：令牌持久化到 localStorage（刷新恢复），401 时统一走 logout。
 * XSS 风险已接受——单管理员内网控制台，令牌 TTL 12 小时（design D3）。
 */
export const useAuthStore = create<AuthState>()(
  persist(
    (set) => ({
      token: null,
      username: null,
      login: (token, username) => set({ token, username }),
      logout: () => set({ token: null, username: null }),
    }),
    { name: "acmecast-auth" },
  ),
);
