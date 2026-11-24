// Thin companion extension for the VS Code family (zero compression logic; talks only to local `tinkr`).
const vscode = require("vscode");
const { execFile } = require("child_process");
const fs = require("fs");
const os = require("os");
const path = require("path");

function findTinkrBin() {
  const isWin = process.platform === "win32";
  const exe = isWin ? "tinkr.exe" : "tinkr";
  const candidates = [];
  if (isWin) {
    if (process.env.LOCALAPPDATA) {
      candidates.push(
        path.join(process.env.LOCALAPPDATA, "tinkr", "bin", exe),
        path.join(process.env.LOCALAPPDATA, "Microsoft", "WinGet", "Links", exe)
      );
    }
    candidates.push(path.join(os.homedir(), ".cargo", "bin", exe));
  } else {
    candidates.push(
      path.join(os.homedir(), ".local", "bin", exe),
      path.join(os.homedir(), ".cargo", "bin", exe),
      "/opt/homebrew/bin/tinkr",
      "/usr/local/bin/tinkr"
    );
  }
  for (const p of candidates) {
    try {
      if (fs.existsSync(p)) return p;
    } catch {}
  }
  return "tinkr";
}

function runTinkr(args, cb) {
  const bin = findTinkrBin();
  execFile(bin, args, { timeout: 5000 }, (err, stdout, stderr) => {
    if (err && bin !== "tinkr") {
      return execFile("tinkr", args, { timeout: 5000 }, (e2, out2, err2) => cb(e2, out2, err2));
    }
    cb(err, stdout, stderr);
  });
}

function fmtNum(n) {
  return Number(n || 0).toLocaleString("en-US");
}

function prettyLayer(name) {
  const labels = {
    read_less: "Read-Less (CLI/MCP)",
    send_less: "Send-Less (Proxy)",
    say_less: "Say-Less (Rules · est)",
  };
  return labels[name] || name;
}

function prettyProvider(name) {
  const labels = {
    gemini: "Gemini",
    anthropic: "Anthropic",
    openai: "OpenAI",
    vertex: "Vertex AI",
    azure_openai: "Azure OpenAI",
    copilot: "Copilot",
    openai_compatible: "OpenAI-Compat",
    bedrock: "Bedrock",
  };
  return labels[name] || name;
}

function progressBarSvg(percent, width = 268, height = 5) {
  const pct = Math.max(0, Math.min(100, Number(percent) || 0));
  const fillW = pct > 0 ? Math.max(4, Math.round((width * pct) / 100)) : 0;
  const r = Math.floor(height / 2);
  const svg =
    `<svg xmlns="http://www.w3.org/2000/svg" width="${width}" height="${height}" viewBox="0 0 ${width} ${height}">` +
    `<rect width="${width}" height="${height}" rx="${r}" fill="#3b3b3b"/>` +
    (fillW > 0
      ? `<rect width="${fillW}" height="${height}" rx="${r}" fill="#3794ff"/>`
      : "") +
    `</svg>`;
  return `data:image/svg+xml;base64,${Buffer.from(svg).toString("base64")}`;
}

function kvRows(entries, labelFn) {
  return entries
    .map(([k, v]) => {
      const pct = v.before > 0 ? ((v.saved * 100) / v.before).toFixed(1) : "0.0";
      const label = labelFn ? labelFn(k) : k;
      return (
        `<tr>` +
        `<td width="136" align="left">${label}</td>` +
        `<td width="132" align="right"><span style="color:#9da5b4;"><b>${fmtNum(v.saved)}</b> tok · ${pct}%</span></td>` +
        `</tr>`
      );
    })
    .join("");
}

