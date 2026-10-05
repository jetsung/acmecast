import { Component, type ErrorInfo, type ReactNode } from "react";
import { Button, Result } from "antd";

interface Props {
  children: ReactNode;
}

interface State {
  error: Error | null;
}

/** 全局渲染错误兜底：避免单个页面异常白屏整个控制台（design D7）。 */
export class ErrorBoundary extends Component<Props, State> {
  state: State = { error: null };

  static getDerivedStateFromError(error: Error): State {
    return { error };
  }

  componentDidCatch(error: Error, info: ErrorInfo) {
    console.error("渲染错误", error, info.componentStack);
  }

  render() {
    if (this.state.error) {
      return (
        <Result
          status="error"
          title="页面出现异常"
          subTitle={this.state.error.message}
          extra={
            <Button type="primary" onClick={() => window.location.assign("/")}>
              返回首页
            </Button>
          }
        />
      );
    }
    return this.props.children;
  }
}
