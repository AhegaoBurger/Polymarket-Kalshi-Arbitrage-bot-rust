# Matching Pipeline: Prefilter, Ingestion Expansion, LLM Swap

## Background

The AI matching engine (`scripts/ai_matcher`) pairs Kalshi markets with
Polymarket markets across six stages: ingestion → embedding → HNSW retrieval →
LLM verification → overrides → output. Spec
`2026-04-21-multi-category-matching-design.md` established that pipeline. Live
runs since then surface three structural issues that limit its usefulness:

1. **Tiny ingestion sample.** Caps of `INGEST_KALSHI_MAX_EVENTS=200` /
   `INGEST_POLY_LIMIT=500` cover roughly 5% of the addressable universe
   (~10.5K curated Polymarket events, ~5–20K liquid Kalshi markets). The
   intersection on real events ends up too small for the verifier to find
   matches — recent runs report `accepted: 0` across hundreds of candidate
   pairs.

2. **No structural prefilters before retrieval.** Every Kalshi market searches
   the full Polymarket index. A "Will Trump win 2024 nomination?" Kalshi market
   surfaces "Will Trump win 2028 nomination?" on Polymarket as a top-K
   candidate purely on title similarity. Cross-bucket and cross-window pairs
   waste verifier tokens and dilute candidate quality.

3. **Hardwired Anthropic verifier.** `verifier.Verifier` imports `anthropic`
   directly and uses Claude's native tool-use shape. Switching providers
   requires rewriting the request/response code — and the verifier is the
   dominant per-run cost driver, so it's the place where provider choice
   matters most.

Lifting the cap without prefilters blows up cost; adding prefilters without
lifting the cap doesn't fix the recall problem. Both changes are co-designed.

## Goals

- Add **structural prefilters** that reject pairs by category and by expiry
  window before they reach the LLM verifier.
- **Raise ingestion caps** to cover the addressable universe (~10K Polymarket,
  ~2K Kalshi events to start), with pagination on both sides and a
  volume-based liquidity proxy on Kalshi.
- **Decouple the verifier from a single provider** via LiteLLM, so changing
  models is one env-var change.
- Make the pipeline's funnel **observable end-to-end**, so the operator can
  see exactly where pairs get filtered and tune the right knob.

## Out of scope (explicit deferrals)

- **Bidirectional retrieval flip** (Polymarket → Kalshi as outer loop). User
  agrees the flip is correct; tracked as a separate spec. This spec keeps the
  current Kalshi → Polymarket direction.
- **PMXT integration.** The free SDK doesn't normalize categories (audit
  confirmed the SDK passes venue categories through verbatim). The paid
  catalog is deferred for a future spec.
- **Authenticated Kalshi liquidity.** Public REST returns `null` for
  `liquidity`; we use `volume` as a proxy. Authenticated calls are a separate
  spec.
- **Bulk-endpoint Kalshi ingestion.** The per-event walk is slower but
  returns `rules_primary` (resolution criteria). The verifier needs that
  field; the bulk endpoint omits it.

## Architecture

```
ingestion.fetch_all
   ├─ parse_kalshi_markets_response
   │     ├─ NEW: parse close_time → close_time_utc (drop on parse failure)
   │     └─ NEW: resolve category string → CategoryBucket (or "Unknown")
   └─ parse_poly_gamma_markets_response
         ├─ NEW: parse endDateIso/endDate → close_time_utc (drop on parse failure)
         └─ NEW: resolve category string → CategoryBucket (or "Unknown")
                                       │
                                       ▼
embedder.embed (unchanged)
                                       │
                                       ▼
retrieval.BucketedHnswRetrieval                  ◄── REPLACES HnswRetrieval
   ├─ NEW: per-bucket index on the Polymarket side + "_full" fallback
   ├─ NEW: query routes by Kalshi-side bucket
   └─ (unchanged: top_k, min_cosine semantics)
                                       │
                                       ▼
pipeline.run_pipeline                            ◄── adds date-overlap predicate
   └─ NEW: drop pairs where |Δexpiry| > tolerance_days * EXPIRY_TOLERANCE_SCALE
                                       │
                                       ▼
verifier.Verifier                                ◄── REWRITTEN
   ├─ NEW: LiteLLM client (model from LLM_MODEL env var)
   ├─ Tool-use schema migrated to OpenAI shape
   ├─ Cache key: model | k.hash | p.hash (model string includes provider prefix)
   └─ Cost-per-call captured into audit log
                                       │
                                       ▼
overrides + acceptance + output (unchanged)
```

