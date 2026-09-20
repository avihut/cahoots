"""S3: start a harness in its own session (as the supervisor will), signal its
process group mid-run, and record: time to first id, exit status, stragglers."""
import json, os, signal, subprocess, sys, time, uuid

which, signame, delay = sys.argv[1], sys.argv[2], float(sys.argv[3])
prompt = "Count from 1 to 40, running the shell command `sleep 2` between numbers. Do not stop early."
sid = str(uuid.uuid4())
if which == "codex":
    argv = ["codex", "exec", "--json", "--sandbox", "read-only", "-m", "gpt-5.6-luna", "-c", "model_reasoning_effort=low", "-"]
else:
    argv = ["claude", "-p", "--model", "haiku", "--output-format", "json", "--session-id", sid, "--allowedTools", "Bash(sleep:*)"]
t0 = time.time()
p = subprocess.Popen(argv, stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE, start_new_session=True, text=True)
p.stdin.write(prompt); p.stdin.close()
first = None
if which == "codex":
    while True:
        line = p.stdout.readline()
        if not line: break
        try:
            e = json.loads(line)
        except ValueError:
            print("non-json stdout line:", line.strip()[:80]); continue
        if e.get("type") == "thread.started":
            sid = e["thread_id"]; first = time.time() - t0; break
print(f"{which}: id known after {first if first is not None else 0:.2f}s (claude: preset)")
time.sleep(delay)
group = subprocess.run(["pgrep", "-g", str(p.pid)], capture_output=True, text=True).stdout.split()
print(f"group {p.pid} has {len(group)} processes before {signame}")
os.killpg(p.pid, getattr(signal, signame))
t1 = time.time()
try:
    rc = p.wait(timeout=20); print(f"exited rc={rc} after {time.time()-t1:.2f}s")
except subprocess.TimeoutExpired:
    print("did NOT exit within 20s; SIGKILL"); os.killpg(p.pid, signal.SIGKILL); rc = p.wait()
time.sleep(1)
left = subprocess.run(["pgrep", "-g", str(p.pid)], capture_output=True, text=True).stdout.split()
print(f"stragglers in group after exit: {len(left)}")
tail = (p.stdout.read() or "")[-300:]
print("stdout tail:", tail.replace("\n", " | ")[:300])
print("SESSION", sid)
