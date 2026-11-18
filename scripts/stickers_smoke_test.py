# Sticker smoke test for a running steeld. Usage: python3 scripts/stickers_smoke_test.py http://127.0.0.1:8080
import base64, json, sys, time, urllib.request, urllib.error, uuid
BASE = sys.argv[1]; API = BASE + "/api/v10"
RUN = str(int(time.time()))[-6:]
PNG = base64.b64decode("iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNkYPhfDwAChwGA60e6kgAAAABJRU5ErkJggg==")
GIF = base64.b64decode("R0lGODlhAQABAIAAAP///wAAACH5BAEAAAAALAAAAAABAAEAAAICRAEAOw==")
def req(method, path, body=None, token=None, raw=None, ctype="application/json"):
    data = raw if raw is not None else (json.dumps(body).encode() if body is not None else None)
    headers = {"content-type": ctype, **({"authorization": "Bearer " + token} if token else {})}
    r = urllib.request.Request(API + path, data=data, method=method, headers=headers)
    try:
        with urllib.request.urlopen(r) as resp:
            raw_out = resp.read(); return resp.status, (json.loads(raw_out) if raw_out else None)
    except urllib.error.HTTPError as e:
        return e.code, json.loads(e.read() or b"null")
def multipart(fields, file_bytes, filename):
    boundary = "----steel" + uuid.uuid4().hex
    body = b""
    for k, v in fields.items():
        body += f'--{boundary}\r\nContent-Disposition: form-data; name="{k}"\r\n\r\n{v}\r\n'.encode()
    body += f'--{boundary}\r\nContent-Disposition: form-data; name="file"; filename="{filename}"\r\nContent-Type: application/octet-stream\r\n\r\n'.encode() + file_bytes + f"\r\n--{boundary}--\r\n".encode()
    return f"multipart/form-data; boundary={boundary}", body
def reg(n): return req("POST", "/auth/register", {"username": f"{n}_{RUN}", "password": "password123"})[1]
own, mem = reg("stown"), reg("stmemb")
to, tm = own["token"], mem["token"]
g = req("POST", "/guilds", {"name": "Stickers"}, to)[1]; gid = g["id"]; ch = g["channels"][0]["id"]
inv = req("POST", f"/channels/{ch}/invites", {}, to)[1]["code"]; req("POST", f"/invites/{inv}", token=tm)

ctype, body = multipart({"name": "wave_1", "description": "A wave", "tags": "wave"}, PNG, "wave.png")
s, st = req("POST", f"/guilds/{gid}/stickers", raw=body, token=to, ctype=ctype)
print("create png ->", s, st.get("type") == 2, st.get("format_type") == 1)
sid = st["id"]
ctype, body = multipart({"name": "dance_1", "tags": "dance"}, GIF, "dance.gif")
s2, st2 = req("POST", f"/guilds/{gid}/stickers", raw=body, token=to, ctype=ctype)
print("create gif ->", s2, st2.get("format_type") == 4, st2.get("description") is None)
ctype, body = multipart({"name": "wave_2", "tags": "wave"}, PNG, "wave.png")
print("member create ->", req("POST", f"/guilds/{gid}/stickers", raw=body, token=tm, ctype=ctype)[0])
ctype, body = multipart({"name": "wave_3", "tags": "wave"}, b"not an image", "x.png")
print("non-image refused ->", req("POST", f"/guilds/{gid}/stickers", raw=body, token=to, ctype=ctype)[0])
ctype, body = multipart({"name": "w", "tags": "wave"}, PNG, "wave.png")
print("name too short ->", req("POST", f"/guilds/{gid}/stickers", raw=body, token=to, ctype=ctype)[0])
ctype, body = multipart({"name": "wave_1", "tags": "wave"}, PNG, "wave.png")
print("duplicate name ->", req("POST", f"/guilds/{gid}/stickers", raw=body, token=to, ctype=ctype)[0])
ctype, body = multipart({"name": "no_tags"}, PNG, "wave.png")
print("missing tags ->", req("POST", f"/guilds/{gid}/stickers", raw=body, token=to, ctype=ctype)[0])
lst = req("GET", f"/guilds/{gid}/stickers", token=tm)[1]
print("member list ->", sorted(x["name"] for x in lst) == ["dance_1", "wave_1"])
print("outsider list ->", req("GET", f"/guilds/{gid}/stickers", token=reg("stout")["token"])[0])
print("edit ->", req("PATCH", f"/guilds/{gid}/stickers/{sid}", {"name": "wave_renamed", "description": "Hi there", "tags": "hi"}, to)[1].get("name"))
print("edit by member ->", req("PATCH", f"/guilds/{gid}/stickers/{sid}", {"name": "nope1"}, tm)[0])
print("edit unknown ->", req("PATCH", f"/guilds/{gid}/stickers/999", {"name": "nope2"}, to)[0])
print("serve png ->", urllib.request.urlopen(f"{BASE}/cdn/stickers/{sid}.png").headers.get("content-type"))
print("delete by member ->", req("DELETE", f"/guilds/{gid}/stickers/{sid}", token=tm)[0])
print("delete by owner ->", req("DELETE", f"/guilds/{gid}/stickers/{sid}", token=to)[0])
try:
    urllib.request.urlopen(f"{BASE}/cdn/stickers/{sid}.png"); print("served after delete -> 200 (bad)")
except urllib.error.HTTPError as err:
    print("served after delete ->", err.code)
print("remaining ->", [x["name"] for x in req("GET", f"/guilds/{gid}/stickers", token=to)[1]])