Two structural choices baked into this architecture:

1. **Bucketing happens at ingestion**, not at query time. The bucket name is
   stamped onto each `Market` once. Cleaner audit log; trivial perf cost;
   config changes need a re-run (acceptable since the pipeline runs on TTL).
2. **Per-bucket HNSW indexes on the Polymarket side**, not a single index with
   post-filter. A Politics-bucketed Kalshi market queries the Politics-only
   Polymarket index — its top-8 are all viable Politics candidates, instead
   of being polluted by cross-bucket noise. Recall and verifier cost both
   improve.

## Design

### 1. Category equivalence config

**File**: `config/category_equivalence.json` (sibling of
`config/manual_overrides.json`). Single file holds bucket membership and
date tolerance, since they're co-evolving metadata.

```json
{
  "buckets": {
    "Politics":     { "kalshi": ["Politics"],            "poly": ["Politics"],             "tolerance_days": 60 },
    "Sports":       { "kalshi": ["Sports"],              "poly": ["Sports"],               "tolerance_days": 2  },
    "Weather":      { "kalshi": ["Climate and Weather"], "poly": ["Weather"],              "tolerance_days": 2  },
    "Crypto":       { "kalshi": ["Crypto", "Bitcoin"],   "poly": ["Crypto", "Bitcoin"],    "tolerance_days": 1  },
    "Economics":    { "kalshi": ["Economics"],           "poly": ["Finance", "Economics"], "tolerance_days": 14 },
    "Tech":         { "kalshi": ["Tech"],                "poly": ["Tech"],                 "tolerance_days": 30 },
    "Mentions":     { "kalshi": ["Mentions"],            "poly": ["Mentions"],             "tolerance_days": 7  },
    "Culture":      { "kalshi": ["Culture"],             "poly": ["Culture"],              "tolerance_days": 30 },
    "Geopolitical": { "kalshi": ["World"],               "poly": ["Geopolitical"],         "tolerance_days": 30 }
  },
  "default_tolerance_days": 30
}
```

Top-level bucket names match the existing `PolyCategory` enum in
`src/fees.rs:86` so Rust and Python share the vocabulary.

**Bucket-resolution algorithm** (`ai_matcher/categories.py`, called from
parsers in `ingestion.py`):

```python
def resolve_bucket(market) -> str:
    candidates = [market.category]
    if market.platform == "polymarket":
        candidates.extend(market.tags)            # Polymarket tag fallback
    candidates = [c.strip().lower() for c in candidates if c]
    if not candidates:
        return "Unknown"
    for bucket_name, cfg in config["buckets"].items():
        platform_aliases = [a.lower() for a in cfg[market.platform]]
        if any(c in platform_aliases for c in candidates):
            return bucket_name
    return "Unknown"
```