function buildTooltip(s) {
  const md = new vscode.MarkdownString("", true);
  md.isTrusted = true;
  md.supportHtml = true;
  md.supportThemeIcons = true;

  const pct = Number(s.percent || 0);
  const pctStr = pct.toFixed(1);
  const barUri = progressBarSvg(pct);

  // Header row (Copilot-style title + right-aligned icon actions)
  md.appendMarkdown(
    `<table><tr>` +
      `<td width="180" align="left">$(sparkle) <b>Tinkr</b> <span style="color:#8b949e;">Active</span></td>` +
      `<td width="88" align="right"><a href="command:tinkr.refresh" title="Refresh Savings">$(refresh)</a>&nbsp;&nbsp;<a href="command:tinkr.doctor" title="Run Tinkr Doctor">$(settings-gear)</a></td>` +
      `</tr></table>\n\n`
  );
  md.appendMarkdown(`---\n\n`);

  // Primary metric block + progress bar
  const countMeta =
    Number(s.estimated_events || 0) > 0
      ? `${fmtNum(s.events)} events · ${fmtNum(s.measured_events)} exact · ${fmtNum(s.estimated_events)} est`
      : `${fmtNum(s.events)} events · ${fmtNum(s.measured_events)} measured`;
  md.appendMarkdown(
    `<table><tr>` +
      `<td width="104" align="left"><b>Tokens Saved</b></td>` +
      `<td width="164" align="right"><span style="color:#8b949e;">${countMeta}</span></td>` +
      `</tr></table>\n\n`
  );
  md.appendMarkdown(
    `### **${pctStr}%** <span style="color:#8b949e;">saved · ${fmtNum(s.saved)} of ${fmtNum(s.before)} tok</span>\n\n` +
      `<img src="${barUri}" width="268" height="5" />\n\n`
  );

  // Clean 2-column breakdown: Layers
  const layers = Object.entries(s.by_layer || {})
    .filter(([, v]) => v && (v.saved > 0 || v.events > 0))
    .sort((a, b) => (b[1].saved || 0) - (a[1].saved || 0))
    .slice(0, 4);

  // Providers (only show real providers, hide "none")
  const providers = Object.entries(s.by_provider || {})
    .filter(([k, v]) => k !== "none" && v && (v.saved > 0 || v.events > 0))
    .sort((a, b) => (b[1].saved || 0) - (a[1].saved || 0))
    .slice(0, 4);

  // Top tools
  const tools = Object.entries(s.by_tool || {})
    .filter(([, v]) => v && (v.saved > 0 || v.events > 0))
    .sort((a, b) => (b[1].saved || 0) - (a[1].saved || 0))
    .slice(0, 5);

  if (layers.length > 0) {
    md.appendMarkdown(`---\n\n`);
    md.appendMarkdown(`<table>${kvRows(layers, prettyLayer)}</table>\n\n`);
  }

  if (providers.length > 0) {
    md.appendMarkdown(`---\n\n`);
    md.appendMarkdown(`<table>${kvRows(providers, prettyProvider)}</table>\n\n`);
  }

  if (tools.length > 0) {
    md.appendMarkdown(`---\n\n`);
    md.appendMarkdown(
      `<table><tr>` +
        `<td width="136" align="left"><b>Top Tools</b></td>` +
        `<td width="132" align="right"><span style="color:#8b949e;">${fmtNum(s.before)} → ${fmtNum(s.after)}</span></td>` +
        `</tr>${kvRows(tools, (k) => `<code>${k}</code>`)}</table>\n\n`
    );
  }

  // Footer action row
  md.appendMarkdown(
    `---\n\n` +
      `<table><tr>` +
      `<td width="100" align="left"><a href="command:tinkr.doctor">Run Doctor</a></td>` +
      `<td width="84" align="center"><a href="command:tinkr.overlap">Overlap</a></td>` +
      `<td width="84" align="right"><a href="command:tinkr.refresh">Refresh</a></td>` +
      `</tr></table>`
  );
  return md;
}

function buildMissingTooltip(err) {
  const md = new vscode.MarkdownString("", true);
  md.isTrusted = true;
  md.supportHtml = true;
  md.supportThemeIcons = true;
  md.appendMarkdown(
    `<table><tr>` +
      `<td width="180" align="left">$(warning) <b>Tinkr</b></td>` +
      `<td width="88" align="right"><span style="color:#8b949e;">Not found</span></td>` +
      `</tr></table>\n\n` +
      `---\n\n` +
      `<span style="color:#8b949e;">${err && err.message ? err.message : "Could not locate local tinkr binary."}</span>\n\n` +
      `<a href="command:tinkr.refresh">Retry?</a>`
  );
  return md;
}

function activate(context) {
  const item = vscode.window.createStatusBarItem(vscode.StatusBarAlignment.Right, 100);
  item.command = "tinkr.doctor";
  item.tooltip = "Tinkr token savings (loading...)";
  item.show();

  const refresh = () => {
    runTinkr(["gain", "--json"], (err, stdout) => {
      if (err) {
        item.text = "$(pulse) Tinkr: not found";
        item.tooltip = buildMissingTooltip(err);
        return;
      }
      try {
        const s = JSON.parse(stdout);
        item.text = `$(sparkle) Tinkr: -${Number(s.percent || 0).toFixed(0)}% (${fmtNum(s.saved)} tok)`;
        item.tooltip = buildTooltip(s);
      } catch {
        item.text = "$(sparkle) Tinkr";
        item.tooltip = "Tinkr token savings (click to run Tinkr: Doctor)";
      }
    });
  };

  const out = vscode.window.createOutputChannel("Tinkr");
  context.subscriptions.push(
    item,
    out,
    vscode.commands.registerCommand("tinkr.refresh", refresh),
    vscode.commands.registerCommand("tinkr.doctor", () => {
      out.clear();
      out.show(true);
      runTinkr(["doctor", "--fix"], (err, stdout, stderr) => {
        out.appendLine(stdout || stderr || (err ? err.message : "done"));
        refresh();
      });
    }),
    vscode.commands.registerCommand("tinkr.overlap", () => {
      out.clear();
      out.show(true);
      runTinkr(["overlap"], (err, stdout, stderr) => {
        out.appendLine(stdout || stderr || (err ? err.message : "done"));
        refresh();
      });
    })
  );

  refresh();
  const timer = setInterval(refresh, 15_000);
  // Also watch Tinkr's data dir so status bar updates immediately when ledger.db changes.
  const dataDir =
    process.env.TINKR_HOME ||
    (process.platform === "win32" && process.env.LOCALAPPDATA
      ? path.join(process.env.LOCALAPPDATA, "tinkr")
      : path.join(os.homedir(), ".local", "share", "tinkr"));
  let watcher = null;
  let debounce = null;
  try {
    if (fs.existsSync(dataDir)) {
      watcher = fs.watch(dataDir, () => {
        if (debounce) clearTimeout(debounce);
        debounce = setTimeout(refresh, 500);
      });
    }
  } catch {}
  context.subscriptions.push({
    dispose: () => {
      clearInterval(timer);
      if (debounce) clearTimeout(debounce);
      if (watcher) watcher.close();
    },
  });
}

function deactivate() {}
module.exports = { activate, deactivate };
