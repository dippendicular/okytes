import React from "react";
import { createRoot } from "react-dom/client";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { App } from "./App";
import { DetailsWindow } from "./DetailsWindow";
import "./style.css";

// Per-download windows are labelled `details-<job UUID>` by the Rust side.
function detailsId(): string | null {
  try {
    const label = getCurrentWindow().label;
    return label.startsWith("details-") ? label.slice("details-".length) : null;
  } catch {
    return null;
  }
}
const root = document.getElementById("root");
const id = detailsId();
if (root)
  createRoot(root).render(
    <React.StrictMode>
      {id ? <DetailsWindow id={id} /> : <App />}
    </React.StrictMode>,
  );
