# Validate effective Compose without touching production or reading secrets.
import json, subprocess
from pathlib import Path
root=Path(__file__).resolve().parent
c=json.loads(subprocess.check_output(["docker","compose","-p","firecrawl-rust-canary","-f",str(root/"compose.rust-canary.yaml"),"config","--format","json"],text=True))
assert c["name"]=="firecrawl-rust-canary"
s=c["services"]; assert set(s)=={"init","rust-api","pdf-ocr","mcp"}
for v in s.values():
    assert not any(v.get(k) for k in ("ports","container_name","privileged"))
    assert v["read_only"] is True
    assert "ALL" in v["cap_drop"]
    assert "no-new-privileges:true" in v["security_opt"]
assert s["init"]["network_mode"]=="none"
assert s["init"]["environment"]["RESEARCH_CI_FIXTURE"]=="0"
assert set(s["rust-api"]["networks"])=={"private","production_backend"}
assert s["rust-api"]["networks"]["private"]["aliases"]==["api"]
assert not (s["rust-api"]["networks"]["production_backend"] or {}).get("aliases")
assert "CANDIDATE_MODEL_NAME" not in s["rust-api"]["environment"]
assert s["rust-api"]["environment"]["CANDIDATE_BROWSER_ENABLED"]=="1"
assert s["mcp"]["environment"]["FIRECRAWL_API_URL"]=="http://api:3002"
assert s["mcp"]["environment"]["FIRECRAWL_API_KEY_FILE"]==s["rust-api"]["environment"]["CANDIDATE_TOKEN_FILE"]
for n in ("mcp","pdf-ocr"): assert set(s[n]["networks"])=={"private"}
assert not s["pdf-ocr"].get("volumes")
assert c["networks"]["private"]["internal"] is True
assert c["networks"]["production_backend"]["external"] is True
assert c["networks"]["production_backend"]["name"]=="firecrawl_backend"
assert set(c["volumes"])=={"candidate-token","candidate-jobs"}
for v in c["volumes"].values(): assert not v.get("external") and v["name"].startswith("firecrawl-rust-canary_")
for v in s.values():
    assert all(m["type"]=="volume" and m["source"] in c["volumes"] for m in v.get("volumes",[]))
print("CANARY_COMPOSE_POLICY_PASS")
