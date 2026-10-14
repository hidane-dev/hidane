//! The benchmark's parameters: the canonical definitions built in (and repeated, versioned, in
//! `bench/canonical.json`), a config file, `--quick`, and command-line overrides, in that order.

use serde_json::{Map, Value, json};

/// Version of the benchmark definitions: what each scenario does and with which parameters.
/// Bump it whenever a change makes new numbers incomparable with older ones.
pub const BENCHMARK_VERSION: &str = "1";

/// The official emulator the canonical benchmark compares with, as firebase-tools 15.33.0 pins
/// it (`src/emulator/downloadableEmulatorInfo.json`).
pub const OFFICIAL_VERSION: &str = "1.22.0";
pub const OFFICIAL_URL: &str = "https://storage.googleapis.com/firebase-preview-drop/emulator/cloud-firestore-emulator-v1.22.0.jar";
pub const OFFICIAL_SHA256: &str =
    "9b6498b7f62714d67f48f59b3818883cd682dbcd46b9f59511de81c97bb5166c";
pub const OFFICIAL_SIZE: u64 = 136_707_194;
pub const OFFICIAL_JAR_NAME: &str = "cloud-firestore-emulator-v1.22.0.jar";
/// What the official emulator is run with besides `-jar`: an English locale, without which some
/// of its messages fail (tools/oracle/README.md).
pub const JAVA_OPTIONS: &[&str] = &["-Duser.language=en", "-Duser.country=US"];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Runs {
    pub warmup: usize,
    pub runs: usize,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Memory {
    pub runs: Runs,
    pub idle_ms: u64,
    pub documents: usize,
    pub batch: usize,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Operations {
    /// Iterations of each operation discarded before measuring, in the same process.
    pub warmup: usize,
    pub iterations: usize,
    pub batch: usize,
    pub seed_documents: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Size {
    pub documents: usize,
    pub runs: Runs,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Datasets {
    pub sizes: Vec<Size>,
    pub batch: usize,
    pub query_warmup: usize,
    pub query_runs: usize,
}

/// Writes with a listener on the collection (firebase-tools#3477).
#[derive(Debug, Clone, PartialEq)]
pub struct ListenWrites {
    pub runs: Runs,
    pub documents: usize,
    pub batch: usize,
}

/// The write scenarios again with the owner credential the Admin SDKs send, which skips Security
/// Rules (the others send none).
#[derive(Debug, Clone, PartialEq)]
pub struct OwnerWrites {
    pub runs: Runs,
    /// Commits of `batch` writes to the same documents discarded before measuring, in each run.
    pub commit_warmup: usize,
    pub commits: usize,
    pub batch: usize,
    /// Then written in commits of `batch`.
    pub documents: usize,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Ci {
    pub runs: Runs,
    pub tests: usize,
    pub seed_documents: usize,
}

/// Limits of the stability checks; `None` turns a check off.
#[derive(Debug, Clone, PartialEq)]
pub struct Stability {
    pub max_load_1m: Option<f64>,
    pub min_available_memory_mib: Option<f64>,
    pub max_cpu_steal_percent: Option<f64>,
    pub max_cpu_temperature_c: Option<f64>,
    pub max_startup_cv: Option<f64>,
    pub max_known_job_cpu_percent: Option<f64>,
    pub allow_battery: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Params {
    pub version: String,
    pub startup: Runs,
    pub memory: Memory,
    pub operations: Operations,
    pub datasets: Datasets,
    pub ci: Ci,
    /// The CI workload through `firebase emulators:exec` (with `--firebase`).
    pub ci_firebase: Ci,
    pub listen_writes: ListenWrites,
    pub owner_writes: OwnerWrites,
    pub command: Runs,
    pub stability: Stability,
}

const fn runs(warmup: usize, runs: usize) -> Runs {
    Runs { warmup, runs }
}

/// Runs of the listener scenario: one run of the official emulator takes minutes (121 s for
/// 100,000 documents in Phase 0), and each run starts a fresh process after the other scenarios
/// have warmed the OS's cache of the binary and the jar.
pub const LISTEN_WRITES_RUNS: Runs = runs(0, 3);

/// Warm-up and measured runs for a dataset size: the defaults (3 and 10) up to 1,000 documents,
/// scaled down above, where one run of the official emulator takes seconds.
pub fn size_runs(documents: usize, base: Runs) -> Runs {
    match documents {
        0..=1_000 => base,
        1_001..=10_000 => runs(base.warmup.min(1), base.runs.min(5)),
        _ => runs(base.warmup.min(1), base.runs.min(3)),
    }
}

impl Default for Params {
    /// The canonical definitions (`bench/canonical.json` repeats them; a test keeps the two
    /// equal).
    fn default() -> Self {
        let base = runs(3, 10);
        Self {
            version: BENCHMARK_VERSION.to_owned(),
            startup: base,
            memory: Memory {
                runs: base,
                idle_ms: 1_000,
                documents: 1_000,
                batch: 500,
            },
            operations: Operations {
                warmup: 20,
                iterations: 100,
                batch: 500,
                seed_documents: 100,
            },
            datasets: Datasets {
                sizes: [100, 1_000, 10_000, 100_000]
                    .into_iter()
                    .map(|documents| Size {
                        documents,
                        runs: size_runs(documents, base),
                    })
                    .collect(),
                batch: 500,
                query_warmup: 3,
                query_runs: 10,
            },
            ci: Ci {
                runs: base,
                tests: 200,
                seed_documents: 100,
            },
            ci_firebase: Ci {
                runs: runs(1, 5),
                tests: 200,
                seed_documents: 100,
            },
            listen_writes: ListenWrites {
                runs: LISTEN_WRITES_RUNS,
                documents: 100_000,
                batch: 500,
            },
            owner_writes: OwnerWrites {
                runs: runs(1, 3),
                commit_warmup: 20,
                commits: 100,
                batch: 500,
                documents: 100_000,
            },
            command: runs(1, 5),
            stability: Stability {
                max_load_1m: Some(2.0),
                min_available_memory_mib: Some(4_096.0),
                max_cpu_steal_percent: Some(5.0),
                max_cpu_temperature_c: Some(85.0),
                max_startup_cv: Some(0.25),
                max_known_job_cpu_percent: Some(10.0),
                allow_battery: false,
            },
        }
    }
}

impl Params {
    /// The smoke-test parameters of `--quick`: every scenario, a few runs each.
    pub fn quick() -> Self {
        let base = runs(1, 3);
        let mut params = Self {
            startup: base,
            command: runs(0, 1),
            ..Self::default()
        };
        params.memory.runs = runs(0, 2);
        params.operations.warmup = 5;
        params.operations.iterations = 20;
        params.datasets.sizes = vec![
            Size {
                documents: 100,
                runs: base,
            },
            Size {
                documents: 1_000,
                runs: runs(0, 2),
            },
        ];
        params.datasets.query_warmup = 1;
        params.datasets.query_runs = 5;
        params.ci = Ci {
            runs: runs(0, 2),
            tests: 20,
            seed_documents: 20,
        };
        params.ci_firebase = Ci {
            runs: runs(0, 1),
            tests: 20,
            seed_documents: 20,
        };
        params.listen_writes.runs = runs(0, 1);
        params.listen_writes.documents = 10_000;
        params.owner_writes = OwnerWrites {
            runs: runs(0, 1),
            commit_warmup: 5,
            commits: 20,
            documents: 10_000,
            ..params.owner_writes
        };
        params
    }

    /// Applies a config file (`bench/canonical.json`'s shape). Unknown keys are errors, so a
    /// typo cannot silently fall back to a default.
    pub fn apply_config(&mut self, config: &Value, quick: bool) -> Result<(), String> {
        let top = object(config, "config")?;
        known(
            top,
            "config",
            &[
                "$comment",
                "benchmark_version",
                "official",
                "scenarios",
                "stability",
                "quick",
            ],
        )?;
        if let Some(version) = top.get("benchmark_version") {
            self.version = version
                .as_str()
                .ok_or("benchmark_version: expected a string")?
                .to_owned();
        }
        if let Some(official) = top.get("official") {
            check_official(official)?;
        }
        if let Some(stability) = top.get("stability") {
            self.apply_stability(stability)?;
        }
        // `--quick` starts from the built-in quick parameters and takes the config's `quick`
        // block; otherwise the config's `scenarios` apply.
        let scenarios = if quick {
            match top.get("quick") {
                Some(q) => {
                    let q = object(q, "quick")?;
                    known(q, "quick", &["scenarios"])?;
                    q.get("scenarios")
                }
                None => None,
            }
        } else {
            top.get("scenarios")
        };
        if let Some(scenarios) = scenarios {
            self.apply_scenarios(scenarios)?;
        }
        Ok(())
    }

    fn apply_scenarios(&mut self, scenarios: &Value) -> Result<(), String> {
        let map = object(scenarios, "scenarios")?;
        known(
            map,
            "scenarios",
            &[
                "startup",
                "memory",
                "operations",
                "datasets",
                "ci",
                "ci_firebase",
                "listen_writes",
                "owner_writes",
                "command",
            ],
        )?;
        if let Some(v) = map.get("startup") {
            let m = object(v, "scenarios.startup")?;
            known(m, "scenarios.startup", &["warmup", "runs"])?;
            read_runs(m, "scenarios.startup", &mut self.startup)?;
        }
        if let Some(v) = map.get("memory") {
            let m = object(v, "scenarios.memory")?;
            known(
                m,
                "scenarios.memory",
                &["warmup", "runs", "idle_ms", "documents", "batch"],
            )?;
            read_runs(m, "scenarios.memory", &mut self.memory.runs)?;
            read_u64(m, "scenarios.memory", "idle_ms", &mut self.memory.idle_ms)?;
            read_usize(
                m,
                "scenarios.memory",
                "documents",
                &mut self.memory.documents,
            )?;
            read_usize(m, "scenarios.memory", "batch", &mut self.memory.batch)?;
        }
        if let Some(v) = map.get("operations") {
            let m = object(v, "scenarios.operations")?;
            known(
                m,
                "scenarios.operations",
                &["warmup", "iterations", "batch", "seed_documents"],
            )?;
            let o = &mut self.operations;
            read_usize(m, "scenarios.operations", "warmup", &mut o.warmup)?;
            read_usize(m, "scenarios.operations", "iterations", &mut o.iterations)?;
            read_usize(m, "scenarios.operations", "batch", &mut o.batch)?;
            read_usize(
                m,
                "scenarios.operations",
                "seed_documents",
                &mut o.seed_documents,
            )?;
        }
        if let Some(v) = map.get("datasets") {
            let m = object(v, "scenarios.datasets")?;
            known(
                m,
                "scenarios.datasets",
                &["sizes", "batch", "query_warmup", "query_runs"],
            )?;
            let d = &mut self.datasets;
            read_usize(m, "scenarios.datasets", "batch", &mut d.batch)?;
            read_usize(m, "scenarios.datasets", "query_warmup", &mut d.query_warmup)?;
            read_usize(m, "scenarios.datasets", "query_runs", &mut d.query_runs)?;
            if let Some(sizes) = m.get("sizes") {
                let list = sizes
                    .as_array()
                    .ok_or("scenarios.datasets.sizes: expected an array")?;
                d.sizes = list
                    .iter()
                    .map(|size| {
                        let s = object(size, "scenarios.datasets.sizes[]")?;
                        known(
                            s,
                            "scenarios.datasets.sizes[]",
                            &["documents", "warmup", "runs"],
                        )?;
                        let mut documents = 0;
                        read_usize(s, "scenarios.datasets.sizes[]", "documents", &mut documents)?;
                        if documents == 0 {
                            return Err("scenarios.datasets.sizes[].documents: expected > 0".into());
                        }
                        let mut r = size_runs(documents, runs(3, 10));
                        read_runs(s, "scenarios.datasets.sizes[]", &mut r)?;
                        Ok(Size { documents, runs: r })
                    })
                    .collect::<Result<_, String>>()?;
            }
        }
        for (key, ci) in [("ci", &mut self.ci), ("ci_firebase", &mut self.ci_firebase)] {
            if let Some(v) = map.get(key) {
                let what = format!("scenarios.{key}");
                let m = object(v, &what)?;
                known(m, &what, &["warmup", "runs", "tests", "seed_documents"])?;
                read_runs(m, &what, &mut ci.runs)?;
                read_usize(m, &what, "tests", &mut ci.tests)?;
                read_usize(m, &what, "seed_documents", &mut ci.seed_documents)?;
            }
        }
        if let Some(v) = map.get("listen_writes") {
            let what = "scenarios.listen_writes";
            let m = object(v, what)?;
            known(m, what, &["warmup", "runs", "documents", "batch"])?;
            let l = &mut self.listen_writes;
            read_runs(m, what, &mut l.runs)?;
            read_usize(m, what, "documents", &mut l.documents)?;
            read_usize(m, what, "batch", &mut l.batch)?;
            if l.documents == 0 || l.batch == 0 {
                return Err(format!("{what}: documents and batch must be at least 1"));
            }
        }
        if let Some(v) = map.get("owner_writes") {
            let what = "scenarios.owner_writes";
            let m = object(v, what)?;
            known(
                m,
                what,
                &[
                    "warmup",
                    "runs",
                    "commit_warmup",
                    "commits",
                    "batch",
                    "documents",
                ],
            )?;
            let o = &mut self.owner_writes;
            read_runs(m, what, &mut o.runs)?;
            read_usize(m, what, "commit_warmup", &mut o.commit_warmup)?;
            read_usize(m, what, "commits", &mut o.commits)?;
            read_usize(m, what, "batch", &mut o.batch)?;
            read_usize(m, what, "documents", &mut o.documents)?;
            if o.commits == 0 || o.batch == 0 || o.documents == 0 {
                return Err(format!(
                    "{what}: commits, batch and documents must be at least 1"
                ));
            }
        }
        if let Some(v) = map.get("command") {
            let m = object(v, "scenarios.command")?;
            known(m, "scenarios.command", &["warmup", "runs"])?;
            read_runs(m, "scenarios.command", &mut self.command)?;
        }
        Ok(())
    }

    fn apply_stability(&mut self, stability: &Value) -> Result<(), String> {
        let m = object(stability, "stability")?;
        let s = &mut self.stability;
        let fields: [(&str, &mut Option<f64>); 6] = [
            ("max_load_1m", &mut s.max_load_1m),
            ("min_available_memory_mib", &mut s.min_available_memory_mib),
            ("max_cpu_steal_percent", &mut s.max_cpu_steal_percent),
            ("max_cpu_temperature_c", &mut s.max_cpu_temperature_c),
            ("max_startup_cv", &mut s.max_startup_cv),
            (
                "max_known_job_cpu_percent",
                &mut s.max_known_job_cpu_percent,
            ),
        ];
        let mut names: Vec<&str> = fields.iter().map(|(name, _)| *name).collect();
        names.push("allow_battery");
        known(m, "stability", &names)?;
        for (name, slot) in fields {
            match m.get(name) {
                None => {}
                Some(Value::Null) => *slot = None,
                Some(v) => {
                    *slot = Some(
                        v.as_f64()
                            .ok_or_else(|| format!("stability.{name}: expected a number"))?,
                    );
                }
            }
        }
        if let Some(v) = m.get("allow_battery") {
            s.allow_battery = v
                .as_bool()
                .ok_or("stability.allow_battery: expected a boolean")?;
        }
        Ok(())
    }

    /// The parameters as recorded in a result (`benchmark.scenarios`), for the scenarios that ran.
    pub fn scenario_json(&self, name: &str) -> Value {
        let r = |r: Runs| json!({ "warmup": r.warmup, "runs": r.runs });
        match name {
            "startup" => r(self.startup),
            "memory" => json!({
                "warmup": self.memory.runs.warmup,
                "runs": self.memory.runs.runs,
                "idle_ms": self.memory.idle_ms,
                "documents": self.memory.documents,
                "batch": self.memory.batch,
            }),
            "operations" => json!({
                "warmup": self.operations.warmup,
                "iterations": self.operations.iterations,
                "batch": self.operations.batch,
                "seed_documents": self.operations.seed_documents,
            }),
            "datasets" => json!({
                "sizes": self.datasets.sizes.iter().map(|s| json!({
                    "documents": s.documents, "warmup": s.runs.warmup, "runs": s.runs.runs,
                })).collect::<Vec<_>>(),
                "batch": self.datasets.batch,
                "query_warmup": self.datasets.query_warmup,
                "query_runs": self.datasets.query_runs,
            }),
            "ci" | "ci_firebase" => {
                let ci = if name == "ci" {
                    &self.ci
                } else {
                    &self.ci_firebase
                };
                json!({
                    "warmup": ci.runs.warmup,
                    "runs": ci.runs.runs,
                    "tests": ci.tests,
                    "seed_documents": ci.seed_documents,
                })
            }
            "listen_writes" => json!({
                "warmup": self.listen_writes.runs.warmup,
                "runs": self.listen_writes.runs.runs,
                "documents": self.listen_writes.documents,
                "batch": self.listen_writes.batch,
            }),
            "owner_writes" => json!({
                "warmup": self.owner_writes.runs.warmup,
                "runs": self.owner_writes.runs.runs,
                "commit_warmup": self.owner_writes.commit_warmup,
                "commits": self.owner_writes.commits,
                "batch": self.owner_writes.batch,
                "documents": self.owner_writes.documents,
            }),
            "command" => r(self.command),
            _ => Value::Null,
        }
    }

    pub fn stability_json(&self) -> Value {
        let s = &self.stability;
        json!({
            "max_load_1m": s.max_load_1m,
            "min_available_memory_mib": s.min_available_memory_mib,
            "max_cpu_steal_percent": s.max_cpu_steal_percent,
            "max_cpu_temperature_c": s.max_cpu_temperature_c,
            "max_startup_cv": s.max_startup_cv,
            "max_known_job_cpu_percent": s.max_known_job_cpu_percent,
            "allow_battery": s.allow_battery,
        })
    }
}

/// A config's `official` block must name the pinned jar: the engine compares with that one.
fn check_official(official: &Value) -> Result<(), String> {
    let m = object(official, "official")?;
    known(m, "official", &["version", "url", "sha256", "size"])?;
    let expect = [
        ("version", json!(OFFICIAL_VERSION)),
        ("url", json!(OFFICIAL_URL)),
        ("sha256", json!(OFFICIAL_SHA256)),
        ("size", json!(OFFICIAL_SIZE)),
    ];
    for (key, value) in expect {
        if let Some(found) = m.get(key)
            && *found != value
        {
            return Err(format!(
                "official.{key}: {found} differs from the pin this hidane build carries ({value})"
            ));
        }
    }
    Ok(())
}

fn object<'a>(value: &'a Value, what: &str) -> Result<&'a Map<String, Value>, String> {
    value
        .as_object()
        .ok_or_else(|| format!("{what}: expected an object"))
}

fn known(map: &Map<String, Value>, what: &str, keys: &[&str]) -> Result<(), String> {
    match map.keys().find(|k| !keys.contains(&k.as_str())) {
        Some(key) => Err(format!(
            "{what}: unknown key {key:?} (known: {})",
            keys.join(", ")
        )),
        None => Ok(()),
    }
}

fn read_usize(
    map: &Map<String, Value>,
    what: &str,
    key: &str,
    slot: &mut usize,
) -> Result<(), String> {
    if let Some(v) = map.get(key) {
        *slot = v
            .as_u64()
            .and_then(|n| usize::try_from(n).ok())
            .ok_or_else(|| format!("{what}.{key}: expected a non-negative integer"))?;
    }
    Ok(())
}

fn read_u64(map: &Map<String, Value>, what: &str, key: &str, slot: &mut u64) -> Result<(), String> {
    if let Some(v) = map.get(key) {
        *slot = v
            .as_u64()
            .ok_or_else(|| format!("{what}.{key}: expected a non-negative integer"))?;
    }
    Ok(())
}

fn read_runs(map: &Map<String, Value>, what: &str, slot: &mut Runs) -> Result<(), String> {
    read_usize(map, what, "warmup", &mut slot.warmup)?;
    read_usize(map, what, "runs", &mut slot.runs)?;
    if slot.runs == 0 {
        return Err(format!("{what}.runs: expected at least 1"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dataset_sizes_scale_down_above_1000_documents() {
        let base = runs(3, 10);
        assert_eq!(size_runs(100, base), runs(3, 10));
        assert_eq!(size_runs(1_000, base), runs(3, 10));
        assert_eq!(size_runs(10_000, base), runs(1, 5));
        assert_eq!(size_runs(100_000, base), runs(1, 3));
        assert_eq!(size_runs(100_000, runs(0, 2)), runs(0, 2));
    }

    #[test]
    fn config_overrides_and_rejects_unknown_keys() {
        let mut params = Params::default();
        params
            .apply_config(
                &json!({
                    "benchmark_version": "9",
                    "scenarios": {"startup": {"runs": 4}, "datasets": {"sizes": [{"documents": 50000}]}},
                    "stability": {"max_load_1m": null, "max_cpu_steal_percent": 2.5},
                }),
                false,
            )
            .unwrap();
        assert_eq!(params.version, "9");
        assert_eq!(params.startup, runs(3, 4));
        assert_eq!(
            params.datasets.sizes,
            [Size {
                documents: 50_000,
                runs: runs(1, 3)
            }]
        );
        assert_eq!(params.stability.max_load_1m, None);
        assert_eq!(params.stability.max_cpu_steal_percent, Some(2.5));

        let mut params = Params::default();
        let err = params
            .apply_config(&json!({"scenarios": {"startup": {"run": 4}}}), false)
            .unwrap_err();
        assert!(err.contains("unknown key \"run\""), "{err}");
        let err = params
            .apply_config(&json!({"official": {"sha256": "00"}}), false)
            .unwrap_err();
        assert!(err.contains("official.sha256"), "{err}");
        let err = params
            .apply_config(&json!({"scenarios": {"ci": {"runs": 0}}}), false)
            .unwrap_err();
        assert!(err.contains("at least 1"), "{err}");
    }

    /// `bench/canonical.json` states the built-in definitions; the two must not drift apart.
    /// (Skipped in a packaged crate, which does not carry the file.)
    #[test]
    fn canonical_json_matches_the_built_in_definitions() {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../bench/canonical.json");
        let Ok(text) = std::fs::read_to_string(path) else {
            return;
        };
        let config: Value = serde_json::from_str(&text).unwrap();
        let mut params = Params::default();
        params.apply_config(&config, false).unwrap();
        assert_eq!(params, Params::default());
        let mut quick = Params::quick();
        quick.apply_config(&config, true).unwrap();
        assert_eq!(quick, Params::quick());
        assert_eq!(config["benchmark_version"], BENCHMARK_VERSION);
        // Every scenario and limit is stated, not left to the defaults.
        let scenarios = config["scenarios"].as_object().unwrap();
        for name in [
            "startup",
            "memory",
            "operations",
            "datasets",
            "ci",
            "ci_firebase",
            "listen_writes",
            "owner_writes",
            "command",
        ] {
            assert_eq!(scenarios[name], params.scenario_json(name), "{name}");
        }
        let stability = config["stability"].as_object().unwrap();
        for (key, value) in params.stability_json().as_object().unwrap() {
            let stated = &stability[key];
            assert!(
                stated.as_f64() == value.as_f64() && stated.as_bool() == value.as_bool(),
                "{key}"
            );
        }
    }

    #[test]
    fn quick_block_applies_only_with_quick() {
        let config = json!({"quick": {"scenarios": {"startup": {"warmup": 0, "runs": 2}}}});
        let mut params = Params::default();
        params.apply_config(&config, false).unwrap();
        assert_eq!(params.startup, runs(3, 10));
        params.apply_config(&config, true).unwrap();
        assert_eq!(params.startup, runs(0, 2));
    }
}
