# Evaluation

This page reports how well Etio localises root causes, how that was
measured, and what did not work. Everything here can be reproduced with
`make eval` (downloads about 3 GB) and `etio sim bench`.

## Summary

* On **RCAEval** (733 usable cases, three systems), the default ranker
  reaches **0.930 Avg@5** in nested leave-one-system-out cross-validation
  (the learned model alone 0.938, the hand-set heuristic 0.905, ranking by
  the largest anomaly alone 0.854), with 95 % bootstrap intervals below.
* Against the published baselines run on identical inputs, Etio's
  heuristic ranker, which uses no training data, is better than or on par
  with every baseline on every suite, with one dataset where it is worse
  than N-Sigma (RE3-TT). The learned ranker is better on all three suites.
* On the built-in simulator, where detection can be measured too, every
  injected fault was detected, and the faulty service was ranked first in
  83–96 % of first analyses and in the top three in all of them.
* One idea that looked promising made things worse, one diagnosis of a
  failure was wrong, a first version scaled scores in a way that let
  anomalies hide themselves, and real SDK telemetry exposed three bugs the
  simulator could not. All are documented below.

## Benchmark

[RCAEval](https://github.com/phamquiluan/RCAEval) (Pham et al., "RCAEval: A
Benchmark for Root Cause Analysis of Microservice Systems with Telemetry
Data", WWW 2025 companion) contains 735 fault-injection cases on three
open-source systems: **Online Boutique** (OB), **Sock Shop** (SS) and
**Train Ticket** (TT), with about 13, 15 and 64 candidate services per case
respectively. Faults target 7, 6 and 5 distinct services of each system;
since Etio's features never include service identities, a model cannot
learn which services tend to be faulty.

| suite | cases | telemetry | faults |
|---|---:|---|---|
| RE1 | 375 | metrics | CPU, memory, disk, network delay, packet loss |
| RE2 | 270 | metrics, logs, traces | the above plus socket exhaustion |
| RE3 | 90 | metrics, logs, traces | code-level faults (bugs injected in the application) |

Two RE1 cases are excluded because their data is defective:
`re1ob_currencyservice_loss_1` (truncated injection time) and
`re1ob_productcatalogservice_cpu_3` (no data after the injection). Every
result below is on the remaining **733** cases. The dataset revision is
pinned (`afeacb11…` on Hugging Face) and verified by checksum on download.

## Protocol

**Task.** Given the telemetry of a case and the injection time, rank the
services; the answer is the service where the fault was injected. This is
the offline setting of the benchmark: the incident time is given.
Detection is evaluated separately on the simulator (below).

**Inputs.** Every method, Etio's and the baselines', receives the same
series, converted once by the harness from the dataset's CSV files. With
`--sources metrics traces logs`, trace-derived series (request rate, error
rate, latency per operation) and log-derived series (line and error rates)
are added where the dataset provides them.

**Metrics.** AC@k is the fraction of cases where the root-cause service is
among the first k ranked services; **Avg@5** is the mean of AC@1…AC@5, the
benchmark's headline metric. Rankings are at the service level. Methods that
rank *metrics* (BARO, N-Sigma) are converted by keeping the first occurrence
of each service; RCAEval's own tables count duplicates (the service of each
of the top-k metrics), which inflates AC@k slightly;
`etio-eval report --published` reproduces that convention. Intervals are
95 % percentile bootstrap intervals over cases (1000 resamples); paired
differences between methods use paired resampling.

## Baselines, and a bug in the reference implementation

Etio implements the two strongest simple baselines of the benchmark paper
itself, so they run on the same inputs at the same speed:

* **BARO** (Pham et al., FSE 2024): scale each series by the median and IQR
  of the reference period, score it by its maximum in the abnormal period,
  rank by score.
* **N-Sigma**: the same with mean and standard deviation.

Both were validated against the official implementations (`RCAEval==1.8.0`,
methods `rcaeval_baro` and `rcaeval_nsigma` in the harness). Reproducing
BARO exactly required three details of the official code: the signed maximum
(not the absolute value), dropping series that are constant in *either*
period, and the IQR-of-zero convention. With them, **the rankings are
identical on every case without missing values**.

On cases *with* missing values they differ, and the official
implementation is the one at fault. It computes `score = max(zscores)` with
Python's built-in `max` and ranks with `sorted`, on arrays that contain NaN
for missing observations. Every comparison with NaN is false, so the score
of a series depends on *where* its missing values are (a NaN first
poisons the score, a NaN later is skipped), and `sorted` produces an
order that is not sorted around NaN keys. On RE1-TT, 96 % of the cases
contain such series. Etio's implementation ignores missing values in both
statistics. The official numbers are reported as they are, under their own
names, but we compare against the corrected implementation.