Properties: case-insensitive match, whitespace-trimmed, Polymarket tag
fallback when `category` is empty (mirrors PMXT's open-source SDK), first
match wins on multi-bucket ambiguity.

**Loader.** A small helper `load_category_config(path)` runs once at pipeline
startup; the config is held in memory. Missing or malformed config →
fallback to "every market is Unknown" (current behavior) with a `WARN` log
line. The pipeline still runs, just without the prefilter.

**Data model change.** Add `bucket: str` to the `Market` dataclass at
`scripts/ai_matcher/src/ai_matcher/ingestion.py:36`. Populated during
`parse_kalshi_markets_response` and `parse_poly_gamma_markets_response`.

### 2. Date overlap predicate and UTC normalization

**UTC parser** (in `ingestion.py`):

```python
def parse_close_time_utc(raw: dict, platform: str) -> datetime | None:
    if platform == "kalshi":
        s = raw.get("close_time")
    else:
        s = raw.get("endDateIso") or raw.get("endDate")
    if not s:
        return None
    try:
        dt = datetime.fromisoformat(s.replace("Z", "+00:00"))
    except (ValueError, TypeError):
        return None
    if dt.tzinfo is None:
        return None  # refuse to guess on naive datetimes
    return dt.astimezone(timezone.utc)
```

Markets where this returns `None` are **dropped during ingestion**. Per-platform
counters surface the count in the run summary.

**Data model change.** Add `close_time_utc: datetime` (non-optional, tz-aware)
to `Market`. Any market reaching the embedder has a valid UTC expiry.

**The predicate** (runs after retrieval, before the verifier):

```python
def date_overlap_ok(k: Market, p: Market, cfg: CategoryConfig, scale: float) -> bool:
    bucket = k.bucket if k.bucket != "Unknown" else p.bucket
    tol_days = (
        cfg.buckets[bucket].tolerance_days
        if bucket in cfg.buckets
        else cfg.default_tolerance_days
    )
    delta_seconds = abs((k.close_time_utc - p.close_time_utc).total_seconds())
    return delta_seconds <= tol_days * scale * 86_400
```

**Global scale knob.** `EXPIRY_TOLERANCE_SCALE` env var, float, default `1.0`.
Multiplies every per-bucket tolerance. Values `<=0` log a warning and fall back
to `1.0`.

**Both-Unknown case.** If `k.bucket == "Unknown"` and `p.bucket == "Unknown"`
(both markets failed to bucket), the predicate uses `default_tolerance_days`
(currently 30). This is intentional: Unknown markets shouldn't be dropped
just because they couldn't be classified, but they also shouldn't get the
loosest possible tolerance — `default_tolerance_days` is the conservative
middle.

### 3. Bucket-aware retrieval

Replace `HnswRetrieval` (`scripts/ai_matcher/src/ai_matcher/retrieval.py`)
with `BucketedHnswRetrieval`. Same constructor surface (`dim`, `top_k`,
`min_cosine`). Two new methods:

```python
class BucketedHnswRetrieval:
    UNBUCKETED = "_full"   # sentinel for Unknown-Kalshi fallback

    def build(self,
              polys_by_bucket: dict[str, list[tuple[np.ndarray, str]]],
              all_polys: list[tuple[np.ndarray, str]]) -> None:
        for bucket, items in polys_by_bucket.items():
            if items:
                self._build_one(bucket, items)
        if all_polys:
            self._build_one(self.UNBUCKETED, all_polys)

    def query(self, vector: np.ndarray, bucket: str) -> list[tuple[str, float]]:
        if bucket != "Unknown" and bucket in self._indexes:
            target = bucket
        elif bucket == "Unknown" and self.UNBUCKETED in self._indexes:
            target = self.UNBUCKETED
        else:
            return []  # known bucket but no Polys there → empty (deliberate no-op)
        index, ids = self._indexes[target], self._ids[target]
        labels, distances = index.knn_query(vector, k=min(self.top_k, len(ids)))
        return [(ids[l], 1.0 - float(d))
                for l, d in zip(labels[0], distances[0])
                if 1.0 - float(d) >= self.min_cosine]
```

Each Polymarket market lives in at most 2 indexes (its bucket + `_full`).
Memory cost: ~30 MB at 10K markets × 384 dims × 4 bytes × 2 (duplicated into
both bucket index and `_full`). Negligible relative to the embedder's resident
size; called out so future cap raises (e.g. to 50K) can be costed honestly.

**Routing branches:**

| Kalshi bucket | Poly bucket index exists | Behavior |
|---|---|---|
| Known (`Politics`) | Yes | Query that bucket's index |
| Known (`Politics`) | No (zero Polys this run) | Return empty |
| `Unknown` | N/A | Query `_full` (current pre-spec behavior) |

**Pipeline integration.** In `pipeline.run_pipeline`, the existing
`for k in result.kalshi:` loop changes minimally —
`retrieval.query(k_vec)` becomes `retrieval.query(k_vec, k.bucket)`. Bucket
grouping for `build()` is a one-pass `defaultdict(list)` over `result.poly`.

### 4. LiteLLM verifier swap

**Dependency.** Add `litellm` to `pyproject.toml`. Drop `anthropic` from
runtime deps (only `verifier.Verifier` and `pipeline.run_pipeline_default`
imported it).

**Tool-use schema in OpenAI shape:**

```python
VERIFIER_TOOL = {
    "type": "function",
    "function": {
        "name": "report_match_decision",
        "description": "Report whether two prediction markets resolve to identical outcomes.",
        "parameters": {                  # was "input_schema" in Anthropic shape
            "type": "object",
            "properties": { ... },        # unchanged
            "required": [ ... ],          # unchanged
        }
    }
}
```

The properties + required keys are byte-identical; only the wrapper changes.

**Verifier rewrite.** Drop the Anthropic-specific client; take a model string
and call LiteLLM:

```python
import litellm

class Verifier:
    def __init__(self, model: str, cache_path: Path | None = None) -> None:
        self.model = model
        self.cache_path = cache_path
        self._cache = self._load_cache()
        ...

    def verify(self, kalshi: Market, poly: Market) -> Decision:
        key = self._cache_key(kalshi, poly)
        if key in self._cache:
            self.cache_hits += 1
            return Decision(**self._cache[key])
        self.cache_misses += 1
        resp = litellm.completion(
            model=self.model,
            messages=[
                {"role": "system", "content": SYSTEM_PROMPT},
                {"role": "user",   "content": user_prompt(kalshi, poly)},
            ],
            tools=[VERIFIER_TOOL],
            tool_choice={"type": "function", "function": {"name": "report_match_decision"}},
            num_retries=3,
        )
        tool_call = resp.choices[0].message.tool_calls[0]
        tool_input = json.loads(tool_call.function.arguments)
        cost = resp._hidden_params.get("response_cost", 0.0)
        decision = Decision(...)
        self._cache[key] = asdict(decision) | {"cost_usd": cost}
        ...
```

**Cache key.** Stays `model | k.hash | p.hash`. Because LiteLLM model strings
include the provider prefix (`deepseek/...`, `gpt-4.1-mini`,
`anthropic/claude-opus-4-7`), swapping providers naturally invalidates only
the relevant cache entries.

**Config.** Single env var: `LLM_MODEL` (default `gpt-4.1-mini`). Each
provider's standard API-key env var (`OPENAI_API_KEY`, `DEEPSEEK_API_KEY`,
`ANTHROPIC_API_KEY`) is whatever LiteLLM expects for that provider. The
pipeline fails loudly at startup if the required key is missing.

