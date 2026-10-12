//! What the benchmark reads from the machine: its description, tool versions, the state the
//! stability checks look at, and a process's memory and CPU time.
//!
//! Linux reads `/proc` and `/sys`; macOS asks `ps`, `sysctl`, `vm_stat` and `top`. Nothing that
//! identifies the machine or its user is recorded: no hostname, IP address, user name, path or
//! environment variable, and process names only from a fixed list of known build and system jobs.

use std::{
    io::{Read as _, Write as _},
    net::{SocketAddr, TcpStream},
    path::Path,
    process::{Command, Stdio},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

use serde_json::{Map, Value, json};

const LINUX: bool = cfg!(target_os = "linux");
const MACOS: bool = cfg!(target_os = "macos");

/// Runs a probe and returns its trimmed stdout (or stderr, for `java -version`) on success.
/// Version managers' shims are asked not to install or look anything up over the network.
pub fn probe(program: &str, args: &[&str]) -> Option<String> {
    let output = Command::new(program)
        .args(args)
        .env("MISE_OFFLINE", "1")
        .stdin(Stdio::null())
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = if output.stdout.is_empty() {
        output.stderr
    } else {
        output.stdout
    };
    Some(String::from_utf8_lossy(&text).trim().to_owned()).filter(|s| !s.is_empty())
}

fn read(path: &str) -> Option<String> {
    std::fs::read_to_string(path)
        .ok()
        .map(|s| s.trim().to_owned())
}

/// Values recorded from probes are short, single-line and printable; anything else is dropped.
fn clean(value: &str) -> Option<String> {
    let line = value.lines().next()?.trim();
    (!line.is_empty() && line.len() <= 200 && !line.chars().any(char::is_control))
        .then(|| line.to_owned())
}

// --- One process: memory and CPU time --------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ProcSample {
    pub rss_bytes: u64,
    /// User + system CPU time since the process started.
    pub cpu_ms: f64,
}

/// The process's resident memory and CPU time, or `None` once it has gone.
pub fn sample(pid: u32) -> Option<ProcSample> {
    if LINUX {
        let status = std::fs::read_to_string(format!("/proc/{pid}/status")).ok()?;
        let rss_kib = status_kib(&status, "VmRSS:")?;
        let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
        let ticks = proc_stat_cpu_ticks(&stat)?;
        Some(ProcSample {
            rss_bytes: rss_kib * 1024,
            cpu_ms: ticks as f64 * 1000.0 / clock_ticks(),
        })
    } else {
        let out = probe("/bin/ps", &["-o", "rss=,time=", "-p", &pid.to_string()])?;
        parse_ps_sample(&out)
    }
}

/// The highest resident memory of the process so far, where the kernel keeps it (Linux
/// `VmHWM`). Elsewhere the benchmark takes the highest periodic sample instead.
pub fn peak_rss(pid: u32) -> Option<u64> {
    if !LINUX {
        return None;
    }
    let status = std::fs::read_to_string(format!("/proc/{pid}/status")).ok()?;
    status_kib(&status, "VmHWM:").map(|kib| kib * 1024)
}

pub fn peak_rss_method() -> &'static str {
    if LINUX {
        "kernel high-water mark (VmHWM)"
    } else {
        "highest periodic sample"
    }
}

fn status_kib(status: &str, key: &str) -> Option<u64> {
    status
        .lines()
        .find_map(|l| l.strip_prefix(key))?
        .split_whitespace()
        .next()?
        .parse()
        .ok()
}

/// utime + stime of `/proc/<pid>/stat`, in clock ticks. The command name (field 2) can hold
/// spaces and parentheses, so fields are counted after its closing parenthesis.
fn proc_stat_cpu_ticks(stat: &str) -> Option<u64> {
    let rest = &stat[stat.rfind(')')? + 1..];
    let fields: Vec<&str> = rest.split_whitespace().collect();
    // After the name: state is field 3, utime 14 and stime 15.
    let utime: u64 = fields.get(11)?.parse().ok()?;
    let stime: u64 = fields.get(12)?.parse().ok()?;
    Some(utime + stime)
}

fn proc_stat_name(stat: &str) -> Option<&str> {
    Some(&stat[stat.find('(')? + 1..stat.rfind(')')?])
}

fn clock_ticks() -> f64 {
    static TICKS: std::sync::OnceLock<f64> = std::sync::OnceLock::new();
    *TICKS.get_or_init(|| {
        probe("getconf", &["CLK_TCK"])
            .and_then(|s| s.parse().ok())
            .filter(|t: &f64| *t > 0.0)
            .unwrap_or(100.0)
    })
}

