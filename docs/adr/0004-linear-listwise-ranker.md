# 0004. A linear listwise ranker with an expert prior

**Status:** accepted

## Context

Ranking root-cause candidates is a learning-to-rank problem with very little
labelled data: public benchmarks provide about 700 incidents from three
systems. A model must transfer to systems it has never seen (every user's
system is new), must be explainable to an on-call engineer, and must run in
microseconds.

## Decision

* Services are described by 20 **scale-free** features (log-scaled anomaly
  strengths, ranks, fractions, graph features), so that the same model
  applies to any system.
* The ranker is **linear**, trained **listwise** (softmax over the services
  of an incident, maximum likelihood of the true root cause) with exact
  Newton steps.
* The penalty is a Gaussian prior centred on **hand-set heuristic weights**,
  its strength chosen by nested leave-one-system-out cross-validation.
* The default is an ensemble of the learned and the heuristic model.

## Consequences

* Transfer is measured honestly: every reported learned-model number is
  out-of-fold for the whole system (see [evaluation](../evaluation.md)).
* Scores decompose exactly into feature contributions, which the UI shows.
* The model cannot learn interactions between features; so far, the
  features themselves (e.g. "callee explains the anomaly") carry the
  interactions that matter.

## Alternatives

* **Gradient-boosted trees**: fit the training systems better, but with
  three systems they overfit system-specific magnitudes; not decomposable
  exactly.
* **Graph neural networks / causal discovery** (as in part of the
  literature): need far more data or strong assumptions, are slow, and are
  hard to explain. Etio uses the graph through features (random walk,
  upstream/downstream anomaly) instead.
* **Hand-set weights only**: the heuristic model is 0.905 Avg@5 against
  0.938 for the learned model in cross-validation; keeping it as the prior
  and as an ensemble member retains its robustness.
