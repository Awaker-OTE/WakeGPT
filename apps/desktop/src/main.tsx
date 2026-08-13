import React from "react";
import ReactDOM from "react-dom/client";

async function bootstrap() {
  if (import.meta.env.DEV && new URLSearchParams(window.location.search).has("preview")) {
    const [{ mockIPC }, { previewInvoke }] = await Promise.all([
      import("@tauri-apps/api/mocks"),
      import("./preview"),
    ]);
    mockIPC(previewInvoke, { shouldMockEvents: true });
  }

  const quickCapture = new URLSearchParams(window.location.search).get("surface") === "quick-capture";
  const { default: Surface } = quickCapture
    ? await import("./QuickCapture")
    : await import("./App");
  ReactDOM.createRoot(document.getElementById("root") as HTMLElement).render(
    <React.StrictMode>
      <Surface />
    </React.StrictMode>,
  );
}

void bootstrap();
