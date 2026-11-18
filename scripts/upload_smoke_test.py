# Upload and CDN smoke test for a running steeld. Usage: python3 scripts/upload_smoke_test.py http://127.0.0.1:8080
# Uses only the standard library. The server needs the same STEEL_DATA_DIR visible to delete checks (optional).
import json, os, sys, time, urllib.request, urllib.error, uuid
BASE=sys.argv[1]; API=BASE+"/api/v10"
RUN=str(int(time.time()))[-6:]
def req(method, path, body=None, token=None, raw=None, ctype="application/json", base=API):
    data = raw if raw is not None else (json.dumps(body).encode() if body is not None else None)
    headers = {"content-type": ctype}
    if token: headers["authorization"] = "Bearer " + token
    r = urllib.request.Request(base+path, data=data, method=method, headers=headers)
    try:
        with urllib.request.urlopen(r) as resp:
            raw_out = resp.read(); return resp.status, raw_out, resp.headers
    except urllib.error.HTTPError as e:
        return e.code, e.read(), e.headers
def jbody(status_body): return json.loads(status_body[1] or b"null")
def multipart(payload, files):
    boundary = uuid.uuid4().hex
    parts = []
    parts.append(f'--{boundary}\r\nContent-Disposition: form-data; name="payload_json"\r\nContent-Type: application/json\r\n\r\n{json.dumps(payload)}\r\n'.encode())
    for i,(name, ctype, data) in enumerate(files):
        parts.append(f'--{boundary}\r\nContent-Disposition: form-data; name="files[{i}]"; filename="{name}"\r\nContent-Type: {ctype}\r\n\r\n'.encode()+data+b"\r\n")
    parts.append(f"--{boundary}--\r\n".encode())
    return b"".join(parts), f"multipart/form-data; boundary={boundary}"
tok = json.loads(req("POST","/auth/register",{"username":f"up_{RUN}","password":"password123"})[1])["token"]
g = json.loads(req("POST","/guilds",{"name":"Uploads"},tok)[1])
ch = g["channels"][0]["id"]
body, ctype = multipart({"content":"see attached"}, [("hello world.txt","text/plain",b"hi there\n"), ("pic.png","image/png",b"\x89PNG fake")])
s, out, _ = req("POST", f"/channels/{ch}/messages", raw=body, token=tok, ctype=ctype)
msg = json.loads(out); print("upload status", s, "attachments", [(a["filename"], a["size"]) for a in msg["attachments"]])
att = msg["attachments"][0]
s, out, hdrs = req("GET", att["url"].replace(BASE,""), base=BASE)
print("cdn fetch", s, out, hdrs.get("content-type"), hdrs.get("x-content-type-options"))
print("cdn wrong filename ->", req("GET", f"/cdn/attachments/{ch}/{att['id']}/nope.txt", base=BASE)[0])
print("cdn wrong channel ->", req("GET", f"/cdn/attachments/1/{att['id']}/{att['filename']}", base=BASE)[0])
s, out, _ = req("GET", f"/channels/{ch}/messages?limit=5", token=tok)
hist = json.loads(out); print("history has attachments", len(hist[0]["attachments"]))
big_body, big_ctype = multipart({"content":"big"}, [("big.bin","application/octet-stream", b"x"*(9*1024*1024))])
s, out, _ = req("POST", f"/channels/{ch}/messages", raw=big_body, token=tok, ctype=big_ctype)
print("oversize file ->", s)
s, out, _ = req("POST", f"/channels/{ch}/messages", {"content":"plain json"}, tok)
print("json send ->", s, json.loads(out)["attachments"])
s, out, _ = req("POST", f"/channels/{ch}/messages", {"content":""}, tok)
print("empty json ->", s)
s, out, _ = req("DELETE", f"/channels/{ch}/messages/{msg['id']}", token=tok)
print("delete with files ->", s, "cdn after delete ->", req("GET", att["url"].replace(BASE,""), base=BASE)[0])
