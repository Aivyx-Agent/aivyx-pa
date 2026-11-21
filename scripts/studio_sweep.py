#!/usr/bin/env python3
"""Headless Studio regression sweep — clicks through every screen and
fails loudly on any wasm panic / console error.

Born from the Chime context-panic (2026-07-06): a missing
use_context_provider compiled clean and passed every native test, then
killed the whole app on the operator's first navigate. Runtime context
lookups are invisible to the compiler — drive the real page.

Usage:
    AIVYX_TOKEN=<gatehouse token> python3 scripts/studio_sweep.py
Requires: google-chrome-stable, `pip install websocket-client`.
Set STUDIO_SWEEP_PROFILE to a scratch Chrome profile dir.
Target URL is hardcoded to the dogfood rig; edit as needed.
"""
import json, subprocess, time, os, base64
import urllib.request

port = 9224
prof = os.environ.get("STUDIO_SWEEP_PROFILE")
if not prof:
    raise SystemExit("set STUDIO_SWEEP_PROFILE to a scratch Chrome profile directory")
chrome = subprocess.Popen([
    "/usr/bin/google-chrome-stable", "--headless=new", "--disable-gpu",
    f"--remote-debugging-port={port}", "--remote-allow-origins=*",
    "--no-first-run", f"--user-data-dir={prof}", "about:blank",
], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
time.sleep(3)
tabs = json.loads(urllib.request.urlopen(f"http://127.0.0.1:{port}/json").read())
from websocket import create_connection
ws = create_connection(tabs[0]["webSocketDebuggerUrl"])
mid = 0
errors = []
def send(method, params=None, wait=False):
    global mid
    mid += 1
    ws.send(json.dumps({"id": mid, "method": method, "params": params or {}}))
    if not wait:
        return None
    while True:
        msg = json.loads(ws.recv())
        if msg.get("id") == mid:
            return msg.get("result")
        m = msg.get("method","")
        if m == "Runtime.exceptionThrown":
            errors.append(json.dumps(msg["params"]["exceptionDetails"])[:300])
        elif m == "Runtime.consoleAPICalled" and msg["params"]["type"] == "error":
            args = [str(a.get("value", a.get("description","?")))[:200] for a in msg["params"]["args"]]
            errors.append("console.error: " + " ".join(args))

def ev(expr):
    r = send("Runtime.evaluate", {"expression": expr, "returnByValue": True}, wait=True)
    return (r or {}).get("result", {}).get("value")

send("Runtime.enable"); send("Page.enable"); send("Network.enable")
token = os.environ["AIVYX_TOKEN"]
b64 = base64.b64encode(f"x:{token}".encode()).decode()
send("Network.setExtraHTTPHeaders", {"headers": {"Authorization": f"Basic {b64}"}})
send("Page.navigate", {"url": "http://10.80.80.148:7843/#schedules"}, wait=True)
time.sleep(20)

# The operator's exact failure mode: booting straight into #schedules.
print("boot-on-#schedules main:", str(ev("(document.querySelector('main')||{}).textContent"))[:120])

for label in ["Command","Chat","Missions","Schedules","Notifications","Memory","Skills","Tools","Agents","Teams","Settings"]:
    ok = ev(f"""
    (() => {{
      const els = [...document.querySelectorAll('.sidebar *')];
      const m = els.find(e => e.textContent.trim() === '{label}' && e.children.length <= 2);
      if (!m) return 'MISSING';
      m.click();
      return 'ok';
    }})()""")
    time.sleep(1.2)
    h = ev("location.hash")
    alive = ev("!!document.querySelector('main') && document.querySelector('main').textContent.length > 0")
    print(f"{label}: click={ok} hash={h} rendered={alive}")

sched_text = str(ev("""
(() => { const els=[...document.querySelectorAll('.sidebar *')];
  const m=els.find(e=>e.textContent.trim()==='Schedules'&&e.children.length<=2);
  if(m) m.click(); return 1; })()
""" ))
time.sleep(2)
print("schedules content:", str(ev("(document.querySelector('main')||{}).textContent"))[:400])
print("ERRORS:", len(errors))
for e in errors[:5]: print("  ", e)
chrome.kill()
print("SWEEP DONE")
