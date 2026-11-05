// First: profiling builds count renders through React's DevTools hook.
import './utils/renderCensus';
import React from 'react';
import { createRoot } from 'react-dom/client';
import App from './App';
import ErrorBoundary from './components/ui/ErrorBoundary';
import { installFrontendLogBridge } from './utils/frontendLogBridge';
import { registerAppWrites } from './utils/appWrites';
import './styles.css';

installFrontendLogBridge();

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
