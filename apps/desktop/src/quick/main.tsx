import { createRoot } from "react-dom/client";
import { Quick } from "./Quick";
import "./quick.css";

const root = document.getElementById("root");
if (root) createRoot(root).render(<Quick />);