**Default model rationale.** `gpt-4.1-mini` hits a sweet spot:
~50× cheaper than Opus, more rigorous tool-use compliance than DeepSeek.
DeepSeek (~3× cheaper still) is one env-var change away once audit data
shows quality is acceptable.

**`EmbeddingsOnlyVerifier` stays unchanged.** No LLM, no LiteLLM. The
existing `_call_verifier` isinstance dispatch handles routing.

### 5. Ingestion expansion

**5a. Polymarket: cap → 10,000 markets, with offset pagination.**

```python
def fetch_poly(self) -> list[Market]:
    out: list[Market] = []
    page_size = 500
    for offset in range(0, self.poly_fetch_limit, page_size):
        resp = self._http.get(
            f"{GAMMA_API_BASE}/markets"
            f"?limit={page_size}&offset={offset}"
            f"&active=true&closed=false"
            f"&order=liquidity&ascending=false"
        )
        resp.raise_for_status()
        body = resp.json() if isinstance(resp.json(), list) else []
        if not body:
            break
        out.extend(parse_poly_gamma_markets_response(body, min_liquidity_usd=self.min_liquidity_usd))
    return out
```

`order=liquidity desc` is preserved across pages, so we still get the most-liquid 10K.

**5b. Kalshi: cap → 2,000 events with cursor pagination.**