/// `ps -o rss=,time=`: KiB, then `[[dd-]hh:]mm:ss.cc`.
fn parse_ps_sample(out: &str) -> Option<ProcSample> {
    let mut parts = out.split_whitespace();
    let rss_kib: u64 = parts.next()?.parse().ok()?;
    let cpu_ms = parse_cpu_time(parts.next()?)?;
    Some(ProcSample {
        rss_bytes: rss_kib * 1024,
        cpu_ms,
    })
}

fn parse_cpu_time(text: &str) -> Option<f64> {
    let (days, clock) = match text.split_once('-') {
        Some((d, rest)) => (d.parse::<f64>().ok()?, rest),
        None => (0.0, text),
    };
    let mut seconds = 0.0;
    for part in clock.split(':') {
        seconds = seconds * 60.0 + part.parse::<f64>().ok()?;
    }
    Some((days * 86_400.0 + seconds) * 1000.0)
}

/// Samples a process's resident memory in the background, for the peak during a workload.
pub struct Sampler {
    stop: Arc<AtomicBool>,
    samples: Arc<Mutex<Vec<(Instant, u64)>>>,
    handle: Option<thread::JoinHandle<()>>,
}

impl Sampler {
    /// Every 10 ms on Linux (a file read); every 50 ms elsewhere, where each sample runs `ps`.
    pub fn interval() -> Duration {
        Duration::from_millis(if LINUX { 10 } else { 50 })
    }