The official implementations also take 200–750 ms per case, against 4–30 ms
for Etio's methods (including, for Etio, feature extraction and the full
ranking).

## Results

### Methods on identical inputs

Multi-source inputs (metrics, logs and traces where available). `etio` is
the **heuristic** ranker here (hand-set weights, no training), so that no
number in this table is in-sample; the learned ranker is evaluated with
cross-validation in the next table.

| suite | Etio (heuristic) | N-Sigma | BARO | max-score | random walk | BARO (official) | N-Sigma (official) |
|---|---|---|---|---|---|---|---|
| RE1 (373) | **0.90** [0.88, 0.92] | 0.88 [0.85, 0.90] | 0.89 [0.86, 0.91] | 0.86 [0.84, 0.89] | 0.86 [0.84, 0.89] | 0.85 [0.82, 0.88] | 0.84 [0.81, 0.87] |
| RE2 (270) | **0.92** [0.89, 0.94] | 0.89 [0.86, 0.91] | 0.75 [0.72, 0.78] | 0.85 [0.81, 0.88] | 0.84 [0.80, 0.87] | 0.76 [0.73, 0.79] | 0.86 [0.83, 0.89] |
| RE3 (90) | **0.89** [0.86, 0.91] | 0.88 [0.84, 0.92] | 0.68 [0.62, 0.73] | 0.84 [0.80, 0.87] | 0.61 [0.51, 0.69] | 0.80 [0.74, 0.84] | 0.88 [0.83, 0.93] |
| ms per case | 5–30 | 5–25 | 4–22 | 4–16 | 4–20 | 620–750 | 200–600 |

Top-1 accuracy (AC@1) tells the same story: 0.75 / 0.76 / 0.53 for Etio's
heuristic against 0.69 / 0.74 / 0.66 for N-Sigma and 0.72 / 0.29 / 0.17 for
BARO. Per-dataset rows are in the output of `etio-eval run`.

Paired bootstrap differences in Avg@5 (same cases, 95 % intervals):

| | RE1 | RE2 | RE3 |
|---|---|---|---|
| Etio (heuristic) − N-Sigma | +0.021 [+0.001, +0.041] | +0.031 [+0.012, +0.051] | +0.002 [−0.042, +0.051] |
| Etio (heuristic) − BARO | +0.013 [−0.003, +0.029] | +0.167 [+0.146, +0.190] | +0.209 [+0.169, +0.253] |

The untrained heuristic is significantly better than N-Sigma on RE1 and RE2
and on par on RE3, and far better than BARO on the multi-source suites
(BARO was designed for metrics; with logs and traces added, its AC@1 falls
to 0.29 on RE2). It is *worse* than N-Sigma on one dataset, RE3-TT
(−0.053 [−0.107, −0.013]); we have not established why. The learned ranker
below does not have this weakness (0.99 on RE3-TT against 0.97 for
N-Sigma).

### The learned ranker (nested cross-validation)

The ranker is trained on RCAEval itself, so its evaluation must hold out
data it was not fitted on, and the held-out data must be *systems*, not
random cases: cases of the same system share services, metric names and
magnitudes, and a random split would reward memorising them. The protocol
is **nested leave-one-system-out**:

* outer loop: hold out one system (all its cases, in all suites), train on
  the other two, predict the held-out one;
* inner loop, on the training systems only: choose the regularisation
  strength by the same leave-one-system-out procedure.

Every number below is therefore a prediction on a system the model never
saw, with hyper-parameters chosen without it.

| held-out system | cases | learned | heuristic | ensemble (default) | max-score |
|---|---:|---:|---:|---:|---:|
| Online Boutique | 243 | 0.955 | 0.924 | 0.950 | 0.897 |
| Sock Shop | 245 | 0.983 | 0.963 | 0.976 | 0.953 |
| Train Ticket | 245 | 0.876 | 0.827 | 0.863 | 0.713 |
| **pooled** | 733 | **0.938** | 0.905 | 0.930 | 0.854 |

By suite and dataset (out-of-fold predictions, 95 % intervals):

