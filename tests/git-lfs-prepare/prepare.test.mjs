import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { spawnSync } from "node:child_process";
import {
  chmodSync,
  copyFileSync,
  existsSync,
  mkdirSync,
  mkdtempSync,
  readFileSync,
  readdirSync,
  rmSync,
  writeFileSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import test from "node:test";

const repository = resolve(dirname(fileURLToPath(import.meta.url)), "../..");
const sha256 = (bytes) => createHash("sha256").update(bytes).digest("hex");

function fixture(t) {
  const root = mkdtempSync(join(tmpdir(), "kb-lfs-prepare-test-"));
  t.after(() => rmSync(root, { recursive: true, force: true }));
  mkdirSync(join(root, "scripts"));
  mkdirSync(join(root, "source"));
  const executable = join(root, "source/git-lfs");
  writeFileSync(executable, "#!/bin/sh\nprintf 'git-lfs/3.7.1 fixture\\n'\n");
  chmodSync(executable, 0o755);
  const archive = join(root, "source.tar.gz");
  const tar = spawnSync("/usr/bin/tar", [
    "-czf",
    archive,
    "-C",
    join(root, "source"),
    "git-lfs",
  ]);
  assert.equal(tar.status, 0);
  const digest = sha256(readFileSync(archive));
  const target = "aarch64-apple-darwin";
  writeFileSync(
    join(root, "scripts/git-lfs-assets.json"),
    JSON.stringify({
      version: "3.7.1",
      releaseBase: "https://fixture.invalid",
      targets: {
        [target]: {
          asset: "source.tar.gz",
          archive: "tar.gz",
          sha256: digest,
          releaseAssetId: 123456,
        },
      },
    }),
  );
  copyFileSync(
    join(repository, "scripts/prepare-git-lfs.mjs"),
    join(root, "scripts/prepare-git-lfs.mjs"),
  );
  // localhost待受や外部取得を使わず、取得bytesだけを差し替える。
  const loader = join(root, "fetch.mjs");
  const requestsPath = join(root, "requests.jsonl");
  const configureFetch = (steps = [{}]) =>
    writeFileSync(
      loader,
      `import { readFileSync, writeFileSync } from 'node:fs';
const steps = ${JSON.stringify(steps)};
let index = 0;
const originalTimeout = globalThis.setTimeout;
const signalTimeout = AbortSignal.timeout.bind(AbortSignal);
let requestedTimeout;
AbortSignal.timeout = (milliseconds) => { requestedTimeout = milliseconds; return signalTimeout(5); };
globalThis.setTimeout = (done) => originalTimeout(done, 0);
const networkError = () => { const error = new TypeError('synthetic transport failure'); error.cause = { code: 'ECONNRESET' }; return error; };
globalThis.fetch = async (url, options) => {
  writeFileSync(${JSON.stringify(requestsPath)}, JSON.stringify({ url, headers: options.headers, redirect: options.redirect, timeout: requestedTimeout }) + '\\n', { flag: 'a' });
  const step = steps[Math.min(index++, steps.length - 1)];
  if (step.error === 'network') throw networkError();
  if (step.error === 'timeout') { await new Promise(done => originalTimeout(done, 10)); options.signal.throwIfAborted(); }
  const status = step.status ?? 200;
  return { ok: status >= 200 && status < 300, status, body: { cancel: async () => {} }, arrayBuffer: async () => {
    if (step.error === 'body') throw networkError();
    return step.wrongBytes ? Buffer.from('invalid archive') : readFileSync(${JSON.stringify(archive)});
  } };
};\n`,
    );
  configureFetch();
  const run = () =>
    spawnSync(
      process.execPath,
      [
        "--import",
        loader,
        join(root, "scripts/prepare-git-lfs.mjs"),
        "--target",
        target,
      ],
      {
        encoding: "utf8",
        env: { ...process.env, KB_SIDECAR_CACHE_DIR: join(root, "cache") },
      },
    );
  const requests = () =>
    existsSync(requestsPath)
      ? readFileSync(requestsPath, "utf8")
          .trim()
          .split("\n")
          .filter(Boolean)
          .map((line) => JSON.parse(line))
      : [];
  const resetRequests = () => rmSync(requestsPath, { force: true });
  return {
    root,
    run,
    archive,
    digest,
    configureFetch,
    requests,
    resetRequests,
    sidecar: join(root, `app/src-tauri/binaries/git-lfs-${target}`),
  };
}

test("版文字列だけ一致する改変binaryを固定archiveの実体で置き換える", (t) => {
  const f = fixture(t);
  assert.equal(f.run().status, 0);
  const expected = readFileSync(f.sidecar);
  writeFileSync(
    f.sidecar,
    "#!/bin/sh\nprintf 'git-lfs/3.7.1 counterfeit\\n'\n",
  );
  assert.equal(f.run().status, 0);
  assert.deepEqual(readFileSync(f.sidecar), expected);
  const proof = JSON.parse(readFileSync(`${f.sidecar}.provenance.json`));
  assert.equal(proof.archive_sha256, f.digest);
  assert.equal(proof.binary_sha256, sha256(expected));
});

test("改変cacheを再取得し、固定SHAと違う取得物からprovenanceを作らない", (t) => {
  const f = fixture(t);
  assert.equal(f.run().status, 0);
  writeFileSync(join(f.root, "cache/source.tar.gz"), "damaged-cache");
  assert.equal(f.run().status, 0);
  assert.equal(
    sha256(readFileSync(join(f.root, "cache/source.tar.gz"))),
    f.digest,
  );
  writeFileSync(join(f.root, "cache/source.tar.gz"), "damaged-cache-again");
  writeFileSync(f.archive, "untrusted-download");
  const proof = readFileSync(`${f.sidecar}.provenance.json`);
  const result = f.run();
  assert.notEqual(result.status, 0);
  assert.match(result.stderr, /SHA-256が一致しません/);
  assert.deepEqual(readFileSync(`${f.sidecar}.provenance.json`), proof);
});

// 2026-09-09: 一時的なHTTP 500を待ち直し、恒久404や改変bytesまで再試行しない。
test("一時HTTP失敗だけを有限再試行し、成功後のcacheは通信せず再利用する", (t) => {
  const f = fixture(t);
  f.configureFetch([{ status: 500 }, { status: 503 }, {}]);
  const result = f.run();
  assert.equal(result.status, 0, result.stderr);
  assert.equal(f.requests().length, 3);
  assert.ok(
    f
      .requests()
      .every(
        (request) =>
          request.url === "https://fixture.invalid/source.tar.gz" &&
          request.timeout === 30_000 &&
          request.redirect === "follow",
      ),
  );
  f.resetRequests();
  assert.equal(f.run().status, 0);
  assert.deepEqual(f.requests(), []);
});

test("直接URLが復旧しなければ固定asset IDの公式APIをbinary取得に使う", (t) => {
  const f = fixture(t);
  f.configureFetch([{ status: 500 }, { status: 502 }, { status: 504 }, {}]);
  const result = f.run();
  assert.equal(result.status, 0, result.stderr);
  assert.equal(f.requests().length, 4);
  assert.equal(
    f.requests()[3].url,
    "https://api.github.com/repos/git-lfs/git-lfs/releases/assets/123456",
  );
  assert.equal(f.requests()[3].headers.Accept, "application/octet-stream");
  assert.equal(f.requests()[3].redirect, "follow");
  assert.match(result.stderr, /公式GitHub APIへ切り替え/);
  assert.equal(
    sha256(readFileSync(join(f.root, "cache/source.tar.gz"))),
    f.digest,
  );
});

test("恒久404は再試行せずAPI fallbackでも隠さない", (t) => {
  const f = fixture(t);
  f.configureFetch([{ status: 404 }]);
  const result = f.run();
  assert.notEqual(result.status, 0);
  assert.match(result.stderr, /HTTP 404/);
  assert.equal(f.requests().length, 1);
  assert.deepEqual(readdirSync(join(f.root, "cache")), []);
  assert.equal(existsSync(`${f.sidecar}.provenance.json`), false);
});

test("通信失敗・body中断・timeoutは期限内の試行を尽くした後だけAPIへ移る", (t) => {
  const f = fixture(t);
  f.configureFetch([
    { error: "network" },
    { error: "body" },
    { error: "timeout" },
    { status: 503 },
    {},
  ]);
  const result = f.run();
  assert.equal(result.status, 0, result.stderr);
  assert.equal(f.requests().length, 5);
  assert.ok(f.requests().every((request) => request.timeout === 30_000));
  assert.match(result.stderr, /ECONNRESET/);
  assert.match(result.stderr, /30秒の取得期限/);
});

test("両経路が失敗したら6試行で停止し、cacheと既存sidecar/provenanceを変えない", (t) => {
  const f = fixture(t);
  assert.equal(f.run().status, 0);
  const proof = readFileSync(`${f.sidecar}.provenance.json`);
  const sidecar = readFileSync(f.sidecar);
  f.resetRequests();
  writeFileSync(join(f.root, "cache/source.tar.gz"), "damaged-cache");
  f.configureFetch([{ status: 503 }]);
  const result = f.run();
  assert.notEqual(result.status, 0);
  assert.equal(f.requests().length, 6);
  assert.equal(
    readFileSync(join(f.root, "cache/source.tar.gz"), "utf8"),
    "damaged-cache",
  );
  assert.deepEqual(readdirSync(join(f.root, "cache")), ["source.tar.gz"]);
  assert.deepEqual(readFileSync(f.sidecar), sidecar);
  assert.deepEqual(readFileSync(`${f.sidecar}.provenance.json`), proof);
});

test("200のSHA不一致は直接URLでもAPIでも即停止し、cacheへ確定しない", (t) => {
  for (const steps of [
    [{ wrongBytes: true }],
    [{ status: 500 }, { status: 500 }, { status: 500 }, { wrongBytes: true }],
  ]) {
    const f = fixture(t);
    f.configureFetch(steps);
    const result = f.run();
    assert.notEqual(result.status, 0);
    assert.match(result.stderr, /SHA-256が一致しません/);
    assert.equal(f.requests().length, steps.length);
    assert.deepEqual(readdirSync(join(f.root, "cache")), []);
    assert.equal(existsSync(`${f.sidecar}.provenance.json`), false);
  }
});
