//! `etio`: the Etio command-line entry point.

use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::Context;
use clap::{Parser, Subcommand};
use etio_analysis::rca::{self, RankModel, RcaConfig, RcaInput};
use etio_server::actor::Clock;
use etio_server::config::{ClockMode, LogFormat, ServerConfig};
use etio_server::serve::Running;

/// Root-cause analysis for distributed systems, from OpenTelemetry data.
#[derive(Parser)]
#[command(name = "etio", version, about, long_about = None)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Run the server (OTLP receivers, engine, API).
    Serve {
        /// Configuration file (TOML).
        #[arg(short, long, env = "ETIO_CONFIG")]
        config: Option<PathBuf>,
        /// State directory (overrides `storage.dir`).
        #[arg(long)]
        data_dir: Option<PathBuf>,
        /// Ranking model JSON (overrides the built-in model).
        #[arg(long)]
        model: Option<PathBuf>,
    },
    /// Configuration helpers.
    Config {
        #[command(subcommand)]
        action: ConfigAction,
    },
    /// Simulated systems: send traffic to a server, or run the scale benchmark.
    Sim {
        #[command(subcommand)]
        action: SimAction,
    },
    /// Probe a running server's readiness (for container health checks,
    /// since the image has no shell or curl). Exits non-zero when not ready.
    Health {
        /// Readiness URL. The probe targets the server itself, so a TLS
        /// certificate is not verified.
        #[arg(long, default_value = "http://127.0.0.1:7070/readyz")]
        url: String,
    },
    /// Rank root causes offline, from a JSON analysis input (see `RcaInput`).
    Analyze {
        /// Input file (`-` for stdin).
        input: PathBuf,
        /// Ranking method: etio, max_score, baro, nsigma, random_walk.
        #[arg(long, default_value = "etio")]
        method: String,
        /// Ranking model JSON.
        #[arg(long)]
        model: Option<PathBuf>,
    },
}

#[derive(Subcommand)]
enum SimAction {
    /// Stream OTLP from a simulated system to a server.
    Run {
        /// OTLP/gRPC endpoint.
        #[arg(long, default_value = "http://127.0.0.1:4317")]
        endpoint: String,
        /// `shop` or `layered:N[:seed]`.
        #[arg(long, default_value = "shop")]
        topology: String,
        /// Requests per second at the entry points.
        #[arg(long, default_value_t = 30.0)]
        rate: f64,
        /// Faults as `kind[=value]:target@start+duration` (repeatable).
        #[arg(long = "fault")]
        faults: Vec<String>,
        /// Simulated duration.
        #[arg(long, default_value = "30m")]
        duration: String,
        /// Simulated seconds per second (0 = as fast as possible).
        #[arg(long, default_value_t = 1.0)]
        speed: f64,
        /// File holding the bearer token expected by the server.
        #[arg(long)]
        token_file: Option<PathBuf>,
        /// Random seed.
        #[arg(long, default_value_t = 1)]
        seed: u64,
    },
    /// Run random scenarios through the engine and report detection and ranking accuracy.
    Bench {
        /// System sizes (numbers of services).
        #[arg(long, value_delimiter = ',', default_value = "10,30,100")]
        sizes: Vec<usize>,
        /// Scenarios per size.
        #[arg(long, default_value_t = 20)]
        per_size: usize,
        /// Base seed.
        #[arg(long, default_value_t = 1)]
        seed: u64,
        /// Worker threads (default: all cores but one).
        #[arg(long)]
        workers: Option<usize>,
        /// Also write every outcome as JSON.
        #[arg(long)]
        json: Option<PathBuf>,
    },
}

#[derive(Subcommand)]
enum ConfigAction {
    /// Print the default configuration.
    Default,
    /// Validate a configuration file (including environment overrides).
    Check {
        /// File to check.
        config: PathBuf,
    },
}

fn init_logging(cfg: &ServerConfig) {
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new(&cfg.log.level));
    let builder = tracing_subscriber::fmt().with_env_filter(filter).with_target(false);
    match cfg.log.format {
        LogFormat::Json => builder.json().init(),
        LogFormat::Pretty => builder.init(),
    }
}

fn load_model(path: &std::path::Path) -> anyhow::Result<RankModel> {
    let text = std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    RankModel::from_json(&text).with_context(|| format!("loading model {}", path.display()))
}

async fn wait_for_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    #[cfg(unix)]
    let term = async {
        if let Ok(mut s) = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            s.recv().await;
        }
    };
    #[cfg(not(unix))]
    let term = std::future::pending::<()>();
    tokio::select! { () = ctrl_c => {}, () = term => {} }
}