| dataset | cases | learned | heuristic | ensemble (default) | max-score |
|---|---:|---|---|---|---|
| RE1 | 373 | 0.925 [0.901, 0.945] | 0.900 [0.879, 0.922] | 0.917 [0.895, 0.937] | 0.864 [0.838, 0.891] |
| RE1-OB | 123 | 0.961 [0.935, 0.982] | 0.924 [0.886, 0.956] | 0.946 [0.915, 0.972] | 0.894 [0.849, 0.937] |
| RE1-SS | 125 | 0.995 [0.989, 1.000] | 0.979 [0.968, 0.989] | 0.987 [0.978, 0.994] | 0.974 [0.962, 0.984] |
| RE1-TT | 125 | 0.819 [0.765, 0.870] | 0.798 [0.747, 0.843] | 0.818 [0.766, 0.864] | 0.725 [0.662, 0.778] |
| RE2 | 270 | 0.959 [0.941, 0.976] | 0.917 [0.894, 0.940] | 0.945 [0.926, 0.964] | 0.846 [0.810, 0.879] |
| RE2-OB | 90 | 0.980 [0.964, 0.993] | 0.949 [0.918, 0.976] | 0.973 [0.956, 0.989] | 0.933 [0.900, 0.962] |
| RE2-SS | 90 | 0.980 [0.960, 0.996] | 0.964 [0.940, 0.984] | 0.976 [0.956, 0.991] | 0.953 [0.924, 0.978] |
| RE2-TT | 90 | 0.916 [0.867, 0.960] | 0.838 [0.780, 0.891] | 0.887 [0.838, 0.933] | 0.651 [0.573, 0.729] |
| RE3 | 90 | 0.929 [0.889, 0.962] | 0.887 [0.856, 0.913] | 0.936 [0.909, 0.960] | 0.838 [0.796, 0.873] |
| RE3-OB | 30 | 0.853 [0.753, 0.947] | 0.853 [0.813, 0.893] | 0.893 [0.833, 0.947] | 0.800 [0.740, 0.853] |
| RE3-SS | 30 | 0.940 [0.900, 0.973] | 0.893 [0.833, 0.947] | 0.933 [0.893, 0.967] | 0.860 [0.780, 0.933] |
| RE3-TT | 30 | 0.993 [0.980, 1.000] | 0.913 [0.860, 0.953] | 0.980 [0.960, 1.000] | 0.853 [0.793, 0.907] |

The learned model improves on the heuristic on every system (+0.03 pooled),
most on Train Ticket, the largest and hardest system. The default is the
ensemble of both, which gives up 0.008 in cross-validation for robustness on
systems unlike the three in the benchmark.

**Train Ticket is the weak spot** (0.82 on RE1-TT). With 64 services,
metrics only, and many services that react to any fault (the gateway, the
order and travel services), the cause is often second or third. Traces help
(RE2-TT: 0.92).

### In-sample caution

The **bundled model** (`crates/etio-analysis/models/etio-rank-v1.json`) is
trained on all 733 cases, the right choice for users, whose systems are not
in the benchmark. Its scores on RCAEval are in-sample and must not be
reported as results. `etio-eval run` therefore scores with the heuristic
model unless a model is passed explicitly (`--model`), and
`--bundled-model` prints a warning. The cross-validated numbers above are
the ones to quote.

### Detection and ranking on the simulator

RCAEval gives the injection time, so it cannot measure detection. The
simulator (`crates/etio-sim`) can: it models request-level queueing
(M/M/c servers with backlog) over a service graph and emits realistic
OTLP traces, metrics and logs, with CPU starvation, network delay, packet
loss, error bursts, memory leaks and crashes.

`etio sim bench` runs random scenarios through the full streaming engine
(1 s windows): 15 minutes of normal traffic, then a 5-minute fault on a
random service. 72 scenarios, 24 per system size, fault kinds stratified:

| services | scenarios | detected | median delay | AC@1 (first analysis) | AC@3 (first analysis) | AC@1 (final analysis) | false incidents |
|---|---:|---:|---:|---:|---:|---:|---:|
| 10 | 24 | 100 % | 7 s | 96 % | 100 % | 96 % | 0 |
| 30 | 24 | 100 % | 9 s | 88 % | 100 % | 88 % | 1 |
| 100 | 24 | 100 % | 9 s | 83 % | 100 % | 96 % | 2 |

By fault kind, first-analysis AC@1 is 100 % for CPU, errors and memory
leaks, 92 % for crashes, 83 % for packet loss and 58 % for network delay.
A delay added in front of a service is seen first by its callers, and until
the client-side edge latency accumulates evidence, the callee and its
callers look alike; the final analysis gets 75 %. The 3 false incidents
occurred in 18 hours of fault-free traffic (the 15-minute lead-in of each
scenario).

The simulator is ours, so these numbers show that the pieces work together
at scale, not how Etio compares with other tools; that is what RCAEval is
for.

