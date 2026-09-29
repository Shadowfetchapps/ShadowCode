import { Component, type ErrorInfo, type ReactNode } from "react";

/** A bug in the interface must not leave an empty window. Tasks run in the
 * engine, not the page, so reloading the window is safe: running tasks keep
 * going and the conversation comes back from its saved events. */
export class CrashScreen extends Component<
  { children: ReactNode; onReload?: () => void },
  { failed: boolean }
> {
  state = { failed: false };

  static getDerivedStateFromError() {
    return { failed: true };
  }

  componentDidCatch(error: unknown, info: ErrorInfo) {
    console.error("ShadowCode window error", error, info.componentStack);
  }

  render() {
    if (!this.state.failed) return this.props.children;
    return (
      <main className="crash-screen" role="alert">
        <h1>Something went wrong in this window</h1>
        <p>
          Your tasks keep running and your conversations are saved. Reload the
          window to continue.
        </p>
        <button
          type="button"
          className="primary"
          onClick={() => (this.props.onReload ?? (() => location.reload()))()}
        >
          Reload window
        </button>
      </main>
    );
  }
}
