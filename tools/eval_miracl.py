#!/usr/bin/env python3
"""Evaluate KoiosBase retrieval on MIRACL-zh, correctly.

Usage:
    python3 tools/eval_miracl.py --koios rust-cli/target/release/koios \
        --data /tmp/miracl [--queries 40] [--distractors 300] [--json out.json]

Why this script exists, and what it guards against:

An earlier ad-hoc version of this measured hit@1 = 0.700 and hit@5 = 0.725.
That was wrong, and wrong by a lot. Only part of the MIRACL-zh corpus was
downloaded, so 56 of the 103 gold passages for the sampled queries were not
in the indexed corpus at all -- and were scored as retrieval misses. The
system was being asked for text that did not exist.

The corrected number, on queries whose gold passages are all present, is
hit@1 = 0.950, hit@5 = 1.000. The tell had been visible in the bad run:
hit@5 exceeding hit@1 by only 0.025 is not a property of the benchmark, it
is what missing answers look like -- expanding the window cannot help when
the answer is not in the collection.

So this script refuses to report a score without first proving every gold
passage is actually indexed (--strict, the default). It reports
`gold_coverage` explicitly and fails loudly if any are missing.

Data layout expected under --data:
    topics.dev.tsv      query id -> query text
    qrels.dev.tsv       query id -> relevant passage ids (docid#passage)
    docs-0.jsonl ...    corpus shards, one JSON object per line:
                        {"docid": ..., "title": ..., "text": ...}

A benchmark number is not trustworthy until the gold answer has been
confirmed to exist inside the searched collection.
"""

import argparse
import json
import os
import random
import shutil
import subprocess
import sys
import threading
import time

MIRACL_BASE = (
    "https://huggingface.co/datasets/miracl/{repo}/resolve/main/{path}"
)


class Mcp:
    """One long-lived `koios mcp` process.

    Each `koios search` invocation reloads the ~600 MB model, which at ~1.6s
    per load dominates a 100-query run. Keeping one process alive amortises
    that to a single load.
    """

    def __init__(self, koios):
        self.p = subprocess.Popen(
            [koios, "mcp"], stdin=subprocess.PIPE, stdout=subprocess.PIPE,
            text=True, bufsize=1)
        self.lock = threading.Lock()
        self.n = 0

    def call(self, method, params):
        with self.lock:
            self.n += 1
            self.p.stdin.write(json.dumps({
                "jsonrpc": "2.0", "id": self.n, "method": method,
                "params": params}) + "\n")
            self.p.stdin.flush()
            while True:
                line = self.p.stdout.readline()
                if not line:
                    raise RuntimeError("mcp process closed unexpectedly")
                d = json.loads(line)
                if d.get("id") == self.n:
                    return d

    def search(self, vault, question, top=10, layer=None):
        args = {"vault": vault, "question": question, "top": top}
        if layer:
            args["layer"] = layer
        d = self.call("tools/call", {
            "name": "koios_search", "arguments": args})
        if "error" in d:
            raise RuntimeError(str(d["error"]))
        payload = json.loads(d["result"]["content"][0]["text"])
        return [b["id"] for b in payload.get("blocks", [])]

    def close(self):
        try:
            self.p.stdin.close()
            self.p.terminate()
        except Exception:
            pass


def load_labels(data):
    topics, qrels = {}, {}
    with open(os.path.join(data, "topics.dev.tsv")) as f:
        for line in f:
            qid, q = line.rstrip("\n").split("\t", 1)
            topics[qid] = q
    with open(os.path.join(data, "qrels.dev.tsv")) as f:
        for line in f:
            qid, _, docid, rel = line.rstrip("\n").split("\t")
            if rel == "1":
                qrels.setdefault(qid, set()).add(docid)
    return topics, qrels


def shard_paths(data, shards):
    """Corpus shards that exist locally; download any that do not."""
    paths = []
    for i in range(shards):
        p = os.path.join(data, f"docs-{i}.jsonl")
        gz = p + ".gz"
        if not os.path.exists(p):
            if not os.path.exists(gz):
                url = MIRACL_BASE.format(
                    repo="miracl-corpus",
                    path=f"miracl-corpus-v1.0-zh/docs-{i}.jsonl.gz")
                print(f"fetching shard {i}...", flush=True)
                subprocess.run(["curl", "-sL", "-o", gz, url], check=False)
            if os.path.exists(gz):
                subprocess.run(["gunzip", "-f", gz], check=False)
        if os.path.exists(p):
            paths.append(p)
    return paths


def collect(data, usable, n_distract, shards):
    """Return {docid: (title, text)} covering every gold passage plus noise.

    Single streaming pass over the available shards: gold passages are kept
    when seen, distractors are accumulated until the budget is met.
    """
    gold = set()
    for _, rel in usable:
        gold |= rel
    docs, need = {}, set(gold)
    distract = []
    for path in shard_paths(data, shards):
        with open(path) as f:
            for line in f:
                d = json.loads(line)
                did = d["docid"]
                if did in need:
                    docs[did] = (d.get("title", ""), d.get("text", ""))
                    need.discard(did)
                elif len(distract) < n_distract and len(d.get("text", "")) > 80:
                    distract.append(did)
                    docs[did] = (d.get("title", ""), d.get("text", ""))
                if not need and len(distract) >= n_distract:
                    break
        if not need and len(distract) >= n_distract:
            break
    return docs, gold, need


