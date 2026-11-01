// Dashboard. The session token lives in sessionStorage (cleared when the tab
// closes) and is sent as a bearer header, so there are no cookies to forge.
"use strict";
const $ = (id) => document.getElementById(id);
let token = null;
try {
  token = sessionStorage.getItem("okytes-token");
} catch {
  /* storage blocked: sign-in lasts for this page only */
}
let devices = [];
let history = [];

// OAuth callbacks deliver the session or an error in the fragment.
const fragment = new URLSearchParams(location.hash.slice(1));
if (fragment.get("token")) save(fragment.get("token"));
if (fragment.get("error")) alertText(fragment.get("error"));
if (fragment.get("notice")) alertText(fragment.get("notice"), true);
// Reset tokens stay in memory only; they are cleared from the address bar below.
const resetToken = fragment.get("reset");
if (location.hash) history_replace();
function history_replace() {
  window.history.replaceState(null, "", location.pathname);
}

function save(value) {
  token = value;
  try {
    if (value) sessionStorage.setItem("okytes-token", value);
    else sessionStorage.removeItem("okytes-token");
  } catch {
    /* ignore */
  }
}
function alertText(message, notice = false) {
  const box = $("alert");
  box.textContent = message || "";
  box.hidden = !message;
  box.className = notice ? "notice" : "";
}
async function call(path, options = {}) {
  const response = await fetch(path, {
    ...options,
    headers: {
      "content-type": "application/json",
      ...(token ? { authorization: `Bearer ${token}` } : {}),
    },
  });
  if (response.status === 401 && token) {
    save(null);
    show();
  }
  const body = response.status === 204 ? null : await response.json().catch(() => null);
  if (!response.ok) throw new Error(body?.error || `Request failed (${response.status})`);
  return body;
}
function bytes(value) {
  if (!Number.isFinite(value) || value < 0) return "—";
  const unit = Math.min(4, Math.floor(Math.log2(Math.max(1, value)) / 10));
  return `${(value / 1024 ** unit).toFixed(unit ? 1 : 0)} ${["B", "KiB", "MiB", "GiB", "TiB"][unit]}`;
}
function when(seconds) {
  return seconds ? new Date(seconds * 1000).toLocaleString() : "never";
}
// Builds table rows with textContent only; nothing from the server is parsed as HTML.
function table(element, headings, rows) {
  element.replaceChildren();
  const head = element.insertRow();
  for (const text of headings) {
    const th = document.createElement("th");
    th.textContent = text;
    head.append(th);
  }
  if (!rows.length) {
    const cell = element.insertRow().insertCell();
    cell.colSpan = headings.length;
    cell.className = "muted";
    cell.textContent = "Nothing here yet.";
  }
  for (const row of rows) {
    const tr = element.insertRow();
    for (const value of row) {
      const cell = tr.insertCell();
      if (value instanceof Node) cell.append(value);
      else cell.textContent = value ?? "";
    }
  }
}
function button(label, action) {
  const b = document.createElement("button");
  b.textContent = label;
  b.addEventListener("click", () => action().catch((e) => alertText(e.message)));
  return b;
}

