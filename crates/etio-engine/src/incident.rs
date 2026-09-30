//! Incident lifecycle.
//!
//! An incident groups the anomalies of one failure. It opens when enough
//! services are anomalous at once (or a user-facing entry point is), is
//! analysed after a short delay and then periodically while it lasts, and
//! resolves after a quiet period.
//!
//! Incident identifiers are derived from the data (start window and the
//! services that triggered it), never from a clock or a random source: two
//! engines fed the same telemetry, for example two replicas of the core, open
//! the *same* incident, so notifications can be de-duplicated downstream.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::hash::BuildHasher;

use etio_analysis::rca::RcaResult;
use serde::{Deserialize, Serialize};

use crate::config::IncidentConfig;
use etio_core::time::duration_nanos;

/// Lifecycle state.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IncidentStatus {
    /// Anomalies are ongoing.
    Open,
    /// The system has been quiet for the resolve period.
    Resolved,
}

/// An incident and its latest analysis.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Incident {
    /// Deterministic identifier.
    pub id: String,
    /// Lifecycle state.
    pub status: IncidentStatus,
    /// Estimated onset, ns since the epoch.
    pub start: i64,
    /// When the incident was opened (end of the triggering window), ns.
    pub opened_at: i64,
    /// When it resolved, ns.
    pub resolved_at: Option<i64>,
    /// End of the last window with anomalies, ns.
    pub last_anomaly_at: i64,
    /// Services that triggered the incident.
    pub trigger: Vec<String>,
    /// The most surprising series of each triggering service, so that an
    /// operator can see *why* the incident opened.
    #[serde(default)]
    pub trigger_signals: BTreeMap<String, String>,
    /// Every service seen anomalous, with the largest surprise observed.
    pub services: BTreeMap<String, f64>,
    /// Latest root-cause analysis.
    pub rca: Option<RcaResult>,
    /// Number of analyses run.
    pub analyses: u32,
    next_analysis_at: i64,
}

impl Incident {
    /// The top-ranked root-cause candidate, if analysed.
    #[must_use]
    pub fn top_candidate(&self) -> Option<(&str, f64)> {
        let r = self.rca.as_ref()?.ranking.first()?;
        Some((r.service.as_str(), r.probability))
    }
}

/// Anomalous services observed in one window.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct WindowAnomalies {
    /// Window index.
    pub window: i64,
    /// End of the window, ns since the epoch.
    pub end: i64,
    /// Anomalous services: earliest onset (ns) and largest surprise.
    pub services: BTreeMap<String, (i64, f64)>,
    /// The most surprising series of each anomalous service.
    pub signals: BTreeMap<String, String>,
    /// Entry points whose *user-facing* signals (latency, errors, traffic)
    /// are anomalous: what users experience, enough to open an incident alone.
    pub user_facing: BTreeSet<String>,
}

/// What the engine must do after a window.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Action {
    /// A new incident was opened.
    Opened(String),
    /// The incident is due for (re)analysis.
    Analyze(String),
    /// The incident resolved (analyse it one last time first).
    Resolved(String),
}

/// Opens, tracks and resolves incidents.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct IncidentManager {
    /// Not persisted: a restored manager always follows the current configuration.
    #[serde(skip)]
    cfg: IncidentConfig,
    open: Option<Incident>,
    history: VecDeque<Incident>,
    max_history: usize,
}

impl IncidentManager {
    /// Creates a manager keeping the `max_history` most recent resolved incidents.
    #[must_use]
    pub fn new(cfg: IncidentConfig, max_history: usize) -> Self {
        Self { cfg, open: None, history: VecDeque::new(), max_history }
    }

    /// Replaces the policy (after a configuration reload or a restore).
    pub fn set_config(&mut self, cfg: IncidentConfig) {
        self.cfg = cfg;
    }

    /// The open incident, if any.
    #[must_use]
    pub const fn open(&self) -> Option<&Incident> {
        self.open.as_ref()
    }