**Target selection.** An early version of the benchmark picked fault
targets uniformly. Some targets were services that received almost no
traffic in that topology, or none at all (unreachable from the entry
points). A fault there has no observable effect, and "missing" it is not an
error. Scenarios now draw targets among services that receive at least 0.5
requests per second in a pilot run. We mention it because the uncorrected
benchmark understated accuracy and nobody would have noticed a design that
overstated it the same way.

## What did not work

**Modality dropout.** The learned model was worse than the heuristic on
RE3-SS (0.57 against 0.88), and our hypothesis was that it had learned to
rely on trace and log features that Sock Shop lacks. We trained with
*modality dropout* (duplicates of the training incidents with trace and log
features removed, 240 extra incidents). It made things worse: RE3-SS fell to
0.31 and the pooled score from 0.931 to 0.913. The option remains
(`etio-eval train --augment`), off by default.

**The wrong diagnosis.** The hypothesis behind modality dropout was wrong.
Looking at the failures case by case showed the actual cause: in code-level
faults, the telling series are often error counters that were *exactly
zero* in the reference period, and the scoring discarded series with a
constant reference (their robust scale is zero). The signal was being thrown
away before the model saw it. Scoring such series by how persistently the
abnormal period deviates (and counting services that go silent) brought
RE3-SS to 0.94 and helped every other suite a little. The lesson we took:
inspect the failures before designing a fix for a hypothesis.

**Scaling with the abnormal period.** The first version scored RE1 at 0.68
Avg@5. Two causes: traffic series (which move in both directions whenever
anything happens) were treated as evidence, and the relative noise floor was
computed over the abnormal period too, so a large anomaly inflated its own
yardstick. Excluding traffic from the evidence (except at entry points) and
computing every scale on the reference period only fixed both.

**What the simulator did not catch.** The first run of the Docker demo,
with real services instrumented by the OpenTelemetry Python SDK, opened a
false incident before the scheduled fault, naming `catalog`. Three causes,
none visible in the simulator (whose metrics are service-scoped and exported
in lockstep with windows):

* Counter rates were computed as increments per window. The SDK exports
  every 5 s and the demo's windows were 5 s, so jitter left some windows
  without an export: `system.network.packets` "fell to zero" (108 robust
  deviations). Rates are now computed over the interval between exports.
* Every service reported `system.cpu.utilization`, which is the *host's*
  CPU; it rose because the machine was busy compiling. A host-wide event
  became five "service" anomalies. Host-scoped metrics no longer open
  incidents.
* After those fixes, a second run still opened an incident two minutes
  before the fault, triggered by `frontend` alone. An anomaly of *any*
  series of an entry point could open an incident by itself, and the
  frontend has some thirty series, most of them runtime internals (threads,
  memory, file descriptors). The rule exists for user impact, so it now
  considers only an entry point's latency, errors and traffic. Diagnosing
  this required knowing which series triggered the incident, which Etio did
  not record: incidents now carry their trigger signals.

Each fix has a regression test. The third run opened no incident during the
15 minutes before the fault, opened one 15 s after the fault began (with the
onset estimated to the second), and ranked `payment` first in every analysis
(86–88 %). The simulator benchmark was unchanged by these fixes.

## Limitations

* **Fault injection is not production.** RCAEval's faults are clean, single
  and injected into healthy systems; real incidents overlap, cascade and
  start from states that are already degraded.
* **Three systems.** Cross-validation over three systems is a small sample
  of "unseen systems"; the intervals above capture case-level uncertainty,
  not system-level variance.
* **Offline versus streaming.** The benchmark gives the injection time;
  Etio in production estimates the incident start from its detectors. The
  simulator measures the streaming setting, but on synthetic systems.
* **Service-level answers.** Etio names a service and shows the evidence;
  it does not localise the faulty code path or resource beyond the series it
  shows.

## Reproduce

```sh
cd python
uv run --extra baselines etio-eval fetch --logs --traces      # ~3 GB into $ETIO_DATA_DIR/rcaeval
uv run --extra baselines etio-eval run --sources metrics traces logs \
    --methods etio max_score baro nsigma random_walk rcaeval_baro rcaeval_nsigma
uv run etio-eval train --out ../eval/out/model.json           # nested CV tables + a model
uv run etio-eval report ../eval/out/run.json --compare etio baro
cd .. && cargo run --release -- sim bench --json eval/out/sim-bench.json
```

Runs record their provenance (versions, dataset revision, configuration)
in the output file, without host names or paths.
