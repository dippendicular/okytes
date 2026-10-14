import { api } from "./compat";
// Alt-click bypasses automatic capture for this exact link for five seconds.
document.addEventListener(
  "click",
  (event) => {
    if (!event.altKey) return;
    const link =
      event.target instanceof Element
        ? event.target.closest<HTMLAnchorElement>("a[href]")
        : null;
    if (link)
      void api.runtime
        .sendMessage({ kind: "bypass", url: link.href })
        .catch(() => {});
  },
  true,
);
