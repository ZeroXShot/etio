# Algorithms

This page describes what Etio computes and why, in the order data flows
through it. Every section names the code that implements it.

## 1. From telemetry to series

### Traces (`etio-pipeline::trace`)

A completed trace is analysed as a tree:

* **Clock-skew correction.** Spans come from different hosts whose clocks
  disagree. A child that starts before its parent or ends after it is shifted
  into the parent (the correction used by Jaeger), top-down, so that the
  derived timings are consistent. Corrected traces are counted
  (`skew_adjusted`).
* **Service-local time.** The latency of a request includes the time spent
  waiting for downstream services. The *local* (self) time of a span is its
  duration minus the union of its children's intervals (computed on the
  corrected timeline). A slow service shows high local time; its callers only
  show high total latency. This is the single most discriminating signal for
  latency faults.
* **Error origins.** An error that propagates up a call chain is attributed
  to the deepest erroring span. A client span that errored or got no answer
  from a service that emitted nothing (it crashed, or it is a datastore
  without tracing) is attributed to the *named peer* (`server.address`,
  `peer.service`, `db.system`...), so that a dead dependency is still a
  candidate.
* **Call edges.** Every outbound client span yields one caller → callee edge
  with the latency and outcome *as the client measured them*. Client-side
  measurements include the network, so they capture delay and packet loss
  that the callee's own spans cannot see, and they exist even when the callee
  is silent. They become the callee's `inbound_*` series.

### Logs (`etio-pipeline::logs`)

Log bodies are clustered into templates with **Drain** (a fixed-depth parse
tree keyed by token count and leading tokens; masked variables such as
numbers, UUIDs and IPs count as matches). Per service and window Etio counts
lines, error lines and lines of *novel* templates (first seen after the
warm-up, within the novelty horizon). Severity comes from the OTLP severity
number when present, otherwise from keywords, including exception names
(`...Exception`, `...Error`). Template tables are LRU-bounded per service.

### Metrics

Gauges are averaged per window; delta sums are summed. Cumulative sums
(counters) are converted, per stream, into a per-second **rate over the
interval since the stream's previous point**, with resets detected (new
start time or decreasing value); a window's value is the sum over streams of
their mean rate. Summing increments per window instead would make the value
depend on how exports align with windows: with a 5 s export interval and
5 s windows, clock jitter leaves some windows without an export (a false drop
to zero) and gives others two.

