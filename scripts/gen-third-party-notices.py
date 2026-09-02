#!/usr/bin/env python3
"""THIRD_PARTY_NOTICES.md を Cargo.lock / package-lock.json とローカルの
パッケージキャッシュから生成する。ネットワークは使わないため、事前に
`cargo fetch` と `npm --prefix app ci` を済ませておくこと。

    python3 scripts/gen-third-party-notices.py
"""
import json, re, glob, os, hashlib, sys
ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
CACHE = glob.glob(os.path.expanduser("~/.cargo/registry/src/*/"))

def read(p, limit=200000):
    try:
        with open(p, encoding="utf-8", errors="replace") as f:
            return f.read(limit)
    except Exception:
        return None

# ---- Rust (Cargo.lock) ----
lock = read(os.path.join(ROOT, "Cargo.lock"))
pkgs = []
for block in lock.split("[[package]]")[1:]:
    n = re.search(r'^name = "(.*?)"', block, re.M)
    v = re.search(r'^version = "(.*?)"', block, re.M)
    if n and v:
        pkgs.append((n.group(1), v.group(1)))

local = {os.path.basename(p.rstrip("/")) for p in glob.glob(os.path.join(ROOT, "crates", "*"))}
rust, missing = [], []
license_texts = {}   # spdx -> (text, source_pkg)
notices = {}         # sha -> (text, [pkgs])

for name, ver in sorted(set(pkgs)):
    if name in local:
        continue
    d = None
    for c in CACHE:
        cand = os.path.join(c, f"{name}-{ver}")
        if os.path.isdir(cand):
            d = cand
            break
    if not d:
        missing.append((name, ver))
        rust.append((name, ver, "(未取得: cacheに無し)"))
        continue
    toml = read(os.path.join(d, "Cargo.toml")) or ""
    m = re.search(r'^license\s*=\s*"(.*?)"', toml, re.M)
    spdx = m.group(1) if m else "(license欄なし)"
    rust.append((name, ver, spdx))
    for fn in sorted(os.listdir(d)):
        low = fn.lower()
        p = os.path.join(d, fn)
        if not os.path.isfile(p):
            continue
        if low.startswith("notice"):
            t = read(p)
            if t:
                h = hashlib.sha256(t.encode()).hexdigest()
                notices.setdefault(h, [t, []])[1].append(f"{name} {ver}")
        elif low.startswith("licen"):
            t = read(p)
            if not t:
                continue
            key = None
            if "apache" in low or "Apache License" in t[:400]:
                key = "Apache-2.0"
            elif "mpl" in low or "Mozilla Public License" in t[:400]:
                key = "MPL-2.0"
            elif "mit" in low or "Permission is hereby granted, free of charge" in t:
                key = "MIT"
            elif "Redistribution and use in source and binary forms" in t:
                key = "BSD"
            elif "unicode" in low or "UNICODE" in t[:400]:
                key = "Unicode-3.0"
            elif "isc" in low or "ISC" in t[:200]:
                key = "ISC"
            elif "unlicense" in low or "public domain" in t[:400].lower():
                key = "Unlicense"
            elif "zlib" in low:
                key = "Zlib"
            if key and key not in license_texts:
                license_texts[key] = (t, f"{name} {ver}")

# ---- npm (package-lock.json, 非dev のみ) ----
plock = json.load(open(os.path.join(ROOT, "app", "package-lock.json")))
npm = []
for path, meta in sorted(plock.get("packages", {}).items()):
    if not path.startswith("node_modules/") or meta.get("dev"):
        continue
    name = path.split("node_modules/")[-1]
    ver = meta.get("version", "?")
    lic = meta.get("license")
    d = os.path.join(ROOT, "app", path)
    if lic is None and os.path.isdir(d):
        pj = read(os.path.join(d, "package.json"))
        if pj:
            try:
                lic = json.loads(pj).get("license")
            except Exception:
                lic = None
    if isinstance(lic, dict):
        lic = lic.get("type")
    npm.append((name, ver, lic or "(未記載)"))
    if os.path.isdir(d):
        for fn in os.listdir(d):
            if fn.lower().startswith("notice"):
                t = read(os.path.join(d, fn))
                if t:
                    h = hashlib.sha256(t.encode()).hexdigest()
                    notices.setdefault(h, [t, []])[1].append(f"{name} {ver} (npm)")

# ---- 同梱バイナリ ----
bundled = []
gl = read(os.path.join(ROOT, "third_party", "git-lfs", "LICENSE.md"))
if gl:
    bundled.append(("git-lfs", "同梱バイナリ (app/src-tauri/binaries/)", gl))

out = []
out.append("# サードパーティ通知 (Third-Party Notices)\n")
out.append("本ソフトウェアは以下のサードパーティコンポーネントを利用・同梱しています。")
out.append("各コンポーネントは、それぞれの著作権者が定めるライセンス条件のもとで配布されます。\n")
out.append("このファイルは依存関係のロックファイルとローカルのパッケージキャッシュから機械生成しました。")
out.append("依存を変更したら `python3 scripts/gen-third-party-notices.py` で再生成してください(CONTRIBUTING.md 参照)。\n")
out.append(f"- Rust クレート: {len(rust)} 件\n- npm パッケージ(非dev): {len(npm)} 件\n- 同梱バイナリ: {len(bundled)} 件\n")

out.append("\n## 1. Rust クレート\n")
out.append("| クレート | バージョン | ライセンス |\n|---|---|---|")
for n, v, l in rust:
    out.append(f"| {n} | {v} | {l} |")

out.append("\n## 2. npm パッケージ(配布物に含まれる非dev依存)\n")
out.append("| パッケージ | バージョン | ライセンス |\n|---|---|---|")
for n, v, l in npm:
    out.append(f"| {n} | {v} | {l} |")

out.append("\n## 3. 個別の NOTICE ファイル(原文)\n")
out.append("以下は各コンポーネントが配布時の再掲を求める通知の原文です。\n")
for h, (t, owners) in sorted(notices.items(), key=lambda kv: kv[1][1][0]):
    out.append(f"### {', '.join(sorted(set(owners)))}\n")
    out.append("```\n" + t.strip() + "\n```\n")

out.append("\n## 4. 同梱バイナリのライセンス原文\n")
for name, note, text in bundled:
    out.append(f"### {name} — {note}\n")
    out.append("```\n" + text.strip() + "\n```\n")

out.append("\n## 5. ライセンス全文\n")
out.append("上表に現れるライセンスの全文です。同一ライセンスを持つ複数のコンポーネントで共有されます。")
out.append("著作権表示はコンポーネントごとに異なるため、各配布物に含まれる原本を正とします。\n")
for key in sorted(license_texts):
    text, src = license_texts[key]
    out.append(f"### {key}(代表例: {src})\n")
    out.append("```\n" + text.strip() + "\n```\n")

if missing:
    out.append("\n## 付録: ローカルキャッシュに存在せずライセンスを確認できなかったクレート\n")
    for n, v in missing:
        out.append(f"- {n} {v}")
    out.append("\n`cargo fetch` の後に再生成してください。\n")

open(os.path.join(ROOT, "THIRD_PARTY_NOTICES.md"), "w", encoding="utf-8").write("\n".join(out) + "\n")
print("rust:", len(rust), "npm:", len(npm), "missing:", len(missing), "licenses:", sorted(license_texts), "notices:", len(notices))