```python
def fetch_kalshi(self) -> list[Market]:
    cursor = ""
    raw_events: list[dict] = []
    while len(raw_events) < self.max_kalshi_events:
        url = f"{KALSHI_API_BASE}/events?limit=200&status=open"
        if cursor:
            url += f"&cursor={cursor}"
        resp = self._http.get(url)
        resp.raise_for_status()
        body = resp.json()
        page = body.get("events", []) or []
        if not page:
            break
        raw_events.extend(page)
        cursor = body.get("cursor", "")
        if not cursor:
            break
    kept_events = [e for e in (parse_kalshi_event(e) for e in raw_events) if e is not None]
    kept_events = kept_events[: self.max_kalshi_events]
    # ...rest of per-event /markets walk unchanged
```

Default 2,000 (was 200). At ~100ms per event-markets call, this is ~3–4
minutes of ingestion wall-time on first run. Bumping to 5K is a config
change, not code.

**5c. Kalshi volume-based liquidity proxy.**

`MIN_VOLUME_USD` env var (default `1000`). Updated parser predicate:

```python
liq_known = liq_cents is not None
vol_known = vol_cents is not None
if liq_known and liq_usd < min_liquidity_usd:
    continue                           # known liquidity below floor → drop
if not liq_known and vol_known and vol_usd < min_volume_usd:
    continue                           # liquidity unknown, volume known and low → drop
# both unknown: pass through (rare; verifier will catch obvious junk)
```

**5d. Pagination warning.** `WARN` log if a paginator terminates early due
to HTTP error (cursor-empty / offset-exhausted is normal; HTTP error is not).

### 6. Audit log and observability

**Per-stage funnel counters in the run summary.** `run_pipeline` returns:

```python
{
  "ingested":         {"kalshi": 4823, "poly": 9847},
  "drops_at_ingest":  {"kalshi_missing_date": 12, "poly_missing_date": 0,
                       "kalshi_low_volume": 3401, "poly_low_liquidity": 0},
  "bucketed":         {"Politics": 1140, "Sports": 980, "Economics": 320, ..., "Unknown": 87},
  "candidates_after_retrieval": 14_220,
  "drops_at_date_overlap":      1_840,
  "verifier_calls":             12_380,
  "verifier_cache_hits":        8_902,
  "verifier_cost_usd":          1.93,
  "accepted": 412, "rejected": 11_968, "rows": 12_380
}
```

**Per-pair audit log additions** (`.ai_matcher_audit.jsonl`):

```python
{
    "ts": ...,
    "kalshi": k.ticker, "poly": p.condition_id,
    "decision": "accept" | "reject",
    "reject_reason": "expiry-gap" | "verifier" | None,
    "bucket_kalshi": k.bucket,
    "bucket_poly":   p.bucket,
    "cosine":        round(cosine, 4),
    "delta_days":    round(...) if both have dates else None,
    "tolerance_days": tol,
    "confidence":    decision.confidence,
    "concerns":      decision.concerns,
    "reasoning":     decision.reasoning,
    "override":      override.value,
    "model":         self.model,
    "cost_usd":      decision_cost,
}
```

`reject_reason` lets you grep / `jq` to see why each pair died. Note that
**cross-bucket pairs never produce an audit row at all** — bucket routing
happens inside `BucketedHnswRetrieval.query`, so a Kalshi-Politics market
never sees Sports candidates in the first place. They're absent from the
JSONL by construction, not present-with-reject-reason.

Per-market drops (missing date, low volume) go to `tracing` logs at
INFO/WARN, plus the per-platform counters above. They are NOT in the JSONL
(per-pair file).

**Report HTML additions.** `audit/report.html` rows already show both
markets' full text and the verifier reasoning. Add columns:

- **Bucket pair** (e.g. `Politics → Politics`)
- **Cosine** (3 decimals)
- **Δdays** (absolute expiry delta)

### 7. Interactive HTML report

`audit/report.html` becomes a single self-contained interactive page.

**7a. Click-to-sort column headers.** Every `<th>` carries a `data-sort`
attribute (`"numeric"` or `"string"`). Numeric: `cosine`, `Δdays`,
`confidence`, `cost_usd`. String/categorical: `kalshi_ticker`, `poly_slug`,
`bucket_kalshi`, `bucket_poly`, `decision`, `reject_reason`. Clicking
toggles asc → desc → asc; an arrow indicator (`▲`/`▼`) shows the active
column.

