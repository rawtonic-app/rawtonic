// First: profiling builds count renders through React's DevTools hook.
import './utils/renderCensus';
import React from 'react';
import { createRoot } from 'react-dom/client';
import App from './App';
import ErrorBoundary from './components/ui/ErrorBoundary';
import { installFrontendLogBridge } from './utils/frontendLogBridge';
import { registerAppWrites } from './utils/appWrites';
import { applyInterfaceShade } from './utils/themes';
import './styles.css';

installFrontendLogBridge();

// The second window (second_window.rs) shows only its own page: none of the
// main window's start-up (library, settings writes, QA bridge) runs there.
const isSecondView = (() => {
  try {
    return (
      (window as { __TAURI_INTERNALS__?: { metadata?: { currentWindow?: { label?: string } } } }).__TAURI_INTERNALS__
        ?.metadata?.currentWindow?.label === 'second-view'
    );
  } catch {
    return false;
  }
})();

if (isSecondView) {
  applyInterfaceShade(undefined);
  void Promise.all([import('./components/secondView/SecondView'), import('./i18n')]).then(
    ([{ default: SecondView }]) => {
      createRoot(document.getElementById('root')!).render(
        <React.StrictMode>
          <ErrorBoundary name="Second window" fullScreen>
            <SecondView />
          </ErrorBoundary>
        </React.StrictMode>
      );
    }
  );
} else {
  startMainWindow();
}

function startMainWindow() {
  // Quitting, closing the window and a library switch wait for the app's own
  // write queues (preferences, albums, presets) once every module registered.
  registerAppWrites();

  // QA builds only (VITE_RAWTONIC_QA=1); the import is dropped from other builds.
  if (import.meta.env.VITE_RAWTONIC_QA === '1') {
    void import('./qa/qaControl').then(({ startQaControl }) => startQaControl());
  }

  const root = createRoot(document.getElementById('root')!);
  root.render(
    <React.StrictMode>
      <ErrorBoundary name="Rawtonic" fullScreen>
        <App />
      </ErrorBoundary>
    </React.StrictMode>
  );
}
