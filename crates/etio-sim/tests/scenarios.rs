//! Full scenarios through the engine: every fault kind on the shop topology.

use etio_sim::{Fault, FaultKind, Scenario, Topology, Workload};

fn shop(target: &str, kind: FaultKind, seed: u64) -> Scenario {
    Scenario {
        name: format!("shop-{target}-{}", kind.name()),
        topology: Topology::shop(),
        workload: Workload { rate: 30.0, ..Workload::default() },
        faults: vec![Fault { target: target.into(), kind, start_s: 900.0, duration_s: 240.0 }],
        seed,
        duration_s: 1_200.0,
    }
}

fn check(s: &Scenario, max_rank: usize) {
    let out = s.run(Scenario::engine_config()).expect("valid scenario");
    assert_eq!(out.false_incidents, 0, "{}: false alarm before the fault", s.name);
    let delay = out.detection_delay_s.unwrap_or_else(|| panic!("{}: fault not detected", s.name));
    assert!(delay < 60.0, "{}: detected after {delay:.0}s", s.name);
    // The first analysis is what on-call sees within seconds of detection.
    let rank = out.first_rank.unwrap_or(usize::MAX);
    assert!(rank <= max_rank, "{}: faulty service ranked {rank} (top: {:?})", s.name, out.first_top);
    let last = out.last_rank.unwrap_or(usize::MAX);
    assert!(last <= max_rank + 1, "{}: final analysis ranked it {last}", s.name);
}

#[test]
fn cpu_starvation_is_localised() {
    check(&shop("cart", FaultKind::Cpu { factor: 6.0 }, 1), 1);
}

#[test]
fn network_delay_is_localised() {
    check(&shop("payment", FaultKind::Delay { ms: 80.0 }, 2), 1);
}

#[test]
fn error_burst_is_localised() {
    check(&shop("catalog", FaultKind::Errors { rate: 0.3 }, 3), 1);
}

#[test]
fn crash_is_localised() {
    check(&shop("shipping", FaultKind::Crash, 4), 2);
}

#[test]
fn slow_datastore_is_localised() {
    // redis emits no traces; its slowness is visible through the client spans
    // that name it and through its infrastructure metrics.
    check(&shop("redis", FaultKind::Cpu { factor: 40.0 }, 5), 2);
}
