import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import "./index.css";
import App from "./App";

declare global {
  interface Window { __CAPL_WASM_READY__?: Promise<unknown> }
}

const root = document.getElementById("root")!;
void (window.__CAPL_WASM_READY__ ?? Promise.reject(new Error("Rust compiler bundle was not requested")))
  .then(() => {
    createRoot(root).render(
      <StrictMode>
        <App />
      </StrictMode>,
    );
  })
  .catch((error: unknown) => {
    root.textContent = `Rust compiler failed to load: ${String(error)}`;
  });
