# Signed-upload smoke test for a running steeld (the flow Flint uses): stage, PUT bytes, send with references.
# Usage: python3 scripts/staged_upload_smoke_test.py http://127.0.0.1:8080
import json, sys, time, urllib.request, urllib.error
BASE = sys.argv[1]; API = BASE + "/api/v10"
RUN = str(int(time.time()))[-6:]
def req(method, path, body=None, token=None, raw=None, ctype="application/json", base=API):
    data = raw if raw is not None else (json.dumps(body).encode() if body is not None else None)
    headers = {"content-type": ctype}
    if token: headers["authorization"] = "Bearer " + token
    r = urllib.request.Request(base + path, data=data, method=method, headers=headers)
    try:
        with urllib.request.urlopen(r) as resp:
            return resp.status, resp.read(), resp.headers
    except urllib.error.HTTPError as e:
        return e.code, e.read(), e.headers
tok = json.loads(req("POST", "/auth/register", {"username": f"sg_{RUN}", "password": "password123"})[1])["token"]
ch = json.loads(req("POST", "/guilds", {"name": "Staged"}, tok)[1])["channels"][0]["id"]
data = b"staged bytes\n"
s, out, _ = req("POST", f"/channels/{ch}/attachments", {"files": [{"id": 42, "filename": "notes.txt", "file_size": len(data), "is_clip": False, "original_content_type": "text/plain"}]}, tok)
staged = json.loads(out); target = staged["attachments"][0]
print("stage", s, "id echoed", target["id"], "upload_url ok", target["upload_url"].startswith(BASE))
s, _, _ = req("PUT", target["upload_url"].replace(BASE, ""), raw=data, ctype="application/octet-stream", base=BASE)
print("put", s)
print("put again ->", req("PUT", target["upload_url"].replace(BASE, ""), raw=data, ctype="application/octet-stream", base=BASE)[0])
s, out, _ = req("POST", f"/channels/{ch}/messages", {"content": "with staged file", "attachments": [{"id": 42, "filename": "notes.txt", "uploaded_filename": target["upload_filename"]}]}, tok)
msg = json.loads(out); print("send", s, [(a["filename"], a["size"]) for a in msg["attachments"]])
att = msg["attachments"][0]
s, body, hdrs = req("GET", att["url"].replace(BASE, ""), base=BASE)
print("cdn", s, body == data, hdrs.get("content-type"))
print("reuse same reference ->", req("POST", f"/channels/{ch}/messages", {"content": "again", "attachments": [{"id": 42, "filename": "notes.txt", "uploaded_filename": target["upload_filename"]}]}, tok)[0])
print("bogus reference ->", req("POST", f"/channels/{ch}/messages", {"content": "x", "attachments": [{"id": 1, "filename": "a", "uploaded_filename": "nope"}]}, tok)[0])
s2, _, _ = req("POST", f"/channels/{ch}/attachments", {"files": [{"id": 1, "filename": "wrong.txt", "file_size": 3}]}, tok)
t2 = json.loads(req("POST", f"/channels/{ch}/attachments", {"files": [{"id": 1, "filename": "wrong.txt", "file_size": 3}]}, tok)[1])["attachments"][0]
print("size mismatch ->", req("PUT", t2["upload_url"].replace(BASE, ""), raw=b"toolong", ctype="application/octet-stream", base=BASE)[0])