async fn serve(config: Option<PathBuf>, data_dir: Option<PathBuf>, model: Option<PathBuf>) -> anyhow::Result<()> {
    let mut cfg = ServerConfig::load(config.as_deref(), std::env::vars())?;
    if let Some(dir) = data_dir {
        cfg.storage.dir = Some(dir);
    }
    if let Some(m) = model {
        cfg.engine.rca.model = load_model(&m)?;
    }
    init_logging(&cfg);
    tracing::info!(version = env!("CARGO_PKG_VERSION"), model = %cfg.engine.rca.model.name, "starting etio");
    let clock = match cfg.clock {
        ClockMode::Wall => Clock::Wall,
        ClockMode::Event => Clock::Event,
    };
    let running = Running::start(cfg, clock).await?;
    wait_for_signal().await;
    tracing::info!("shutting down");
    running.stop().await
}

fn analyze(input: &std::path::Path, method: &str, model: Option<PathBuf>) -> anyhow::Result<()> {
    let text = if input.as_os_str() == "-" {
        std::io::read_to_string(std::io::stdin())?
    } else {
        std::fs::read_to_string(input).with_context(|| format!("reading {}", input.display()))?
    };
    let input: RcaInput = serde_json::from_str(&text).context("parsing the analysis input")?;
    let mut cfg = RcaConfig { method: method.parse().map_err(|e: String| anyhow::anyhow!(e))?, ..RcaConfig::default() };
    if let Some(m) = model {
        cfg.model = load_model(&m)?;
    }
    let result = rca::analyze(&input, &cfg)?;
    let out = serde_json::to_string_pretty(&result)?;
    std::io::Write::write_all(&mut std::io::stdout(), out.as_bytes())?;
    Ok(())
}

fn sim(action: SimAction) -> anyhow::Result<()> {
    match action {
        SimAction::Run { endpoint, topology, rate, faults, duration, speed, token_file, seed } => {
            tracing_subscriber::fmt().with_target(false).init();
            let opts = etio_server::simulate::DriveOptions {
                endpoint,
                topology: etio_server::simulate::parse_topology(&topology)?,
                rate,
                faults: faults.iter().map(|f| etio_server::simulate::parse_fault(f)).collect::<anyhow::Result<_>>()?,
                duration: etio_core::time::parse_duration(&duration)?,
                speed,
                token: token_file
                    .as_deref()
                    .map(etio_server::config::Secret::from_file)
                    .transpose()?
                    .map(|s| s.expose().to_owned()),
                seed,
            };
            tokio::runtime::Builder::new_multi_thread().enable_all().build()?.block_on(etio_server::simulate::run(opts))
        }
        SimAction::Bench { sizes, per_size, seed, workers, json } => {
            let workers = workers.unwrap_or_else(|| {
                std::thread::available_parallelism().map_or(1, |n| n.get().saturating_sub(1).max(1))
            });
            let started = std::time::Instant::now();
            let outcomes = etio_sim::bench::run(&sizes, per_size, seed, workers);
            let report = etio_sim::bench::markdown(&outcomes);
            std::io::Write::write_all(&mut std::io::stdout(), report.as_bytes())?;
            let spans: u64 = outcomes.iter().map(|o| o.spans).sum();
            let secs = started.elapsed().as_secs_f64();
            #[allow(clippy::cast_precision_loss)]
            let line = format!(
                "\n{} scenarios, {spans} spans in {secs:.1} s ({:.0} spans/s)\n",
                outcomes.len(),
                spans as f64 / secs
            );
            std::io::Write::write_all(&mut std::io::stdout(), line.as_bytes())?;
            if let Some(path) = json {
                std::fs::write(&path, serde_json::to_vec_pretty(&outcomes)?)?;
            }
            Ok(())
        }
    }
}

fn health(url: &str) -> anyhow::Result<()> {
    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
    rt.block_on(async {
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(3))
            .danger_accept_invalid_certs(true)
            .build()?;
        let status = client.get(url).send().await?.status();
        anyhow::ensure!(status.is_success(), "{url} answered {status}");
        Ok(())
    })
}

fn main() -> ExitCode {
    etio_server::init_crypto();
    let cli = Cli::parse();
    let result = match cli.command {
        Command::Serve { config, data_dir, model } => tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .thread_name("etio-io")
            .build()
            .map_err(anyhow::Error::from)
            .and_then(|rt| rt.block_on(serve(config, data_dir, model))),
        Command::Config { action: ConfigAction::Default } => {
            std::io::Write::write_all(&mut std::io::stdout(), ServerConfig::default_toml().as_bytes())
                .map_err(Into::into)
        }
        Command::Config { action: ConfigAction::Check { config } } => {
            ServerConfig::load(Some(&config), std::env::vars())
                .map(|_| {
                    let _ = std::io::Write::write_all(&mut std::io::stdout(), b"configuration is valid\n");
                })
                .map_err(Into::into)
        }
        Command::Analyze { input, method, model } => analyze(&input, &method, model),
        Command::Health { url } => health(&url),
        Command::Sim { action } => sim(action),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            let _ = std::io::Write::write_all(&mut std::io::stderr(), format!("error: {e:#}\n").as_bytes());
            ExitCode::FAILURE
        }
    }
}
