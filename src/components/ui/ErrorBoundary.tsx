import { Component, type ErrorInfo, type ReactNode } from "react";
import i18n from "i18next";

interface Props {
  /** What failed, for the message ("Library tools", "Rawtonic"). */
  name: string;
  children: ReactNode;
  /** Whole-window fallback with a reload instead of a panel retry. */
  fullScreen?: boolean;
}

/** Contains a render error to one panel, so a failing tool does not blank the
 * whole window; edits are already saved in the session. */
export default class ErrorBoundary extends Component<Props, { error: Error | null }> {
  state = { error: null as Error | null };

  static getDerivedStateFromError(error: Error) {
    return { error };
  }

  componentDidCatch(error: Error, info: ErrorInfo) {
    // The frontend log bridge forwards this to the app log.
    console.error(`${this.props.name} failed:`, error, info.componentStack);
  }

  render() {
    const { error } = this.state;
    if (!error) return this.props.children;
    return (
      <div className={`error-boundary ${this.props.fullScreen ? "is-full-screen" : ""}`} role="alert">
        <strong>{i18n.t("ui.errorBoundary.stopped", { name: this.props.name })}</strong>
        <span>{error.message}</span>
        <small>{i18n.t("ui.errorBoundary.editsSafe")}</small>
        <button
          onClick={() => (this.props.fullScreen ? window.location.reload() : this.setState({ error: null }))}
        >
          {this.props.fullScreen ? i18n.t("ui.errorBoundary.reload") : i18n.t("ui.errorBoundary.tryAgain")}
        </button>
      </div>
    );
  }
}