def build_vault(koios, vault, docs, model_dir):
    shutil.rmtree(vault, ignore_errors=True)
    subprocess.run([koios, "init", vault], capture_output=True)
    raw = os.path.join(vault, "raw")
    if model_dir:
        dst = os.path.join(vault, "models")
        os.makedirs(dst, exist_ok=True)
        for name in ("qwen3-embedding-0.6b-int8-tei.onnx", "tokenizer.json"):
            src = os.path.join(model_dir, name)
            if os.path.exists(src):
                shutil.copy(src, os.path.join(dst, name))
    for did, (title, text) in docs.items():
        safe = did.replace("#", "_").replace("/", "_")
        with open(os.path.join(raw, f"{safe}.md"), "w") as f:
            f.write(f"# {title}\n\n{text[:1200]}\n")


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--koios", default="rust-cli/target/release/koios")
    ap.add_argument("--data", default="/tmp/miracl")
    ap.add_argument("--queries", type=int, default=40)
    ap.add_argument("--skip-layer-check", action="store_true",
                    help="only measure the default route (faster iteration)")
    ap.add_argument("--distractors", type=int, default=300)
    ap.add_argument("--shards", type=int, default=1)
    ap.add_argument("--seed", type=int, default=3)
    ap.add_argument("--vault", default="/tmp/koios_eval_vault")
    ap.add_argument("--model-dir", default=None,
                    help="dir containing the .onnx + tokenizer.json")
    ap.add_argument("--json", default=None, help="write results as JSON")
    ap.add_argument("--allow-missing-gold", action="store_true",
                    help="report even with gold absent (produces a LOWER "
                         "BOUND, not a comparable score)")
    args = ap.parse_args()

    random.seed(args.seed)
    topics, qrels = load_labels(args.data)

    # Which passages are actually available decides which queries are
    # answerable. Selecting queries first and then discovering half their gold
    # is missing is exactly the mistake this script exists to prevent, so the
    # coverage check runs BEFORE sampling.
    available = set()
    for path in shard_paths(args.data, args.shards):
        with open(path) as f:
            for line in f:
                available.add(json.loads(line)["docid"])

    candidates = [(q, qrels[q]) for q in topics
                  if q in qrels and qrels[q] <= available]
    if len(candidates) < args.queries:
        print(f"only {len(candidates)} answerable queries available "
              f"(wanted {args.queries}); scoring those", flush=True)
    random.shuffle(candidates)
    usable = candidates[:args.queries]

    docs, gold, missing = collect(
        args.data, usable, args.distractors, args.shards)

    coverage = 1.0 - (len(missing) / len(gold)) if gold else 1.0
    print(f"docs={len(docs)} gold={len(gold)} "
          f"queries={len(usable)} coverage={coverage:.3f}", flush=True)
    if missing:
        print(f"WARNING: {len(missing)} gold passages not in the "
              f"downloaded shards", flush=True)
        if not args.allow_missing_gold:
            print("Refusing to report: scores would count absent answers as "
                  "misses. Add --shards to download more, or pass "
                  "--allow-missing-gold for a lower bound.", flush=True)
            # Non-zero so CI treats an invalid measurement as failure rather
            # than quietly recording a number nobody can interpret.
            return 2

    build_vault(args.koios, args.vault, docs, args.model_dir)
    t = time.time()
    r = subprocess.run([args.koios, "index", args.vault],
                       capture_output=True, text=True)
    if r.returncode != 0:
        print("index failed:", r.stderr[:400], file=sys.stderr)
        return 1
    print(f"indexed in {time.time()-t:.1f}s", flush=True)

    def measure(tag, layer=None):
        m = Mcp(args.koios)
        h1 = h5 = 0
        try:
            for qid, rel in usable:
                want = {d.replace("#", "_").replace("/", "_") + ".md"
                        for d in rel}
                hits = m.search(args.vault, topics[qid], layer=layer)
                if any(i.split("#")[0] in want for i in hits[:1]):
                    h1 += 1
                if any(i.split("#")[0] in want for i in hits[:5]):
                    h5 += 1
        finally:
            m.close()
        n = len(usable)
        res = {"hit@1": h1 / n, "hit@5": h5 / n}
        print(f"{tag}: hit@1={res['hit@1']:.3f}  "
              f"hit@5={res['hit@5']:.3f}", flush=True)
        return res

    out = {
        "docs": len(docs), "gold": len(gold), "queries": len(usable),
        "gold_coverage": coverage,
        "default": measure("default (all layers)"),
    }
    if not args.skip_layer_check:
        out["layer_raw"] = measure("layer=raw          ", layer="raw")
        print(f"raw-vs-all:  "
              f"hit@1={out['layer_raw']['hit@1']-out['default']['hit@1']:+.3f}  "
              f"hit@5={out['layer_raw']['hit@5']-out['default']['hit@5']:+.3f}",
              flush=True)
    else:
        print("layer check skipped", flush=True)
    # Comparing the two is the §4.3 check: if compile-derived pages crowded
    # out sources, layer=raw would beat the default.

    if args.json:
        with open(args.json, "w") as f:
            json.dump(out, f, indent=2)
        print(f"wrote {args.json}", flush=True)
    return 0


if __name__ == "__main__":
    sys.exit(main())