    pub fn start(pid: u32) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let samples = Arc::new(Mutex::new(Vec::new()));
        let handle = {
            let (stop, samples) = (Arc::clone(&stop), Arc::clone(&samples));
            thread::spawn(move || {
                while !stop.load(Ordering::Relaxed) {
                    let Some(s) = sample(pid) else { return };
                    if let Ok(mut list) = samples.lock() {
                        list.push((Instant::now(), s.rss_bytes));
                    }
                    thread::sleep(Self::interval());
                }
            })
        };
        Self {
            stop,
            samples,
            handle: Some(handle),
        }
    }

    /// The highest sample taken between `from` and `to`.
    pub fn max_between(&self, from: Instant, to: Instant) -> Option<u64> {
        let list = self.samples.lock().ok()?;
        list.iter()
            .filter(|(at, _)| *at >= from && *at <= to)
            .map(|(_, rss)| *rss)
            .max()
    }

    pub fn max(&self) -> Option<u64> {
        let list = self.samples.lock().ok()?;
        list.iter().map(|(_, rss)| *rss).max()
    }

    pub fn finish(mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

impl Drop for Sampler {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

// --- The whole machine's CPU: busy share and steal --------------------------------------------

/// The aggregate `cpu` line of `/proc/stat`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CpuTimes {
    pub total: u64,
    pub idle: u64,
    pub steal: u64,
}

pub fn cpu_times() -> Option<CpuTimes> {
    if !LINUX {
        return None;
    }
    parse_proc_stat(&std::fs::read_to_string("/proc/stat").ok()?)
}

/// `cpu  user nice system idle iowait irq softirq steal guest guest_nice`. Guest time is already
/// counted in user and nice, so it is left out of the total.
fn parse_proc_stat(stat: &str) -> Option<CpuTimes> {
    let line = stat.lines().find(|l| l.starts_with("cpu "))?;
    let values: Vec<u64> = line
        .split_whitespace()
        .skip(1)
        .map(|v| v.parse().ok())
        .collect::<Option<_>>()?;
    let get = |i: usize| values.get(i).copied().unwrap_or(0);
    let total = (0..8).map(get).sum();
    Some(CpuTimes {
        total,
        idle: get(3) + get(4),
        steal: get(7),
    })
}

impl CpuTimes {
    /// (busy %, steal %) of the time between `self` and `later`.
    pub fn shares(&self, later: &CpuTimes) -> Option<(f64, f64)> {
        let total = later.total.checked_sub(self.total)?;
        if total == 0 {
            return None;
        }
        let idle = later.idle.saturating_sub(self.idle);
        let steal = later.steal.saturating_sub(self.steal);
        let total = total as f64;
        Some((
            100.0 * (total - idle as f64) / total,
            100.0 * steal as f64 / total,
        ))
    }
}

// --- Machine and environment ------------------------------------------------------------------

/// The machine's description (`machine` in the result). `id` and the fields of an environment
/// file come from the operator; the rest is read here.
pub fn machine(id: Option<&str>, operator: Option<&Map<String, Value>>, imds: bool) -> Value {
    let mut m = Map::new();
    m.insert("id".into(), json!(id));
    m.insert("os".into(), json!(std::env::consts::OS));
    m.insert("arch".into(), json!(std::env::consts::ARCH));
    m.insert("cpu_model".into(), json!(cpu_model()));
    m.insert("cpu_cores".into(), json!(cpu_cores()));
    m.insert(
        "cpu_threads".into(),
        json!(thread::available_parallelism().ok().map(|n| n.get())),
    );
    m.insert("ram_bytes".into(), json!(ram_bytes()));
    m.insert(
        "kernel".into(),
        json!(probe("uname", &["-r"]).as_deref().and_then(clean)),
    );
    m.insert("distribution".into(), json!(distribution()));
    m.insert(
        "filesystem".into(),
        json!(std::env::current_dir().ok().and_then(|d| filesystem(&d))),
    );
    m.insert("cpu_frequency".into(), cpu_frequency());
    m.insert("virtualization".into(), json!(virtualization()));
    m.insert(
        "cloud".into(),
        if imds { ec2() } else { None }.unwrap_or(Value::Null),
    );
    if let Some(operator) = operator {
        for (key, value) in operator {
            m.insert(key.clone(), value.clone());
        }
    }
    Value::Object(m)
}

fn cpu_model() -> Option<String> {
    if MACOS {
        return probe("sysctl", &["-n", "machdep.cpu.brand_string"])
            .as_deref()
            .and_then(clean);
    }
    let info = read("/proc/cpuinfo").unwrap_or_default();
    info.lines()
        .find_map(|l| {
            let (key, value) = l.split_once(':')?;
            (key.trim() == "model name").then(|| value.trim().to_owned())
        })
        .or_else(|| {
            probe("lscpu", &[])?.lines().find_map(|l| {
                let (key, value) = l.split_once(':')?;
                (key.trim() == "Model name").then(|| value.trim().to_owned())
            })
        })
        .as_deref()
        .and_then(clean)
}

fn cpu_cores() -> Option<u64> {
    if MACOS {
        return probe("sysctl", &["-n", "hw.physicalcpu"]).and_then(|s| s.parse().ok());
    }
    let info = read("/proc/cpuinfo")?;
    let mut cores = std::collections::BTreeSet::new();
    let (mut physical, mut core) = (None, None);
    for line in info.lines().chain(std::iter::once("")) {
        if line.trim().is_empty() {
            if let (Some(p), Some(c)) = (physical.take(), core.take()) {
                cores.insert((p, c));
            }
            continue;
        }
        if let Some((key, value)) = line.split_once(':') {
            match key.trim() {
                "physical id" => physical = Some(value.trim().to_owned()),
                "core id" => core = Some(value.trim().to_owned()),
                _ => {}
            }
        }
    }
    (!cores.is_empty()).then_some(cores.len() as u64)
}

fn ram_bytes() -> Option<u64> {
    if MACOS {
        return probe("sysctl", &["-n", "hw.memsize"]).and_then(|s| s.parse().ok());
    }
    status_kib(&read("/proc/meminfo")?, "MemTotal:").map(|kib| kib * 1024)
}

fn distribution() -> Option<String> {
    if MACOS {
        let name = probe("sw_vers", &["-productName"])?;
        let version = probe("sw_vers", &["-productVersion"])?;
        return clean(&format!("{name} {version}"));
    }
    read("/etc/os-release")?.lines().find_map(|l| {
        let value = l.strip_prefix("PRETTY_NAME=")?;
        clean(value.trim_matches('"'))
    })
}

/// The filesystem type of a directory (`apfs`, `ext4`, `xfs`, `tmpfs`, …).
fn filesystem(dir: &Path) -> Option<String> {
    let dir = dir.to_str()?;
    if LINUX {
        return probe("findmnt", &["-n", "-o", "FSTYPE", "-T", dir])
            .or_else(|| probe("stat", &["-f", "-c", "%T", dir]))
            .as_deref()
            .and_then(clean);
    }
    // macOS: the mount point from `df`, its type from `mount`.
    let df = probe("df", &["-P", dir])?;
    let mount_point = df
        .lines()
        .nth(1)?
        .split_whitespace()
        .skip(5)
        .collect::<Vec<_>>()
        .join(" ");
    let mounts = probe("mount", &[])?;
    mounts.lines().find_map(|l| {
        let (_, rest) = l.split_once(&format!(" on {mount_point} ("))?;
        clean(rest.split([',', ')']).next()?)
    })
}

/// CPU frequency scaling on Linux: the cpufreq driver and governor, and whether turbo is off.
/// Virtual machines, EC2 among them, often expose none of it; the note says so.
fn cpu_frequency() -> Value {
    if !LINUX {
        return json!({
            "driver": null,
            "governor": null,
            "no_turbo": null,
            "note": "CPU frequency scaling is not exposed on this operating system",
        });
    }
    let base = "/sys/devices/system/cpu/cpu0/cpufreq";
    let driver = read(&format!("{base}/scaling_driver"))
        .as_deref()
        .and_then(clean);
    let governor = read(&format!("{base}/scaling_governor"))
        .as_deref()
        .and_then(clean);
    let no_turbo = read("/sys/devices/system/cpu/intel_pstate/no_turbo").map(|v| v == "1");
    let note = (driver.is_none() && governor.is_none()).then_some(
        "cpufreq is not exposed (common on virtual machines such as EC2, where the hypervisor \
         controls frequency)",
    );
    json!({ "driver": driver, "governor": governor, "no_turbo": no_turbo, "note": note })
}

fn virtualization() -> Option<String> {
    if !LINUX {
        return None;
    }
    // `systemd-detect-virt` prints "none" and exits 1 on bare metal.
    let out = Command::new("systemd-detect-virt").output().ok()?;
    clean(&String::from_utf8_lossy(&out.stdout))
}

/// On EC2 only: the instance type and availability zone from the instance metadata service
/// (IMDSv2, short timeouts). It is asked only when the DMI vendor or the hypervisor says EC2, so
/// other machines send nothing. Nothing else is read: no instance or account ID, AMI owner,
/// address, hostname or credentials.
fn ec2() -> Option<Value> {
    if !LINUX {
        return None;
    }
    let vendor = ["sys_vendor", "board_vendor"]
        .iter()
        .filter_map(|f| read(&format!("/sys/devices/virtual/dmi/id/{f}")))
        .any(|v| v.contains("Amazon EC2"));
    let xen =
        read("/sys/hypervisor/uuid").is_some_and(|u| u.to_ascii_lowercase().starts_with("ec2"));
    if !vendor && !xen {
        return None;
    }
    let addr = SocketAddr::from(([169, 254, 169, 254], 80));
    let timeout = Duration::from_millis(300);
    let (status, token) = http(
        addr,
        "PUT",
        "/latest/api/token",
        &[("X-aws-ec2-metadata-token-ttl-seconds", "60")],
        timeout,
    )
    .ok()?;
    let token = String::from_utf8(token).ok().filter(|_| status == 200)?;
    let get = |path: &str| {
        let (status, body) = http(
            addr,
            "GET",
            path,
            &[("X-aws-ec2-metadata-token", token.trim())],
            timeout,
        )
        .ok()?;
        let value = String::from_utf8(body).ok()?;
        let value = value.trim();
        (status == 200
            && !value.is_empty()
            && value.len() <= 64
            && value
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-'))
        .then(|| value.to_owned())
    };
    Some(json!({
        "provider": "aws-ec2",
        "instance_type": get("/latest/meta-data/instance-type")?,
        "availability_zone": get("/latest/meta-data/placement/availability-zone"),
    }))
}

/// Tool versions (`environment`). Each is `null` when the tool is not installed.
pub fn environment(java: Option<&str>, firebase: Option<&Path>) -> Value {
    let version_word = |text: Option<String>, prefix: &str| {
        text.as_deref()
            .and_then(clean)
            .map(|t| t.trim_start_matches(prefix).trim().to_owned())
    };
    let java = java
        .map(str::to_owned)
        .or_else(|| probe("java", &["-version"]).as_deref().and_then(clean));
    let docker = probe("docker", &["--version"]).and_then(|t| {
        // "Docker version 28.0.1, build 068a01e"
        let rest = t.strip_prefix("Docker version ")?;
        clean(rest.split(',').next()?)
    });
    json!({
        "java": java,
        "node": version_word(probe("node", &["--version"]), "v"),
        "firebase_tools": firebase_tools(firebase),
        "docker": docker,
        "rustc": version_word(probe("rustc", &["--version"]), "rustc"),
    })
}

/// firebase-tools' version from the `package.json` above `firebase` (the one given, else the
/// one on `PATH`), without running it (its update check would go to the network).
fn firebase_tools(given: Option<&Path>) -> Option<String> {
    let firebase = match given {
        Some(path) => path.to_owned(),
        None => {
            let paths = std::env::var_os("PATH")?;
            std::env::split_paths(&paths)
                .map(|dir| dir.join("firebase"))
                .find(|p| p.is_file())?
        }
    }
    .canonicalize()
    .ok()?;
    let mut dir = firebase.parent();
    while let Some(d) = dir {
        if let Ok(text) = std::fs::read_to_string(d.join("package.json"))
            && let Ok(pkg) = serde_json::from_str::<Value>(&text)
            && pkg["name"] == "firebase-tools"
        {
            return pkg["version"].as_str().and_then(clean);
        }
        dir = d.parent();
    }
    None
}

/// Reads an operator's environment file (bench/README.md): `environment_revision` and the
/// machine fields the benchmark cannot see from inside (instance type, image, disk). Recorded as
/// given; unknown keys are refused, so nothing unexpected (an address, a hostname) slips into a
/// published result.
pub fn environment_file(text: &str) -> Result<(i64, Map<String, Value>), String> {
    const MACHINE: &[&str] = &["instance_type", "ami_id", "ebs", "os_image"];
    const EBS: &[&str] = &["type", "size_gib", "iops", "throughput_mibps"];
    let value: Value = serde_json::from_str(text).map_err(|e| format!("not JSON: {e}"))?;
    let top = value.as_object().ok_or("expected an object")?;
    if let Some(key) = top
        .keys()
        .find(|k| !["$comment", "environment_revision", "machine"].contains(&k.as_str()))
    {
        return Err(format!(
            "unknown key {key:?} (known: environment_revision, machine)"
        ));
    }
    let revision = top
        .get("environment_revision")
        .and_then(Value::as_i64)
        .filter(|r| *r >= 0)
        .ok_or("environment_revision: expected a non-negative integer")?;
    let mut machine = Map::new();
    if let Some(m) = top.get("machine") {
        let m = m.as_object().ok_or("machine: expected an object")?;
        for (key, value) in m {
            if !MACHINE.contains(&key.as_str()) {
                return Err(format!(
                    "machine: unknown key {key:?} (known: {})",
                    MACHINE.join(", ")
                ));
            }
            if key == "ebs" {
                let ebs = value.as_object().ok_or("machine.ebs: expected an object")?;
                for (k, v) in ebs {
                    if !EBS.contains(&k.as_str()) {
                        return Err(format!(
                            "machine.ebs: unknown key {k:?} (known: {})",
                            EBS.join(", ")
                        ));
                    }
                    let ok = if k == "type" {
                        v.is_string()
                    } else {
                        v.is_u64()
                    };
                    if !ok {
                        return Err(format!("machine.ebs.{k}: unexpected value {v}"));
                    }
                }
            } else if !value
                .as_str()
                .is_some_and(|s| clean(s).as_deref() == Some(s))
            {
                return Err(format!("machine.{key}: expected a single-line string"));
            }
            machine.insert(key.clone(), value.clone());
        }
    }
    Ok((revision, machine))
}

// --- Stability ---------------------------------------------------------------------------------

/// Build and system jobs that skew a benchmark when they run next to it. Only these names are
/// ever recorded; other processes are counted, not named.
const KNOWN_JOBS: &[&str] = &[
    "rustc",
    "cargo",
    "clippy-driver",
    "rust-analyzer",
    "cc1",
    "cc1plus",
    "clang",
    "gcc",
    "ld",
    "ld64",
    "lld",
    "make",
    "ninja",
    "cmake",
    "java",
    "node",
    "bun",
    "deno",
    "go",
    "gradle",
    "mvn",
    "swift-frontend",
    "xcodebuild",
    "docker",
    "dockerd",
    "containerd",
    "qemu-system",
    "VBoxHeadless",
    "mds_stores",
    "mdworker",
    "mdworker_shared",
    "backupd",
    "apt",
    "apt-get",
    "dpkg",
    "unattended-upgr",
    "dnf",
    "yum",
    "packagekitd",
    "snapd",
    "python",
    "python3",
    "ffmpeg",
];

fn known_job(name: &str) -> Option<&'static str> {
    KNOWN_JOBS
        .iter()
        .copied()
        .find(|job| name == *job || (job.ends_with("system") && name.starts_with(job)))
}

/// The machine's state at one moment, for the stability checks.
#[derive(Debug, Clone, Default)]
pub struct Snapshot {
    pub load: Option<[f64; 3]>,
    pub available_memory_bytes: Option<u64>,
    pub cpu_temperature_c: Option<f64>,
    pub power_source: Option<&'static str>,
    pub cpu_busy_percent: Option<f64>,
    pub cpu_steal_percent: Option<f64>,
    /// Known jobs using CPU (their processes above 1 %), summed per name.
    pub known_jobs: Vec<(String, f64)>,
    /// Other processes above 10 % CPU: how many, and their total share.
    pub other_busy_processes: usize,
    pub other_busy_cpu_percent: f64,
}

/// Takes a snapshot; CPU shares are measured over `window` (about a second).
pub fn snapshot(window: Duration, own: &[u32]) -> Snapshot {
    let mut s = Snapshot {
        load: load_average(),
        available_memory_bytes: available_memory(),
        cpu_temperature_c: cpu_temperature(),
        power_source: power_source(),
        ..Snapshot::default()
    };
    let processes = if LINUX {
        let (before, procs_before) = (cpu_times(), linux_processes());
        thread::sleep(window);
        let (after, procs_after) = (cpu_times(), linux_processes());
        if let (Some(b), Some(a)) = (before, after)
            && let Some((busy, steal)) = b.shares(&a)
        {
            s.cpu_busy_percent = Some(busy);
            s.cpu_steal_percent = Some(steal);
        }
        let seconds = window.as_secs_f64() * clock_ticks();
        procs_after
            .into_iter()
            .filter_map(|(pid, name, ticks)| {
                let (_, _, earlier) = procs_before.iter().find(|(p, _, _)| *p == pid)?;
                Some((
                    pid,
                    name,
                    100.0 * ticks.saturating_sub(*earlier) as f64 / seconds,
                ))
            })
            .collect()
    } else {
        s.cpu_busy_percent = macos_cpu_busy();
        macos_processes()
    };
    for (pid, name, cpu) in processes {
        if own.contains(&pid) {
            continue;
        }
        match known_job(&name) {
            // One entry per job name, with the CPU of all its processes.
            Some(job) if cpu >= 1.0 => match s.known_jobs.iter_mut().find(|(n, _)| n == job) {
                Some((_, total)) => *total += cpu,
                None => s.known_jobs.push((job.to_owned(), cpu)),
            },
            Some(_) => {}
            None if cpu >= 10.0 => {
                s.other_busy_processes += 1;
                s.other_busy_cpu_percent += cpu;
            }
            None => {}
        }
    }
    s.known_jobs
        .sort_by(|a, b| b.1.total_cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    s
}

fn load_average() -> Option<[f64; 3]> {
    let text = if LINUX {
        read("/proc/loadavg")?
    } else {
        probe("sysctl", &["-n", "vm.loadavg"])?
    };
    parse_load(&text)
}

/// `/proc/loadavg` (`0.52 0.58 0.59 1/389 12345`) or macOS `vm.loadavg` (`{ 1.52 1.58 1.59 }`).
fn parse_load(text: &str) -> Option<[f64; 3]> {
    let mut values = text
        .split_whitespace()
        .filter(|w| *w != "{" && *w != "}")
        .map(|w| w.parse::<f64>().ok());
    Some([values.next()??, values.next()??, values.next()??])
}

fn available_memory() -> Option<u64> {
    if LINUX {
        return status_kib(&read("/proc/meminfo")?, "MemAvailable:").map(|kib| kib * 1024);
    }
    parse_vm_stat(&probe("vm_stat", &[])?)
}

/// macOS `vm_stat`: free, inactive, speculative and purgeable pages count as available.
fn parse_vm_stat(text: &str) -> Option<u64> {
    let page: u64 = text
        .lines()
        .next()?
        .split("page size of ")
        .nth(1)?
        .split_whitespace()
        .next()?
        .parse()
        .ok()?;
    let pages = |key: &str| -> u64 {
        text.lines()
            .find_map(|l| l.strip_prefix(key))
            .and_then(|v| v.trim().trim_end_matches('.').parse().ok())
            .unwrap_or(0)
    };
    Some(
        page * (pages("Pages free:")
            + pages("Pages inactive:")
            + pages("Pages speculative:")
            + pages("Pages purgeable:")),
    )
}

/// The hottest thermal zone on Linux. macOS needs privileges for it, so it is not read there.
fn cpu_temperature() -> Option<f64> {
    if !LINUX {
        return None;
    }
    let mut hottest: Option<f64> = None;
    for dir in ["/sys/class/thermal", "/sys/class/hwmon"] {
        let Ok(entries) = std::fs::read_dir(dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let candidates = ["temp", "temp1_input"].map(|f| path.join(f));
            for file in candidates {
                if let Ok(text) = std::fs::read_to_string(&file)
                    && let Ok(milli) = text.trim().parse::<f64>()
                    && milli > 0.0
                {
                    let c = milli / 1000.0;
                    hottest = Some(hottest.map_or(c, |h| h.max(c)));
                }
            }
        }
    }
    hottest
}

fn power_source() -> Option<&'static str> {
    if MACOS {
        let out = probe("pmset", &["-g", "batt"])?;
        return if out.contains("'AC Power'") {
            Some("ac")
        } else if out.contains("'Battery Power'") {
            Some("battery")
        } else {
            None
        };
    }
    let entries = std::fs::read_dir("/sys/class/power_supply").ok()?;
    let mut source = None;
    for entry in entries.flatten() {
        let path = entry.path();
        let kind = std::fs::read_to_string(path.join("type")).unwrap_or_default();
        match kind.trim() {
            "Mains"
                if std::fs::read_to_string(path.join("online")).is_ok_and(|o| o.trim() == "1") =>
            {
                return Some("ac");
            }
            "Battery"
                if std::fs::read_to_string(path.join("status"))
                    .is_ok_and(|s| s.trim() == "Discharging") =>
            {
                source = Some("battery");
            }
            _ => {}
        }
    }
    source
}

/// (pid, command name, CPU ticks so far) of every process.
fn linux_processes() -> Vec<(u32, String, u64)> {
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter_map(|e| {
            let pid: u32 = e.file_name().to_str()?.parse().ok()?;
            let stat = std::fs::read_to_string(e.path().join("stat")).ok()?;
            let name = proc_stat_name(&stat)?.to_owned();
            Some((pid, name, proc_stat_cpu_ticks(&stat)?))
        })
        .collect()
}

/// (pid, command name, recent CPU %) from `ps`, whose %CPU is a decaying average on macOS.
fn macos_processes() -> Vec<(u32, String, f64)> {
    let Some(out) = probe("/bin/ps", &["-A", "-o", "pid=,%cpu=,comm="]) else {
        return Vec::new();
    };
    out.lines()
        .filter_map(|l| {
            let mut parts = l.split_whitespace();
            let pid = parts.next()?.parse().ok()?;
            let cpu = parts.next()?.parse().ok()?;
            let command = parts.collect::<Vec<_>>().join(" ");
            let name = command.rsplit('/').next()?.to_owned();
            Some((pid, name, cpu))
        })
        .collect()
}

/// Busy share of all CPUs over a second, from the second sample of `top`.
fn macos_cpu_busy() -> Option<f64> {
    let out = probe("top", &["-l", "2", "-n", "0", "-s", "1"])?;
    let line = out.lines().rev().find(|l| l.starts_with("CPU usage:"))?;
    let idle = line
        .split(',')
        .find(|p| p.contains("idle"))?
        .trim()
        .split('%')
        .next()?
        .parse::<f64>()
        .ok()?;
    Some(100.0 - idle)
}

// --- A minimal HTTP/1.1 client ----------------------------------------------------------------

/// Sends one HTTP/1.1 request without a body and returns the status code and the body. Enough
/// for the emulators' admin endpoints and the instance metadata service. The response ends at
/// its `Content-Length`, at the last chunk, or when the server closes the connection (some
/// servers keep it open despite `Connection: close`).
pub fn http(
    addr: SocketAddr,
    method: &str,
    path: &str,
    headers: &[(&str, &str)],
    timeout: Duration,
) -> std::io::Result<(u16, Vec<u8>)> {
    let mut stream = TcpStream::connect_timeout(&addr, timeout)?;
    stream.set_read_timeout(Some(timeout.max(Duration::from_secs(1))))?;
    stream.set_write_timeout(Some(timeout))?;
    let mut request = format!(
        "{method} {path} HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\nContent-Length: 0\r\n"
    );
    for (name, value) in headers {
        request.push_str(&format!("{name}: {value}\r\n"));
    }
    request.push_str("\r\n");
    stream.write_all(request.as_bytes())?;
    let mut response = Vec::new();
    let mut buf = [0u8; 8192];
    loop {
        let n = stream.read(&mut buf)?;
        if n == 0 {
            break;
        }
        response.extend_from_slice(&buf[..n]);
        if response_complete(&response) {
            break;
        }
    }
    let invalid = || std::io::Error::new(std::io::ErrorKind::InvalidData, "not an HTTP response");
    let head_end = response
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .ok_or_else(invalid)?;
    let head = String::from_utf8_lossy(&response[..head_end]);
    let status = head
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .ok_or_else(invalid)?;
    Ok((status, response[head_end + 4..].to_vec()))
}

/// Whether `response` holds a whole HTTP/1.1 response: its head, then as many body bytes as
/// `Content-Length` says, or the last chunk of a chunked body.
fn response_complete(response: &[u8]) -> bool {
    let Some(head_end) = response.windows(4).position(|w| w == b"\r\n\r\n") else {
        return false;
    };
    let head = String::from_utf8_lossy(&response[..head_end]).to_ascii_lowercase();
    let body = &response[head_end + 4..];
    for line in head.lines().skip(1) {
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };
        match name.trim() {
            "content-length" => {
                return value.trim().parse::<usize>().is_ok_and(|n| body.len() >= n);
            }
            "transfer-encoding" if value.contains("chunked") => {
                return body.ends_with(b"0\r\n\r\n");
            }
            _ => {}
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ps_samples() {
        assert_eq!(
            parse_ps_sample("  2976   0:01.50"),
            Some(ProcSample {
                rss_bytes: 2976 * 1024,
                cpu_ms: 1500.0
            })
        );
        assert_eq!(parse_cpu_time("1:02:03.25"), Some(3_723_250.0));
        assert_eq!(parse_cpu_time("2-00:00:01"), Some(172_801_000.0));
        assert_eq!(parse_ps_sample(""), None);
    }

    #[test]
    fn proc_files() {
        let stat = "4242 (java (main)) S 1 4242 4242 0 -1 4194560 100 0 0 0 250 50 0 0 20 0 30 0";
        assert_eq!(proc_stat_cpu_ticks(stat), Some(300));
        assert_eq!(proc_stat_name(stat), Some("java (main)"));
        let status = "Name:\tjava\nVmHWM:\t  204800 kB\nVmRSS:\t  102400 kB\n";
        assert_eq!(status_kib(status, "VmRSS:"), Some(102_400));
        assert_eq!(status_kib(status, "VmHWM:"), Some(204_800));
    }

    #[test]
    fn steal_and_busy_shares() {
        let before = parse_proc_stat("cpu  100 0 50 800 50 0 0 0 0 0\ncpu0 1 2 3\n").unwrap();
        let after = parse_proc_stat("cpu  160 0 70 860 60 0 0 50 7 0\n").unwrap();
        // 200 ticks: 70 idle (idle + iowait), 50 steal.
        let (busy, steal) = before.shares(&after).unwrap();
        assert!((busy - 65.0).abs() < 1e-9, "{busy}");
        assert!((steal - 25.0).abs() < 1e-9, "{steal}");
        assert_eq!(before.shares(&before), None);
    }

    #[test]
    fn load_and_memory() {
        assert_eq!(
            parse_load("{ 12.46 11.09 14.32 }"),
            Some([12.46, 11.09, 14.32])
        );
        assert_eq!(
            parse_load("0.52 0.58 0.59 1/389 12345\n"),
            Some([0.52, 0.58, 0.59])
        );
        let vm_stat = "Mach Virtual Memory Statistics: (page size of 16384 bytes)\n\
                       Pages free:                               10.\n\
                       Pages active:                            999.\n\
                       Pages inactive:                           20.\n\
                       Pages speculative:                         3.\n\
                       Pages purgeable:                           2.\n";
        assert_eq!(parse_vm_stat(vm_stat), Some(35 * 16_384));
    }

    #[test]
    fn known_jobs_only() {
        assert_eq!(known_job("rustc"), Some("rustc"));
        assert_eq!(known_job("qemu-system-x86_64"), Some("qemu-system"));
        assert_eq!(known_job("Sourcetree"), None);
        assert_eq!(known_job("javac"), None);
    }

    #[test]
    fn environment_files() {
        let (revision, machine) = environment_file(
            r#"{"environment_revision": 3, "machine": {"instance_type": "c7i.xlarge",
                "ami_id": "ami-0123", "ebs": {"type": "gp3", "size_gib": 30},
                "os_image": "ubuntu-24.04"}}"#,
        )
        .unwrap();
        assert_eq!(revision, 3);
        assert_eq!(machine["instance_type"], "c7i.xlarge");
        assert_eq!(machine["ebs"]["size_gib"], 30);

        for (text, error) in [
            (r#"{"machine": {}}"#, "environment_revision"),
            (r#"{"environment_revision": -1}"#, "environment_revision"),
            (
                r#"{"environment_revision": 1, "hostname": "x"}"#,
                "unknown key",
            ),
            (
                r#"{"environment_revision": 1, "machine": {"private_ip": "10.0.0.1"}}"#,
                "unknown key",
            ),
            (
                r#"{"environment_revision": 1, "machine": {"ebs": {"type": "gp3", "volume_id": "v"}}}"#,
                "unknown key",
            ),
            (
                r#"{"environment_revision": 1, "machine": {"os_image": "a\nb"}}"#,
                "single-line",
            ),
        ] {
            let err = environment_file(text).unwrap_err();
            assert!(err.contains(error), "{text}: {err}");
        }
    }

    #[test]
    fn responses_end_at_their_length() {
        assert!(!response_complete(
            b"HTTP/1.1 200 OK\r\ncontent-length: 4\r\n"
        ));
        assert!(!response_complete(
            b"HTTP/1.1 200 OK\r\nContent-Length: 4\r\n\r\n{\n"
        ));
        assert!(response_complete(
            b"HTTP/1.1 200 OK\r\nContent-Length: 4\r\n\r\n{\n}\n"
        ));
        assert!(response_complete(
            b"HTTP/1.1 200 OK\r\ntransfer-encoding: chunked\r\n\r\n2\r\nok\r\n0\r\n\r\n"
        ));
        // No length: read until the server closes.
        assert!(!response_complete(b"HTTP/1.1 200 OK\r\n\r\nok"));
    }

    #[test]
    fn this_process_can_be_sampled() {
        let s = sample(std::process::id()).expect("own process");
        assert!(s.rss_bytes > 0);
        assert!(s.cpu_ms >= 0.0);
    }
}