    /// All known incidents, most recent first.
    pub fn all(&self) -> impl Iterator<Item = &Incident> {
        self.open.iter().chain(self.history.iter().rev())
    }

    /// Looks up an incident.
    #[must_use]
    pub fn get(&self, id: &str) -> Option<&Incident> {
        self.all().find(|i| i.id == id)
    }

    /// Stores the result of an analysis.
    pub fn record_analysis(&mut self, id: &str, rca: RcaResult) {
        let target = match &mut self.open {
            Some(i) if i.id == id => Some(i),
            _ => self.history.iter_mut().find(|i| i.id == id),
        };
        if let Some(i) = target {
            i.rca = Some(rca);
            i.analyses += 1;
        }
    }

    fn triggers(&self, a: &WindowAnomalies) -> bool {
        a.services.len() >= self.cfg.min_services.max(1)
            || !a.user_facing.is_empty()
            || a.services.values().any(|&(_, surprise)| surprise >= self.cfg.severe_surprise)
    }

    /// Advances the lifecycle by one closed window.
    pub fn on_window(&mut self, a: &WindowAnomalies) -> Vec<Action> {
        let delay = duration_nanos(self.cfg.rca_delay);
        let interval = duration_nanos(self.cfg.rca_interval).max(1);
        let resolve = duration_nanos(self.cfg.resolve_after);
        let mut actions = Vec::new();

        if let Some(inc) = &mut self.open {
            if !a.services.is_empty() {
                inc.last_anomaly_at = a.end;
                for (svc, &(_, surprise)) in &a.services {
                    let e = inc.services.entry(svc.clone()).or_insert(0.0);
                    *e = e.max(surprise);
                }
            }
            if a.end - inc.last_anomaly_at >= resolve {
                inc.status = IncidentStatus::Resolved;
                inc.resolved_at = Some(a.end);
                let id = inc.id.clone();
                if let Some(done) = self.open.take() {
                    self.history.push_back(done);
                    while self.history.len() > self.max_history {
                        self.history.pop_front();
                    }
                }
                actions.push(Action::Resolved(id));
                return actions;
            }
            if a.end >= inc.next_analysis_at {
                inc.next_analysis_at = a.end + interval;
                actions.push(Action::Analyze(inc.id.clone()));
            }
            return actions;
        }

        if self.triggers(a) {
            let start = a.services.values().map(|&(onset, _)| onset).min().unwrap_or(a.end);
            let trigger: Vec<String> = a.services.keys().cloned().collect();
            let id = incident_id(start, &trigger);
            self.open = Some(Incident {
                id: id.clone(),
                status: IncidentStatus::Open,
                start,
                opened_at: a.end,
                resolved_at: None,
                last_anomaly_at: a.end,
                services: a.services.iter().map(|(k, &(_, s))| (k.clone(), s)).collect(),
                trigger_signals: a.signals.clone(),
                trigger,
                rca: None,
                analyses: 0,
                next_analysis_at: a.end + delay,
            });
            actions.push(Action::Opened(id.clone()));
            if delay == 0 {
                if let Some(inc) = &mut self.open {
                    inc.next_analysis_at = a.end + interval;
                }
                actions.push(Action::Analyze(id));
            }
        }
        actions
    }
}