async function show() {
  if (resetToken && !token) {
    $("signin").hidden = true;
    $("reset").hidden = false;
    return;
  }
  $("reset").hidden = true;
  const signedIn = !!token;
  $("signin").hidden = signedIn;
  $("dashboard").hidden = !signedIn;
  $("logout").hidden = !signedIn;
  if (!signedIn) {
    $("who").textContent = "";
    const config = await call("/v1/config").catch(() => ({}));
    $("owner-note").hidden = !config.owner_setup;
    $("forgot").hidden = !config.email;
    const providers = $("providers");
    providers.replaceChildren();
    for (const [key, label] of [["github", "Continue with GitHub"], ["google", "Continue with Google"]]) {
      if (!config[key]) continue;
      const link = document.createElement("a");
      link.className = "button";
      link.href = `/v1/auth/${key}/start`;
      link.textContent = label;
      providers.append(link);
    }
    return;
  }
  const me = await call("/v1/me");
  $("who").textContent = me.user.email;
  await refresh();
}
async function refresh() {
  const [d, q, h, a] = await Promise.all([
    call("/v1/devices"),
    call("/v1/queue"),
    call("/v1/history?limit=500"),
    call("/v1/analytics"),
  ]);
  devices = d.devices;
  history = h.entries;
  const name = (id) => devices.find((x) => x.id === id)?.name ?? (id ? "removed device" : "any");
  const target = $("target");
  const chosen = target.value;
  target.replaceChildren(new Option("First device that checks in", ""));
  for (const device of devices.filter((x) => x.kind === "desktop"))
    target.append(new Option(device.name, device.id));
  target.value = chosen;
  table($("devices"), ["Name", "Type", "Paired", "Last seen", ""], devices.map((x) => [
    x.name, x.kind, when(x.created_at), when(x.last_seen),
    button("Remove", async () => {
      if (!confirm(`Remove ${x.name}? It will need a new pairing code.`)) return;
      await call(`/v1/devices/${x.id}`, { method: "DELETE" });
      await refresh();
    }),
  ]));
  table($("queue"), ["Link", "Device", "State", "Added", ""], q.items.map((x) => [
    x.filename || x.url, name(x.claimed_by || x.target_device),
    x.error ? `${x.state}: ${x.error}` : x.state, when(x.created_at),
    x.state === "claimed" ? "" : button("Remove", async () => {
      await call(`/v1/queue/${x.id}`, { method: "DELETE" });
      await refresh();
    }),
  ]));
  renderHistory();
  const max = Math.max(1, ...a.days.map((x) => x.bytes));
  const bars = $("analytics");
  bars.replaceChildren();
  for (const day of [...a.days].reverse()) {
    const bar = document.createElement("div");
    bar.style.height = `${Math.max(2, (day.bytes / max) * 100)}%`;
    bar.title = `${day.day}: ${day.completed} completed, ${bytes(day.bytes)}`;
    bars.append(bar);
  }
  if (!a.days.length) bars.textContent = "No completed downloads synced yet.";
  $("states").textContent = a.states.map((x) => `${x.state}: ${x.count} (${bytes(x.bytes)})`).join(" · ");
}
function renderHistory() {
  const filter = $("search").value.toLowerCase();
  table($("history"), ["File", "Device", "State", "Size", "Updated"], history
    .filter((x) => `${x.name} ${x.source}`.toLowerCase().includes(filter))
    .map((x) => [x.name, x.device_name, x.state, x.total === null ? bytes(x.bytes) : bytes(x.total), new Date(x.updated_at).toLocaleString()]));
}

$("credentials").addEventListener("submit", (event) => {
  event.preventDefault();
  auth("/v1/auth/login");
});
$("register").addEventListener("click", () => {
  if ($("credentials").reportValidity()) auth("/v1/auth/register");
});
async function auth(path) {
  try {
    const result = await call(path, {
      method: "POST",
      body: JSON.stringify({ email: $("email").value, password: $("password").value }),
    });
    $("password").value = "";
    if (result.verify) {
      alertText("Account created. Open the confirmation link we emailed you, then sign in.", true);
      return;
    }
    save(result.token);
    alertText("");
    await show();
  } catch (e) {
    alertText(e.message);
  }
}
$("forgot").addEventListener("click", async () => {
  const email = $("email").value.trim();
  if (!email) return alertText("Enter your email address first.");
  try {
    await call("/v1/auth/forgot", { method: "POST", body: JSON.stringify({ email }) });
    alertText("If that account exists, a reset link is on its way. It expires in one hour.", true);
  } catch (e) {
    alertText(e.message);
  }
});
$("reset-form").addEventListener("submit", async (event) => {
  event.preventDefault();
  try {
    await call("/v1/auth/reset", {
      method: "POST",
      body: JSON.stringify({ token: resetToken, password: $("new-password").value }),
    });
    window.location.replace("/#notice=Password%20changed.%20Sign%20in%20with%20your%20new%20password.");
    window.location.reload();
  } catch (e) {
    alertText(e.message);
  }
});
$("logout").addEventListener("click", async () => {
  await call("/v1/auth/logout", { method: "POST" }).catch(() => {});
  save(null);
  await show();
});
$("enqueue").addEventListener("submit", async (event) => {
  event.preventDefault();
  try {
    await call("/v1/queue", {
      method: "POST",
      body: JSON.stringify({
        url: $("url").value.trim(),
        filename: $("filename").value.trim() || null,
        target_device: $("target").value || null,
      }),
    });
    $("url").value = "";
    $("filename").value = "";
    alertText("");
    await refresh();
  } catch (e) {
    alertText(e.message);
  }
});
$("pair").addEventListener("click", async () => {
  try {
    const result = await call("/v1/devices/pair-code", { method: "POST" });
    const box = $("pair-code");
    box.hidden = false;
    box.textContent = `Pairing code: ${result.code} — enter it in Okytes → Settings → Cloud within 10 minutes. Anyone with this code can pair a device to your account.`;
  } catch (e) {
    alertText(e.message);
  }
});
$("refresh").addEventListener("click", () => refresh().catch((e) => alertText(e.message)));
$("search").addEventListener("input", renderHistory);
show().catch((e) => alertText(e.message));
setInterval(() => {
  if (token && !document.hidden) refresh().catch(() => {});
}, 10000);
