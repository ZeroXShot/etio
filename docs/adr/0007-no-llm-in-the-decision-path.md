# 0007. No language model in the decision path

**Status:** accepted

## Context

Language models are an obvious way to "explain" incidents, and some tools
use them to rank causes directly. For an on-call engineer at 3 a.m., though,
an explanation must be correct, reproducible and traceable to evidence;
fluency does not help if a sentence is invented. Sending telemetry to an
external model provider also raises privacy and availability concerns.

## Decision

* Detection, ranking and explanations are computed by the deterministic
  pipeline. Explanations are generated from feature values and series scores
  by fixed rules, so each sentence is traceable to a number.
* Etio works fully offline and has no dependency on any model provider.

## Consequences

* The same incident always yields the same ranking and reasons, which makes
  the behaviour testable and auditable.
* Explanations are terser than a language model's narrative. An optional
  narrative layer could consume the structured result (ranking, reasons,
  evidence) as grounded input, but it would sit outside the decision path and
  must be opt-in.