**7b. Free-text filter input** above the table. Single `<input>`,
case-insensitive substring match across every visible cell. Debounced
~150ms.

**7c. Sticky header row** (`position: sticky; top: 0;`) so column labels
stay visible while scrolling.

**Inline JS skeleton (~40 lines, no deps):**

```javascript
const tbody = document.querySelector('tbody');
const rows = [...tbody.querySelectorAll('tr')];

document.querySelectorAll('th[data-sort]').forEach((th, i) => {
  th.style.cursor = 'pointer';
  th.addEventListener('click', () => {
    const numeric = th.dataset.sort === 'numeric';
    const dir = th.classList.contains('asc') ? -1 : 1;
    document.querySelectorAll('th').forEach(h => h.classList.remove('asc', 'desc'));
    th.classList.add(dir === 1 ? 'asc' : 'desc');
    rows.sort((a, b) => {
      const av = a.children[i].textContent.trim();
      const bv = b.children[i].textContent.trim();
      return numeric ? (parseFloat(av) - parseFloat(bv)) * dir : av.localeCompare(bv) * dir;
    });
    rows.forEach(r => tbody.appendChild(r));
  });
});

const filter = document.getElementById('filter');
let filterTimer;
filter.addEventListener('input', () => {
  clearTimeout(filterTimer);
  filterTimer = setTimeout(() => {
    const q = filter.value.trim().toLowerCase();
    rows.forEach(r => {
      r.style.display = !q || r.textContent.toLowerCase().includes(q) ? '' : 'none';
    });
  }, 150);
});
```

CSS polish: `.asc::after { content: ' ▲'; }`, `.desc::after { content: ' ▼'; }`,
alternating row colors, sticky header.

`audit-sample-N.html` (the spot-check view from `pipeline._render_audit_sample`)
gets the same sort + filter treatment.

**Deliberately not added:** multi-column sort, regex filter, CSV/JSON export
button, collapsible rows, pagination.

## Configuration recap

| Var | Default | Was | Effect |
|---|---|---|---|
| `INGEST_POLY_LIMIT` | `10000` | `500` | Polymarket pages fetched |
| `INGEST_KALSHI_MAX_EVENTS` | `2000` | `200` | Kalshi events walked |
| `MIN_LIQUIDITY_USD` | `100` | `100` | unchanged |
| `MIN_VOLUME_USD` | `1000` | (new) | Kalshi volume floor when liquidity unknown |
| `EXPIRY_TOLERANCE_SCALE` | `1.0` | (new) | Global multiplier on per-bucket tolerances |
| `LLM_MODEL` | `gpt-4.1-mini` | (replaces hardcoded `claude-opus-4-7`) | LiteLLM-formatted model string |
| `OPENAI_API_KEY` | — | (was `ANTHROPIC_API_KEY`) | Provider key — varies by selected `LLM_MODEL` |
| `EMBEDDINGS_ONLY` | unchanged | unchanged | unchanged |
| `EMBEDDINGS_ACCEPT_COSINE` | unchanged | unchanged | unchanged |

**New file**: `config/category_equivalence.json` (committed default; live-edit
to expand or rebalance buckets).

## Testing

**New unit tests** (added to existing per-module files):

- `test_ingestion.py`: UTC parsing (good, missing, naive, `Z` suffix);
  volume-proxy filter (liquidity-known/null × volume-known/null matrix);
  Polymarket offset pagination (3-page stub); Kalshi cursor pagination
  (2-page stub).
- `test_categories.py` (new): bucket resolver — exact match, case-insensitive,
  whitespace-trimmed, alias match, Polymarket tag fallback, no match →
  `Unknown`, malformed config → empty config + warning.
- `test_retrieval.py`: `BucketedHnswRetrieval` routing — Politics → Politics
  index, Unknown → `_full`, known bucket with no Polys → empty,
  `min_cosine` enforced per index.
- `test_verifier.py`: LiteLLM stub via `monkeypatch` of `litellm.completion`;
  cache key invalidates on model change; cost field flows into cache entry.