/// `YYYY`-free, clock-free identifier: onset (seconds) and a hash of the trigger.
fn incident_id(start: i64, trigger: &[String]) -> String {
    let h = foldhash::fast::FixedState::with_seed(0x696e_6369_6465_6e74).hash_one(trigger);
    format!("inc-{}-{:08x}", start.div_euclid(1_000_000_000), h as u32)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    const SEC: i64 = 1_000_000_000;

    fn window(w: i64, services: &[&str]) -> WindowAnomalies {
        WindowAnomalies {
            window: w,
            end: (w + 1) * 10 * SEC,
            services: services.iter().map(|s| ((*s).to_owned(), ((w - 1) * 10 * SEC, 6.0))).collect(),
            signals: services.iter().map(|s| ((*s).to_owned(), "latency".to_owned())).collect(),
            user_facing: services.iter().filter(|s| **s == "frontend").map(|s| (*s).to_owned()).collect(),
        }
    }

    fn manager() -> IncidentManager {
        IncidentManager::new(
            IncidentConfig {
                rca_delay: Duration::from_secs(20),
                rca_interval: Duration::from_secs(30),
                resolve_after: Duration::from_secs(60),
                ..IncidentConfig::default()
            },
            10,
        )
    }

    #[test]
    fn an_entry_point_alone_needs_a_user_facing_anomaly() {
        let mut m = manager();
        // The entry point is anomalous, but only on internal signals (say,
        // its thread count): below the severe threshold, nothing opens.
        let mut internal = window(1, &["frontend"]);
        internal.user_facing.clear();
        assert!(m.on_window(&internal).is_empty());
        // Its latency is anomalous: users are affected, an incident opens.
        let opened = m.on_window(&window(2, &["frontend"]));
        assert!(matches!(opened.first(), Some(Action::Opened(_))));
        let inc = m.open.as_ref().unwrap();
        assert_eq!(inc.trigger_signals.get("frontend").map(String::as_str), Some("latency"));
    }

    #[test]
    fn lifecycle_open_analyze_resolve() {
        let mut m = manager();
        assert!(m.on_window(&window(0, &["cart"])).is_empty(), "one internal service is not enough");
        let acts = m.on_window(&window(1, &["cart", "checkout"]));
        let Action::Opened(id) = &acts[0] else { panic!("{acts:?}") };
        let inc = m.open().unwrap();
        assert_eq!(inc.start, 0, "onset of the earliest anomaly");
        assert_eq!(inc.opened_at, 20 * SEC);
        // Analysis is due 20 s after opening, then every 30 s.
        assert!(m.on_window(&window(2, &["cart"])).is_empty());
        assert_eq!(m.on_window(&window(3, &["cart"])), vec![Action::Analyze(id.clone())]);
        assert!(m.on_window(&window(4, &[])).is_empty());
        assert!(m.on_window(&window(5, &[])).is_empty());
        assert_eq!(m.on_window(&window(6, &[])), vec![Action::Analyze(id.clone())]);
        // Last anomaly ended at 40 s; resolves 60 s later.
        assert!(m.on_window(&window(8, &[])).is_empty());
        assert_eq!(m.on_window(&window(9, &[])), vec![Action::Resolved(id.clone())]);
        assert!(m.open().is_none());
        assert_eq!(m.get(id).unwrap().status, IncidentStatus::Resolved);
        assert_eq!(m.get(id).unwrap().services.len(), 2);
    }

    #[test]
    fn a_severe_anomaly_opens_alone() {
        let mut m = manager();
        let mut w = window(3, &["payment"]);
        w.services.get_mut("payment").unwrap().1 = 30.0;
        assert!(matches!(m.on_window(&w)[0], Action::Opened(_)));
    }

    #[test]
    fn an_anomalous_entry_point_opens_alone() {
        let mut m = manager();
        let acts = m.on_window(&window(3, &["frontend"]));
        assert!(matches!(acts[0], Action::Opened(_)));
    }

    #[test]
    fn identifiers_are_deterministic() {
        let (mut a, mut b) = (manager(), manager());
        let w = window(4, &["cart", "checkout"]);
        assert_eq!(a.on_window(&w), b.on_window(&w));
        assert_eq!(a.open().unwrap().id, b.open().unwrap().id);
        assert!(a.open().unwrap().id.starts_with("inc-30-"));
    }

    #[test]
    fn zero_delay_analyses_immediately() {
        let mut m = IncidentManager::new(IncidentConfig { rca_delay: Duration::ZERO, ..IncidentConfig::default() }, 10);
        let acts = m.on_window(&window(1, &["frontend"]));
        assert_eq!(acts.len(), 2);
        assert!(matches!(acts[1], Action::Analyze(_)));
    }
}
