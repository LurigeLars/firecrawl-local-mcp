# Render the real pinned upstream Compose plus current wrapper and proposed
# overlay. No daemon operations, no production .env or secrets.
import json, os, shutil, subprocess, tempfile
from pathlib import Path
root=Path(__file__).resolve().parent.parent
pin=json.loads((root/"upstream/firecrawl.json").read_text())
assert pin["repository"]=="firecrawl/firecrawl"
sha=pin["commit"]
assert len(sha)==40 and all(c in "0123456789abcdef" for c in sha)
with tempfile.TemporaryDirectory(prefix="fc-rust-plan-") as work:
    w=Path(work)
    upstream=w/"firecrawl"
    subprocess.run(["git","init",str(upstream)],check=True,stdout=subprocess.DEVNULL)
    subprocess.run(["git","-C",str(upstream),"fetch","--depth","1","https://github.com/firecrawl/firecrawl.git",sha],check=True,stdout=subprocess.DEVNULL)
    assert subprocess.check_output(["git","-C",str(upstream),"rev-parse","FETCH_HEAD"],text=True).strip()==sha
    (upstream/"docker-compose.yaml").write_bytes(subprocess.check_output(["git","-C",str(upstream),"show","FETCH_HEAD:docker-compose.yaml"]))
    for f in ("compose.local.yaml","compose.public.yaml","compose.defaults.env","research/compose.rust-production-plan.yaml"):
        p=w/f;p.parent.mkdir(parents=True,exist_ok=True);shutil.copyfile(root/f,p)
    (w/"public").mkdir(exist_ok=True)
    (w/"public/gateway.env").write_text("")
    (w/".env").write_text("")
    env={**os.environ,"FIRECRAWL_POSTGRES_PASSWORD_SECRET":"synthetic-config-only","FIRECRAWL_SEARXNG_SECRET_SECRET":"synthetic-config-only",
         "FIRECRAWL_PUBLIC_PROXY_SERVER_SECRET":"synthetic-config-only",
         "FIRECRAWL_PUBLIC_PROXY_USERNAME_SECRET":"synthetic-config-only",
         "FIRECRAWL_PUBLIC_PROXY_PASSWORD_SECRET":"synthetic-config-only"}
    args=["docker","compose","-p","firecrawl","--project-directory",str(upstream),
          "-f",str(upstream/"docker-compose.yaml"),"-f",str(w/"compose.local.yaml"),
          "-f",str(w/"compose.public.yaml"),"--env-file",str(w/"compose.defaults.env"),"--env-file",str(w/".env")]
    def config(extra):
        return json.loads(subprocess.check_output(args+extra+["config","--format","json"],env=env,text=True))
    legacy=config([])
    extra=["-f",str(w/"research/compose.rust-production-plan.yaml")]
    rust=config(extra)
    s=rust["services"];old=legacy["services"]
    assert s["api"]["image"]=="firecrawl-rust-api:local"
    assert s["api"]["user"]=="65532:65532"
    assert s["api"]["read_only"] and s["api"]["cap_drop"]==["ALL"]
    assert set(s["api"]["environment"])=={"CANDIDATE_NETWORK_MODE","CANDIDATE_TOKEN_FILE","CANDIDATE_JOB_DB_FILE","CANDIDATE_PDF_ENABLED","CANDIDATE_BROWSER_ENABLED"}
    inherited=[k for k in ("command","post_start","extra_hosts","privileged") if s["api"].get(k)]
    assert not inherited, f"nonempty legacy API settings: {inherited}"
    assert set(s["api"]["depends_on"])=={"rust-init","pdf-ocr"}
    assert set(s["api"]["networks"])=={"backend","rust_documents"}
    ports=s["api"]["ports"];assert len(ports)==1
    assert ports[0]["host_ip"]=="127.0.0.1" and str(ports[0]["published"])=="3002" and ports[0]["target"]==3002
    assert s["api"]["build"]["target"]=="release-runtime"
    assert s["pdf-ocr"]["build"]["target"]=="release-ocr-runtime"
    assert set(s["pdf-ocr"]["networks"])=={"rust_documents"} and not s["pdf-ocr"].get("ports") and not s["pdf-ocr"].get("volumes")
    assert s["rust-init"]["network_mode"]=="none" and s["rust-init"]["environment"]["RESEARCH_CI_FIXTURE"]=="0"
    assert rust["networks"]["rust_documents"]["internal"]
    assert {m["source"] for m in s["api"]["volumes"]}=={"rust-token","rust-jobs"}
    assert {m["source"] for m in s["mcp"]["volumes"]}=={"rust-token"}
    assert s["mcp"]["environment"]["FIRECRAWL_API_KEY_FILE"]==s["api"]["environment"]["CANDIDATE_TOKEN_FILE"]
    assert s["mcp"]["environment"]["FIRECRAWL_API_URL"]=="http://api:3002"
    assert s["mcp"]["networks"]==old["mcp"]["networks"]
    for service in ("gateway","searxng","searxng-proxy","llm-proxy","playwright-service","browser-session-service","camofox-research","camofox-research-secrets-init"):
        assert s[service]==old[service],service
    for service in ("playwright-router","camofox-research-proxy"):
        assert s.get(service)==old.get(service),service
    rollback=config(extra+["--profile","legacy-rollback"])
    for service in ("redis","rabbitmq","nuq-postgres"):
        assert service not in s or "legacy-rollback" in s[service].get("profiles",[])
        actual=rollback["services"][service].copy();actual.pop("profiles",None)
        assert actual==old[service],service
    # Even when the existing public proxy profile is active, its services
    # and credentials stay on the original paths.
    old_proxy=config(["--profile","public-proxy"])
    rust_proxy=config(extra+["--profile","public-proxy"])
    for service in ("playwright-router","playwright-public-proxy-service","public-proxy-metrics-init","camofox-research-proxy"):
        assert rust_proxy["services"][service]==old_proxy["services"][service],service
    old_smoke=config(["--profile","public-proxy","--profile","smoke"])
    rust_smoke=config(extra+["--profile","public-proxy","--profile","smoke"])
    assert rust_smoke["services"]["public-proxy-smoke"]==old_smoke["services"]["public-proxy-smoke"]
print("RUST_PRODUCTION_CONFIG_ONLY_PASS")
