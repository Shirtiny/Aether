use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Serialize;
use sysinfo::{get_current_pid, Pid, ProcessesToUpdate, System};
use tracing::info;

/// Hardware information collected at startup.
///
/// The struct is `Serialize`-able so it can be sent directly as the
/// `hardware_info` JSON bag in the registration request.  New fields
/// can be added without database schema migrations.
#[derive(Debug, Clone, Serialize)]
pub struct HardwareInfo {
    pub cpu_cores: u32,
    pub total_memory_mb: u64,
    pub os_info: String,
    pub fd_limit: u64,
    #[serde(skip)]
    pub estimated_max_concurrency: u64,
}

/// Runtime resource usage sampled during heartbeat reporting.
#[derive(Debug, Clone, Serialize)]
pub struct RuntimeResourceSnapshot {
    pub sampled_at_unix_secs: u64,
    pub system_cpu_usage_percent: f64,
    pub process_cpu_usage_percent: f64,
    pub memory_total_bytes: u64,
    pub memory_used_bytes: u64,
    pub memory_available_bytes: u64,
    pub memory_used_percent: f64,
    pub process_memory_bytes: u64,
    pub process_virtual_memory_bytes: u64,
    pub process_memory_percent: f64,
    pub load_average_1m: f64,
    pub load_average_5m: f64,
    pub load_average_15m: f64,
    pub system_uptime_secs: u64,
    pub process_uptime_secs: Option<u64>,
}

/// Small, reusable sysinfo monitor. Keeping it alive between samples makes CPU
/// usage deltas meaningful without re-enumerating the whole machine every time.
pub struct RuntimeResourceMonitor {
    system: Mutex<System>,
    current_pid: Option<Pid>,
}

impl RuntimeResourceMonitor {
    pub fn new() -> Self {
        let mut system = System::new_all();
        let current_pid = get_current_pid().ok();
        if let Some(pid) = current_pid {
            system.refresh_processes(ProcessesToUpdate::Some(&[pid]), true);
        }
        system.refresh_cpu_usage();
        system.refresh_memory();
        Self {
            system: Mutex::new(system),
            current_pid,
        }
    }

    pub fn snapshot(&self) -> RuntimeResourceSnapshot {
        let mut system = match self.system.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };

        system.refresh_cpu_usage();
        system.refresh_memory();
        if let Some(pid) = self.current_pid {
            system.refresh_processes(ProcessesToUpdate::Some(&[pid]), true);
        }

        let memory_total_bytes = system.total_memory();
        let memory_used_bytes = system.used_memory();
        let memory_available_bytes = system.available_memory();
        let (
            process_cpu_usage_percent,
            process_memory_bytes,
            process_virtual_memory_bytes,
            process_uptime_secs,
        ) = self
            .current_pid
            .and_then(|pid| system.process(pid))
            .map(|process| {
                (
                    process.cpu_usage() as f64,
                    process.memory(),
                    process.virtual_memory(),
                    Some(process.run_time()),
                )
            })
            .unwrap_or((0.0, 0, 0, None));
        let load = System::load_average();

        RuntimeResourceSnapshot {
            sampled_at_unix_secs: current_unix_secs(),
            system_cpu_usage_percent: system.global_cpu_usage() as f64,
            process_cpu_usage_percent,
            memory_total_bytes,
            memory_used_bytes,
            memory_available_bytes,
            memory_used_percent: ratio_percent(memory_used_bytes, memory_total_bytes),
            process_memory_bytes,
            process_virtual_memory_bytes,
            process_memory_percent: ratio_percent(process_memory_bytes, memory_total_bytes),
            load_average_1m: load.one,
            load_average_5m: load.five,
            load_average_15m: load.fifteen,
            system_uptime_secs: System::uptime(),
            process_uptime_secs,
        }
    }
}

/// Collect hardware information and estimate max concurrency.
///
/// Should be called once at startup -- hardware does not change at runtime.
pub fn collect() -> HardwareInfo {
    let sys = System::new_all();

    let cpu_cores = sys.cpus().len() as u32;
    let total_memory_mb = sys.total_memory() / (1024 * 1024);
    let os_info = format!(
        "{} {}",
        System::name().unwrap_or_else(|| "Unknown".into()),
        System::os_version().unwrap_or_default(),
    )
    .trim()
    .to_string();

    // Estimate max concurrent connections:
    //   - Each tokio async task uses ~8-16 KB stack + heap buffers
    //   - OS file descriptor limit is often the real bottleneck
    //   - Conservative formula: min(fd_limit - 100, ram_mb * 40, cpu_cores * 2000)
    let fd_limit = get_fd_limit();
    let by_fd = fd_limit.saturating_sub(100);
    let by_ram = total_memory_mb.saturating_mul(40);
    let by_cpu = (cpu_cores as u64).saturating_mul(2000);
    let estimated_max_concurrency = by_fd.min(by_ram).min(by_cpu);

    info!(
        cpu_cores,
        total_memory_mb,
        os_info = %os_info,
        fd_limit,
        estimated_max_concurrency,
        "hardware info collected"
    );

    HardwareInfo {
        cpu_cores,
        total_memory_mb,
        os_info,
        fd_limit,
        estimated_max_concurrency,
    }
}

/// Read the soft file-descriptor limit (RLIMIT_NOFILE).
fn get_fd_limit() -> u64 {
    #[cfg(unix)]
    {
        let mut rlim = libc::rlimit {
            rlim_cur: 0,
            rlim_max: 0,
        };
        let ret = unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &mut rlim) };
        if ret == 0 {
            return rlim.rlim_cur;
        }
    }
    // Fallback for non-unix or error
    1024
}

fn ratio_percent(value: u64, total: u64) -> f64 {
    if total == 0 {
        0.0
    } else {
        value as f64 * 100.0 / total as f64
    }
}

fn current_unix_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}
