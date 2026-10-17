#!/usr/bin/env node
// Load a page in headless Chrome, run JavaScript in it, and print the result
// plus any console errors and CSP violations. For interaction checks the
// DevTools MCP cannot make on a machine with no display server.
//
//   node scratch/ui_probe.mjs <url> <script-file>
//
// The script file is the body of an async function; its return value is
// printed as JSON. `confirm()` dialogs are accepted. Serve the UI first, e.g.
// `target/debug/sharerr preview --bind 127.0.0.1:4877`, from the same network
// namespace (outside the Bash sandbox).

import { spawn } from "node:child_process";
import { mkdtempSync, readFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

const [url, scriptFile] = process.argv.slice(2);
if (!url || !scriptFile) {
  console.error("usage: node scratch/ui_probe.mjs <url> <script-file>");
  process.exit(2);
}
const body = readFileSync(scriptFile, "utf8");

const profile = mkdtempSync(join(tmpdir(), "ui-probe."));
const chrome = spawn(
  "google-chrome-stable",
  ["--headless=new", "--disable-gpu", `--user-data-dir=${profile}`, "--remote-debugging-port=0", "about:blank"],
  { stdio: ["ignore", "ignore", "pipe"] },
);
const browserWs = await new Promise((resolve, reject) => {
  let buf = "";
  chrome.stderr.on("data", (d) => {
    buf += d;
    const m = buf.match(/DevTools listening on (ws:\/\/\S+)/);
    if (m) resolve(m[1]);
  });
  setTimeout(() => reject(new Error("chrome did not start")), 15000);
});

const port = new URL(browserWs).port;
const targets = await (await fetch(`http://127.0.0.1:${port}/json/list`)).json();
const page = targets.find((t) => t.type === "page");
const ws = new WebSocket(page.webSocketDebuggerUrl);
await new Promise((r) => ws.addEventListener("open", r, { once: true }));

let id = 0;
const pending = new Map();
const problems = [];
let loaded;
const loadedOnce = new Promise((r) => (loaded = r));
ws.addEventListener("message", (event) => {
  const msg = JSON.parse(event.data);
  if (msg.id && pending.has(msg.id)) {
    pending.get(msg.id)(msg);
    pending.delete(msg.id);
  } else if (msg.method === "Page.loadEventFired") {
    loaded();
  } else if (msg.method === "Page.javascriptDialogOpening") {
    send("Page.handleJavaScriptDialog", { accept: true });
  } else if (msg.method === "Runtime.exceptionThrown") {
    problems.push(msg.params.exceptionDetails.exception?.description ?? "exception");
  } else if (msg.method === "Log.entryAdded" && msg.params.entry.level === "error") {
    problems.push(msg.params.entry.text);
  } else if (msg.method === "Runtime.consoleAPICalled" && msg.params.type === "error") {
    problems.push(msg.params.args.map((a) => a.value ?? a.description).join(" "));
  }
});
const send = (method, params = {}) =>
  new Promise((resolve) => {
    id += 1;
    pending.set(id, resolve);
    ws.send(JSON.stringify({ id, method, params }));
  });

await Promise.all(["Page.enable", "Runtime.enable", "Log.enable"].map((m) => send(m)));
await send("Page.navigate", { url });
await loadedOnce;

const result = await send("Runtime.evaluate", {
  expression: `(async () => { ${body} })()`,
  awaitPromise: true,
  returnByValue: true,
});
console.log(JSON.stringify({ result: result.result?.result?.value ?? result.result, problems }, null, 2));

ws.close();
chrome.kill();
