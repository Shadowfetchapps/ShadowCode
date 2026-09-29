import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import App from "./App";
import { CrashScreen } from "./components/CrashScreen";
import { RemoteGate } from "./components/RemoteGate";
import { applyInitialTheme } from "./hooks/useTheme";
import { isRemote } from "./lib/transport";
import "./tokens.css";
import "./index.css";
import "./workspace.css";
import "./components.css";
import "./review.css";
import "./tasks.css";
import "./remote.css";

// The last saved appearance (else the system theme) until the config loads.
applyInitialTheme();

createRoot(document.getElementById("root")!).render(
  <StrictMode>
    <CrashScreen>
      {isRemote() ? (
        <RemoteGate>
          <App />
        </RemoteGate>
      ) : (
        <App />
      )}
    </CrashScreen>
  </StrictMode>,
);