Metrics that describe the **host** rather than the service (OpenTelemetry's
`system.*`, which language runtimes report from inside every container with
host-wide values, and node-exporter's `node_*`) are kept as evidence but
never open incidents: services sharing a host all report them, so a noisy
neighbour would otherwise look like every service failing at once. Metric names are mapped to categories (CPU, memory,
latency, errors, traffic, disk, network, connections, runtime) by
`etio-core::signal::classify_metric_name`, which also sets the harmful
direction (for most signals, up is bad).

## 2. Detection (`etio-analysis::detect`)

Each series has a detector that sees one value per window.

**Robust standardisation.** The median and MAD of a sliding baseline
(240 windows) give `z = (x − median) / (1.4826 · MAD)`. The exact sliding
median/MAD is maintained in `O(log n)` per update
(`etio-core::stats::window::SortedWindow`: a sorted vector plus a
"k-th element of two sorted arrays" search for the MAD). The scale has
*relative floors* (a fraction of the median and of the series' long-run
magnitude): an error rate that has always been 0 would otherwise give an
infinite `z` on its first error. Sparse series whose MAD is zero use the
standard deviation.

**Extreme-value thresholds (SPOT).** The harmful part of `z` feeds a SPOT
detector (Siffer et al., KDD 2017): peaks above an initial high quantile are
fitted with a generalised Pareto distribution (maximum likelihood by
Grimshaw's method), which gives the threshold exceeded with a chosen
probability. Every series therefore alarms at the same configured risk,
whether its tails are light or heavy.

*A bias we found and fixed:* SPOT normally refuses to learn from values that
raised alarms. Since alarms are exactly the largest values, the tail model is
then fitted on censored data, underestimates the tail, and the false-alarm
rate grows (we measured about 4× the configured risk on heavy-tailed
synthetic data). Etio counts alarms in the exceedance rate and feeds short,
isolated episodes back into the tail model once they end (*deferred
learning*, `Spot::learn`). Long episodes stay out, so real incidents do not
teach the model that incidents are normal.

**Sustained shifts (CUSUM).** A one-sided CUSUM on `z` catches moderate
shifts that never cross the extreme threshold and estimates when they began.
Residuals are winsorised at ±4 before entering the sum (a Huber-type robust
CUSUM): otherwise a single huge outlier in a sparse series would hold the
CUSUM in alarm for thousands of windows.

**Degenerate baselines.** When more than half of the baseline values are
equal (a queue that is almost always empty), no parametric model is
credible: the detector reports a **conformal p-value**,
`(1 + #{baseline values at least as bad}) / (n + 1)`, which is valid without
distributional assumptions. A constant baseline turns any change into an
*event* of bounded strength.

**Freezing.** During an episode the baseline is frozen so the anomaly is not
absorbed into "normal"; if the new level lasts longer than
`freeze_max_points`, it is accepted as the new normal (a deployment that
changed latency for good).

**Surprise.** Detectors report `−log10 p` (capped), an additive and
comparable measure. A service is anomalous in a window when one of its
series has been anomalous for `confirm_windows` consecutive windows with
enough surprise. Traffic changes count only at entry points, where they
measure what users experience; elsewhere they are symptoms. An incident opens
when several services are anomalous together, when one is severely
anomalous, or when an entry point's latency, errors or traffic are: an
entry point's internal signals (thread counts, memory) do not open
incidents on their own.

## 3. Root-cause analysis (`etio-analysis::rca`)

An analysis looks at every series of every service over a **reference**
period (before the incident) and an **abnormal** period (from the incident
start), and ranks services.

### Series scores (`rca::score`)

Each series is scored against its own reference period with robust
statistics: the peak and the sustained deviation (median of the harmful residuals) of the abnormal
period in robust standard deviations, the onset (first sustained crossing),
and the level shift. The scale uses the reference period only (using the
abnormal period would let the anomaly inflate its own yardstick) with
relative floors and a standard-deviation fallback for sparse series.

Two situations get special treatment because they are common and otherwise
invisible:

* a series whose reference period is **constant** (zero errors) scores by how
  persistently the abnormal period deviates, not by an infinite `z`;
* a series that **went silent** (it reported steadily before and stopped
  when the incident began) counts as evidence: a crashed service emits
  nothing, and "nothing" must not look healthy.

For comparison Etio also implements the published baselines faithfully:
**BARO** (median/IQR scaling, Pham et al., FSE 2024) and **N-Sigma**
(mean/standard deviation), validated against the official implementations
(see [evaluation](evaluation.md)).

### Features (`rca::features`)

Series scores are summarised into 20 **scale-free** features per service:
the peak and sustained anomaly (log-scaled), the peak relative to the
strongest service, the rank by peak, per-category evidence (resources,
latency, errors, traffic, logs, local time), the share of anomalous and of
silent signals, whether and how early its anomaly started compared to other
services, the anomaly of its callers and callees, whether its callees
explain its own anomaly, its score in a random walk on the service graph,
and whether it is an entry point or in the graph at all.

Scale-free features are what allow one model to work across systems whose
services, metrics and magnitudes differ.

### Graph random walk (`graph::anomaly_random_walk`)

A random walk with restart on the call graph, where the walker moves from a
service to its callees in proportion to their anomaly, with restarts at
anomalous services. It concentrates probability on anomalous services whose
anomaly is not explained by anomalous dependencies: the classical
MicroRCA/MonitorRank idea, used here as one feature rather than as the
ranker.

### The ranker (`rca::model`)

The ranker is a **linear model over the features**, trained listwise: the
probability that service *i* is the root cause is the softmax of its score
(ListNet top-1 / multinomial logit), and training maximises the likelihood
of the true root cause of each incident.

* Training uses exact Newton steps (the objective is convex and there are
  only 20 parameters).
* Regularisation is a Gaussian prior centred on the **hand-set heuristic
  weights** rather than on zero (maximum a posteriori). With little data the
  model stays close to the expert prior; with more data it departs from it
  where the data says so.
* The penalty strength is selected by nested leave-one-system-out
  cross-validation (see [evaluation](evaluation.md)).

The default ranker is an **ensemble**: the sum of the standardised scores of
the learned model and of the heuristic model (a product of experts). It is
slightly worse than the learned model alone in cross-validation, but it
bounds the damage if the learned weights do not transfer to a new system.

A linear model was chosen deliberately over gradient-boosted trees or graph
neural networks: with ~700 labelled incidents from three systems, a
20-parameter model is what the data can support, it transfers across
systems, and it is exactly explainable (ADR 0004).

### Explanations (`rca::explain`)

Because the model is linear, a service's score decomposes exactly into
per-feature **contributions** (weight × standardised feature). The UI shows
them, and deterministic **reasons** are generated from the numbers behind
them ("its own resources are saturated (up to 35 deviations): the problem is
local, not inherited"). Every sentence is traceable to a feature value or a
series score; nothing is generated by a language model.

## 4. Statistics primitives (`etio-core::stats`)

| primitive | use | notes |
|---|---|---|
| DDSketch | latency quantiles per window | relative error 1 %, bounded bins (lowest collapsed), mergeable: the basis of distributed aggregation |
| `SortedWindow` | sliding median, MAD, IQR, quantiles | exact; `O(log n)` MAD |
| GPD / SPOT | extreme-value thresholds | Grimshaw MLE; the exponential model (ξ = 0) competes as a candidate; Gaussian fallback while calibrating |
| robust CUSUM | sustained shifts | winsorised input |
| conformal p-values | degenerate baselines | distribution-free |
| xoshiro256++ | every random choice | seeded, stable across platforms and versions, so simulations and tests are reproducible |

The property tests in each module compare the streaming structures with
batch recomputation on random inputs.
