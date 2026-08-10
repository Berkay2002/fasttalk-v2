import React from "react";
import ReactDOM from "react-dom/client";

if (import.meta.env.DEV && new URLSearchParams(window.location.search).has("mock")) {
  const { installDevMock } = await import("./devMock");
  installDevMock();
}

const { default: App } = await import("./App");

ReactDOM.createRoot(document.getElementById("root") as HTMLElement).render(
  <React.StrictMode>
    <App />
  </React.StrictMode>,
);