- `test_pipeline.py`: date overlap predicate end-to-end (within / beyond
  tolerance); per-bucket routing end-to-end; funnel counters add up correctly.
- `test_report.py`: HTML output contains sort markers (`<th data-sort=...>`),
  filter input, and the sort/filter JS block.

**Integration test.** Synthetic 4×4 dataset across 2 buckets, `EmbeddingsOnlyVerifier`,
asserts: same-bucket same-date pair → accepted; same-bucket different-date
pair → dropped at date overlap with `reject_reason == "expiry-gap"`; cross-bucket
pair → no audit row (never enters retrieval); funnel counters consistent.

**Mocking pattern** (used in every LiteLLM-touching test):

```python
def test_verifier_parses_litellm_response(monkeypatch):
    fake_resp = SimpleNamespace(
        choices=[SimpleNamespace(message=SimpleNamespace(
            tool_calls=[SimpleNamespace(function=SimpleNamespace(
                arguments=json.dumps({
                    "confidence": 0.95, "resolution_match": True,
                    "concerns": [], "reasoning": "...",
                    "category": "Politics", "event_type": "Election",
                }),
            ))]
        ))],
        _hidden_params={"response_cost": 0.0007},
    )
    monkeypatch.setattr("litellm.completion", lambda **kw: fake_resp)
    ...
```

**Pre-merge checklist:**

1. `uv run pytest` clean from `scripts/ai_matcher`.
2. Manual run with `EMBEDDINGS_ONLY=1` against live APIs to confirm pagination.
3. Manual run with `LLM_MODEL=gpt-4.1-mini` and small `INGEST_KALSHI_MAX_EVENTS=200`
   to spot-check cost rollup before raising caps.
4. Open the new `audit/report.html` and verify sort + filter work in browser.

## Cost & wall-time after rollout

**First run after spec ships**: ~3–5 min Kalshi ingestion + ~30s Polymarket
ingestion + ~5–10 min embedding (10K Polys + ~5K Kalshi markets on CPU) +
LLM verifier on prefilter survivors. Roughly $12–15 in LLM calls.

**Subsequent runs**: embedder cache + verifier cache hit on every unchanged
market/pair, so only the *delta* gets compute. Typical: tens of seconds +
low single-digit dollars.

## Rollback plan

Every new env var defaults safely:

- `category_equivalence.json` missing → fallback to "every market is Unknown"
  → `Unknown` always uses `_full` index → behavior matches pre-spec retrieval.
- `INGEST_POLY_LIMIT` / `INGEST_KALSHI_MAX_EVENTS` can be set back to 500/200.
- `EXPIRY_TOLERANCE_SCALE` can be set very high (e.g., 1000) to effectively
  disable the date filter.
- `LLM_MODEL=anthropic/claude-opus-4-7` (with `ANTHROPIC_API_KEY`) restores
  the prior verifier exactly.

## File-level change list

```
scripts/ai_matcher/pyproject.toml
  + add litellm
  - remove anthropic

scripts/ai_matcher/src/ai_matcher/
  ingestion.py      [modified] add close_time_utc, bucket fields; UTC parser; volume proxy; pagination
  categories.py     [new]      load_category_config + resolve_bucket
  retrieval.py      [modified] HnswRetrieval → BucketedHnswRetrieval
  verifier.py       [modified] LiteLLM client; OpenAI tool schema; cost capture
  pipeline.py       [modified] date overlap predicate; per-stage counters; bucket routing
  report.py         [modified] new columns; sort + filter JS; sticky header

scripts/ai_matcher/tests/
  test_ingestion.py     [modified] new cases per spec
  test_categories.py    [new]
  test_retrieval.py     [modified] BucketedHnswRetrieval cases
  test_verifier.py      [modified] LiteLLM mocking
  test_pipeline.py      [modified] funnel + date overlap
  test_report.py        [modified] markup checks

config/category_equivalence.json   [new]
```

The Rust consumer side (`src/discovery.rs`, `src/adapters/ai_reader.rs`,
`src/config.rs`) is **unchanged**. The `.ai_matches.json` schema produced
by the sidecar is unchanged.
