//! Export and import against the official emulator's recordings (`fixtures/export_import.json`,
//! from `tools/oracle/export_import.py`): the exported files byte for byte, what an import
//! writes where, the endpoints' answers, `--seed_from_export` and the start-up checks.

mod common;

use std::{
    collections::BTreeMap,
    fs,
    net::SocketAddr,
    path::{Path, PathBuf},
    sync::atomic::{AtomicUsize, Ordering},
};

use base64::{Engine as _, engine::general_purpose::STANDARD};
use hidane_core::{
    export::{entity, log},
    normalize::normalize_value,
};
use hidane_proto::google::firestore::v1::{MapValue, Value, value::ValueType};
use prost::Message as _;
use serde_json::{Value as Json, json};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
};

const PROJECT: &str = "demo-export";
const META: &str = "firestore_export.overall_export_metadata";
const OUTPUT: &str = "all_namespaces/all_kinds/output-0";
const KIND_METADATA: &str = "all_namespaces/all_kinds/all_namespaces_all_kinds.export_metadata";

fn fixture() -> Json {
    serde_json::from_str(include_str!("fixtures/export_import.json")).unwrap()
}

/// A directory removed when dropped.
struct TempDir(PathBuf);

impl TempDir {
    fn new() -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let path = std::env::temp_dir().join(format!(
            "hidane-export-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).unwrap();
        Self(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }

    fn text(&self) -> String {
        self.0.to_str().unwrap().to_owned()
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// Writes a recorded export's files into `dir`.
fn unpack(files: &Json, dir: &Path) {
    for (path, data) in files.as_object().unwrap() {
        let path = dir.join(path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, STANDARD.decode(data.as_str().unwrap()).unwrap()).unwrap();
    }
}

fn file(files: &Json, path: &str) -> Vec<u8> {
    STANDARD.decode(files[path].as_str().unwrap()).unwrap()
}

/// The records of a log, sorted: the official emulator writes them in no particular order.
fn sorted_records(data: &[u8]) -> Vec<Vec<u8>> {
    let mut records: Vec<Vec<u8>> = log::records(data)
        .map(|r| r.unwrap().into_owned())
        .collect();
    records.sort();
    records
}

/// What a stored document holds: timestamps to the microsecond; an export also writes `-0.0`
/// as `0.0`.
fn stored(json: &Json) -> BTreeMap<String, Value> {
    fn zero(value: &mut Value) {
        match &mut value.value_type {
            Some(ValueType::DoubleValue(d)) if *d == 0.0 => *d = 0.0,
            Some(ValueType::ArrayValue(a)) => a.values.iter_mut().for_each(zero),
            Some(ValueType::MapValue(m)) => m.fields.values_mut().for_each(zero),
            _ => {}
        }
    }
    let mut fields = common::protojson::fields(json);
    for value in fields.values_mut() {
        normalize_value(value);
        zero(value);
    }
    fields
}

/// Bit for bit, so NaN equals NaN.
fn exact(a: &BTreeMap<String, Value>, b: &BTreeMap<String, Value>) -> bool {
    let encode = |fields: &BTreeMap<String, Value>| {
        MapValue {
            fields: fields.clone(),
        }
        .encode_to_vec()
    };
    encode(a) == encode(b)
}

fn documents_of<'a>(fixture: &'a Json, database: &'a str) -> impl Iterator<Item = &'a Json> {
    fixture["documents"]
        .as_array()
        .unwrap()
        .iter()
        .filter(move |d| d["database"] == database)
}

#[test]
fn official_exports_read_back_as_the_written_documents() {
    let fixture = fixture();
    for (key, database) in [("default", "(default)"), ("db2", "db2")] {
        let dir = TempDir::new();
        unpack(&fixture["exports"][key], dir.path());
        let read = hidane::export::read(&dir.path().join(META)).unwrap();
        let expected: Vec<&Json> = documents_of(&fixture, database).collect();
        assert_eq!(read.len(), expected.len(), "{key}");
        for doc in expected {
            let path = doc["path"].as_str().unwrap();
            let found = read
                .iter()
                .find(|d| d.path.to_string() == path)
                .unwrap_or_else(|| panic!("{key}: {path} missing"));
            assert_eq!(found.database_id, database);
            assert!(
                exact(&found.fields, &stored(&doc["fields"])),
                "{key} {path}:\n{:#?}",
                found.fields
            );
        }
    }
}

#[test]
fn entities_are_encoded_as_the_official_emulator_encodes_them() {
    let fixture = fixture();
    for (key, database) in [("default", "(default)"), ("db2", "db2")] {
        let official = sorted_records(&file(&fixture["exports"][key], OUTPUT));
        let mut ours: Vec<Vec<u8>> = documents_of(&fixture, database)
            .map(|doc| {
                let path =
                    hidane_core::path::ResourcePath::parse(doc["path"].as_str().unwrap()).unwrap();
                entity::encode(PROJECT, database, &path, &stored(&doc["fields"]))
            })
            .collect();
        ours.sort();
        assert_eq!(ours.len(), official.len());
        for (ours, official) in ours.iter().zip(&official) {
            assert_eq!(ours, official, "{key}");
        }
    }
}

async fn start(admin: hidane::Admin) -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(hidane::serve(
        vec![listener],
        hidane::grpc_routes(&admin),
        hidane::http_routes(admin),
        std::future::pending(),
    ));
    addr
}

/// Percent-encodes what a request line cannot carry.
fn encode(path: &str) -> String {
    let mut out = String::new();
    for byte in path.bytes() {
        if byte.is_ascii_alphanumeric() || b"/:()-_.~".contains(&byte) {
            out.push(char::from(byte));
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

struct Answer {
    status: u16,
    content_type: Option<String>,
    body: String,
}

async fn http(
    addr: SocketAddr,
    method: &str,
    path: &str,
    body: Option<&str>,
    headers: &[(String, String)],
) -> Answer {
    let mut request = format!(
        "{method} {} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n",
        encode(path)
    );
    if !headers
        .iter()
        .any(|(k, _)| k.eq_ignore_ascii_case("content-type"))
    {
        request.push_str("Content-Type: application/json\r\n");
    }
    for (name, value) in headers {
        request.push_str(&format!("{name}: {value}\r\n"));
    }
    let body = body.unwrap_or_default();
    request.push_str(&format!("Content-Length: {}\r\n\r\n{body}", body.len()));
    let mut stream = TcpStream::connect(addr).await.unwrap();
    stream.write_all(request.as_bytes()).await.unwrap();
    let mut raw = Vec::new();
    stream.read_to_end(&mut raw).await.unwrap();
    let raw = String::from_utf8(raw).unwrap();
    let (head, body) = raw.split_once("\r\n\r\n").unwrap();
    let content_type = head.lines().find_map(|line| {
        let (name, value) = line.split_once(": ")?;
        name.eq_ignore_ascii_case("content-type")
            .then(|| value.to_owned())
    });
    Answer {
        status: head[9..12].parse().unwrap(),
        content_type,
        body: body.to_owned(),
    }
}

async fn json_call(addr: SocketAddr, method: &str, path: &str, body: &Json) -> (u16, Json) {
    let body = (!body.is_null()).then(|| body.to_string());
    let answer = http(addr, method, path, body.as_deref(), &[]).await;
    (
        answer.status,
        serde_json::from_str(&answer.body).unwrap_or(Json::Null),
    )
}

async fn write_documents(addr: SocketAddr, project: &str, documents: &[&Json]) {
    for doc in documents {
        let path = format!(
            "/v1/projects/{project}/databases/{}/documents/{}",
            doc["database"].as_str().unwrap(),
            doc["path"].as_str().unwrap()
        );
        let (status, body) =
            json_call(addr, "PATCH", &path, &json!({"fields": doc["fields"]})).await;
        assert_eq!(status, 200, "{path}: {body}");
    }
}

async fn export(addr: SocketAddr, project: &str, database: &str, dir: &Path) -> PathBuf {
    fs::create_dir_all(dir).unwrap();
    let (status, body) = json_call(
        addr,
        "POST",
        &format!("/emulator/v1/projects/{project}:export"),
        &json!({
            "database": format!("projects/{project}/databases/{database}"),
            "export_directory": dir.to_str().unwrap(),
            "export_name": "firestore_export",
        }),
    )
    .await;
    assert_eq!(status, 200, "{body}");
    dir.join("firestore_export")
}

#[tokio::test]
async fn exports_match_the_official_files() {
    let fixture = fixture();
    let addr = start(hidane::Admin::default()).await;
    let documents: Vec<&Json> = fixture["documents"].as_array().unwrap().iter().collect();
    write_documents(addr, PROJECT, &documents).await;
    let dir = TempDir::new();
    for (key, project, database) in [
        ("default", PROJECT, "(default)"),
        ("db2", PROJECT, "db2"),
        ("empty", "demo-empty", "(default)"),
    ] {
        let root = export(addr, project, database, &dir.path().join(key)).await;
        let official = &fixture["exports"][key];
        let mut written: Vec<String> = Vec::new();
        for entry in walk(&root) {
            written.push(
                entry
                    .strip_prefix(&root)
                    .unwrap()
                    .to_str()
                    .unwrap()
                    .replace('\\', "/"),
            );
        }
        written.sort();
        let expected: Vec<String> = official.as_object().unwrap().keys().cloned().collect();
        assert_eq!(written, expected, "{key}");
        assert_eq!(
            fs::read(root.join(META)).unwrap(),
            file(official, META),
            "{key}"
        );
        if key != "empty" {
            let output = fs::read(root.join(OUTPUT)).unwrap();
            assert_eq!(output.len(), file(official, OUTPUT).len(), "{key}");
            assert_eq!(
                sorted_records(&output),
                sorted_records(&file(official, OUTPUT))
            );
            // Same name and files; the times differ.
            let metadata = fs::read(root.join(KIND_METADATA)).unwrap();
            assert_eq!(metadata.len(), file(official, KIND_METADATA).len());
            assert_eq!(
                hidane_core::export::parse_export_metadata(&metadata),
                hidane_core::export::parse_export_metadata(&file(official, KIND_METADATA))
            );
        }
    }

    // A record over two block boundaries.
    let big = &fixture["big"];
    let length = usize::try_from(big["length"].as_u64().unwrap()).unwrap();
    let doc = json!({"database": "(default)", "path": big["path"],
                     "fields": {"s": {"stringValue": "y".repeat(length)}}});
    write_documents(addr, "demo-big", &[&doc]).await;
    let root = export(addr, "demo-big", "(default)", &dir.path().join("big")).await;
    let output = fs::read(root.join(OUTPUT)).unwrap();
    assert_eq!(output.len() as u64, big["output_len"].as_u64().unwrap());
    assert_eq!(
        u64::from(log::crc32c(&output)),
        big["output_crc32c"].as_u64().unwrap()
    );
}

fn walk(dir: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    for entry in fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            files.extend(walk(&path));
        } else {
            files.push(path);
        }
    }
    files
}

async fn all_documents(addr: SocketAddr, project: &str, database: &str) -> Vec<Json> {
    let (status, rows) = json_call(
        addr,
        "POST",
        &format!("/v1/projects/{project}/databases/{database}/documents:runQuery"),
        &json!({"structuredQuery": {"from": [{"allDescendants": true}]}}),
    )
    .await;
    assert_eq!(status, 200);
    rows.as_array()
        .unwrap()
        .iter()
        .filter_map(|row| row.get("document"))
        .map(|doc| {
            json!({
                "name": doc["name"],
                "fields": doc.get("fields").cloned().unwrap_or_else(|| json!({})),
                "createTimeIsUpdateTime": doc["createTime"] == doc["updateTime"],
            })
        })
        .collect()
}

async fn import(addr: SocketAddr, project: &str, database: &str, meta: &Path) -> (u16, Json) {
    json_call(
        addr,
        "POST",
        &format!("/emulator/v1/projects/{project}:import"),
        &json!({
            "database": format!("projects/{project}/databases/{database}"),
            "export_directory": meta.to_str().unwrap(),
        }),
    )
    .await
}

#[tokio::test]
async fn imports_write_what_the_official_emulator_writes() {
    let fixture = fixture();
    let dir = TempDir::new();
    unpack(&fixture["exports"]["default"], &dir.path().join("default"));
    unpack(&fixture["exports"]["db2"], &dir.path().join("db2"));
    let default_meta = dir.path().join("default").join(META);
    let db2_meta = dir.path().join("db2").join(META);
    let addr = start(hidane::Admin::default()).await;

    for (target, meta) in [
        ("demo-imported/(default)", &default_meta),
        ("demo-imported/db2", &db2_meta),
        ("demo-mismatch/(default)", &db2_meta),
        ("demo-mismatch/db2", &default_meta),
    ] {
        let (project, database) = target.split_once('/').unwrap();
        let (status, body) = import(addr, project, database, meta).await;
        assert_eq!(status, 200, "{body}");
        let imported = all_documents(addr, project, database).await;
        assert_eq!(
            Json::Array(imported),
            fixture["imported"][target],
            "{target}"
        );
    }

    // Over existing documents, and twice.
    let types = documents_of(&fixture, "(default)")
        .find(|d| d["path"] == "all/types")
        .unwrap();
    let get = |path: &'static str| async move {
        json_call(
            addr,
            "GET",
            &format!("/v1/projects/demo-over/databases/(default)/documents/{path}"),
            &Json::Null,
        )
        .await
        .1
    };
    write_documents(
        addr,
        "demo-over",
        &[
            &json!({"database": "(default)", "path": "all/types", "fields": {"mine": {"integerValue": "1"}}}),
            &json!({"database": "(default)", "path": "keep/doc", "fields": {"keep": {"integerValue": "2"}}}),
        ],
    )
    .await;
    let before = get("all/types").await;
    let keep_before = get("keep/doc").await;
    import(addr, "demo-over", "(default)", &default_meta).await;
    let after = get("all/types").await;
    import(addr, "demo-over", "(default)", &default_meta).await;
    let again = get("all/types").await;
    let keep_after = get("keep/doc").await;
    let keys = |doc: &Json| {
        let mut keys: Vec<String> = doc["fields"].as_object().unwrap().keys().cloned().collect();
        keys.sort();
        keys
    };
    let observed = json!({
        "fields_replaced": keys(&after) == keys(types),
        "create_time_kept": after["createTime"] == before["createTime"],
        "update_time_changed": after["updateTime"] != before["updateTime"],
        "reimport_keeps_update_time": again["updateTime"] == after["updateTime"],
        "other_document_untouched": keep_after == keep_before,
    });
    assert_eq!(observed, fixture["import_over"]);
}

/// A managed export's layout (a kind directory per collection, several output files, the app
/// `s~{project}`, indexed properties), made by the oracle from the official export.
#[tokio::test]
async fn production_shaped_exports_import_like_the_official_emulator() {
    let fixture = fixture();
    let dir = TempDir::new();
    unpack(&fixture["production"]["files"], dir.path());
    let addr = start(hidane::Admin::default()).await;
    let meta = dir.path().join("production.overall_export_metadata");
    let (status, body) = import(addr, "demo-production", "(default)", &meta).await;
    assert_eq!(status, 200, "{body}");
    let imported = all_documents(addr, "demo-production", "(default)").await;
    assert_eq!(Json::Array(imported), fixture["production"]["imported"]);
}

/// Copies of the (default) export with one part missing or broken, and two bad metadata
/// files, as the oracle makes them.
fn damaged(fixture: &Json, dir: &Path) {
    for variant in ["nometa", "nooutput", "crc", "trunc"] {
        unpack(
            &fixture["exports"]["default"],
            &dir.join("bad").join(variant),
        );
    }
    let bad = dir.join("bad");
    fs::remove_file(bad.join("nometa").join(KIND_METADATA)).unwrap();
    fs::remove_file(bad.join("nooutput").join(OUTPUT)).unwrap();
    let path = bad.join("crc").join(OUTPUT);
    let mut data = fs::read(&path).unwrap();
    data[20] ^= 0xff;
    fs::write(&path, data).unwrap();
    let path = bad.join("trunc").join(OUTPUT);
    let data = fs::read(&path).unwrap();
    fs::write(&path, &data[..data.len() / 2]).unwrap();
    fs::write(
        bad.join("version.overall_export_metadata"),
        log::write_all([&[0x34u8][..]]),
    )
    .unwrap();
    fs::write(bad.join("garbage.overall_export_metadata"), b"garbage").unwrap();
}

/// Export names made of the time.
fn untimed(text: &str) -> String {
    let mut out = String::new();
    let mut rest = text;
    while let Some(at) = rest.find("firestore_export_") {
        let (before, after) = rest.split_at(at + "firestore_export_".len());
        out.push_str(before);
        let digits = after.chars().take_while(char::is_ascii_digit).count();
        out.push_str(if digits > 0 { "{seconds}" } else { "" });
        rest = &after[digits..];
    }
    out.push_str(rest);
    out
}

#[tokio::test]
async fn the_endpoints_answer_like_the_official_emulator() {
    let fixture = fixture();
    let dir = TempDir::new();
    let work = dir.text();
    fs::create_dir_all(dir.path().join("out")).unwrap();
    fs::write(dir.path().join("out/file.txt"), b"").unwrap();
    unpack(
        &fixture["exports"]["default"],
        &dir.path().join("default/firestore_export"),
    );
    damaged(&fixture, dir.path());
    let addr = start(hidane::Admin::default()).await;
    let documents: Vec<&Json> = fixture["documents"].as_array().unwrap().iter().collect();
    write_documents(addr, PROJECT, &documents).await;

    let mut differences = Vec::new();
    for case in fixture["requests"].as_array().unwrap() {
        let fill = |text: &str| text.replace("{dir}", &work);
        let body = match (&case["raw"], &case["body"]) {
            (Json::String(raw), _) => Some(raw.clone()),
            (_, Json::Null) => None,
            (_, body) => Some(fill(&body.to_string())),
        };
        let headers: Vec<(String, String)> = case["headers"]
            .as_object()
            .map(|h| {
                h.iter()
                    .map(|(k, v)| (k.clone(), v.as_str().unwrap().to_owned()))
                    .collect()
            })
            .unwrap_or_default();
        let answer = http(
            addr,
            case["method"].as_str().unwrap(),
            case["path"].as_str().unwrap(),
            body.as_deref(),
            &headers,
        )
        .await;
        let response = answer
            .body
            .replace(&work.replace('/', "\\/"), "{dir}")
            .replace(&work, "{dir}");
        let mut listing: Vec<String> = fs::read_dir(dir.path().join("out"))
            .unwrap()
            .map(|e| untimed(e.unwrap().file_name().to_str().unwrap()))
            .collect();
        listing.sort();
        let mut actual = json!({
            "status": answer.status,
            "content_type": answer.content_type,
            "response": untimed(&response),
        });
        let mut expected = json!({
            "status": case["status"],
            "content_type": case["content_type"],
            "response": untimed(case["response"].as_str().unwrap()),
        });
        let name = case["name"].as_str().unwrap();
        if name.starts_with("export") {
            actual["lists"] = json!(listing);
            expected["lists"] = json!(
                case["export_directory_lists"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|n| untimed(n.as_str().unwrap()))
                    .collect::<Vec<_>>()
            );
        }
        if let Some(count) = case.get("imported") {
            let database = serde_json::from_str::<Json>(&fill(&case["body"].to_string())).unwrap();
            let project = database["database"]
                .as_str()
                .unwrap()
                .split('/')
                .nth(1)
                .unwrap()
                .to_owned();
            actual["imported"] = json!(all_documents(addr, &project, "(default)").await.len());
            expected["imported"] = count.clone();
        }
        if actual != expected {
            differences.push(format!(
                "{name}:\n    official {expected}\n    hidane   {actual}"
            ));
        }
    }
    assert!(differences.is_empty(), "{}", differences.join("\n"));
}

#[tokio::test]
async fn databases_are_seeded_like_the_official_emulator() {
    let fixture = fixture();
    let dir = TempDir::new();
    unpack(&fixture["exports"]["default"], dir.path());
    let seed = hidane::export::read(&dir.path().join(META)).unwrap();
    let addr = start(hidane::Admin::default().with_seed(seed)).await;
    for step in fixture["seeding"].as_array().unwrap() {
        let method = step["method"].as_str().unwrap();
        let path = step["path"].as_str().unwrap();
        let body = if method == "PATCH" {
            json!({"fields": {"n": {"integerValue": "1"}}})
        } else {
            Json::Null
        };
        let (status, doc) = json_call(addr, method, path, &body).await;
        let mut actual =
            json!({"what": step["what"], "method": method, "path": path, "status": status});
        if method == "GET" && status == 200 {
            let mut fields: Vec<String> =
                doc["fields"].as_object().unwrap().keys().cloned().collect();
            fields.sort();
            actual["fields"] = json!(fields);
            actual["createTimeIsUpdateTime"] = json!(doc["createTime"] == doc["updateTime"]);
        }
        assert_eq!(&actual, step);
    }
}

#[cfg(unix)]
mod process {
    use std::{
        io::{BufRead, BufReader, Read},
        process::{Command, Stdio},
        sync::mpsc,
        thread,
        time::{Duration, Instant},
    };

    use super::*;

    const BIN: &str = env!("CARGO_BIN_EXE_hidane");

    fn run_to_exit(args: &[String]) -> (i32, String) {
        let output = Command::new(BIN)
            .args(["--host", "127.0.0.1", "--port", "0"])
            .args(args)
            .output()
            .unwrap();
        (
            output.status.code().unwrap_or(-1),
            String::from_utf8(output.stderr).unwrap(),
        )
    }

    /// Starts hidane and returns it with its port once it is ready.
    fn spawn(args: &[String]) -> (std::process::Child, u16) {
        let mut child = Command::new(BIN)
            .args(["--host", "127.0.0.1", "--port", "0"])
            .args(args)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let stdout = child.stdout.take().unwrap();
        let (tx, rx) = mpsc::channel();
        thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                let _ = tx.send(line);
            }
        });
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut port = None;
        while let Ok(line) = rx.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
            if let Some(endpoint) = line.strip_prefix("API endpoint: http://") {
                port = endpoint.rsplit(':').next().and_then(|p| p.parse().ok());
            }
            if line == "Dev App Server is now running." {
                return (child, port.unwrap());
            }
        }
        let _ = child.kill();
        let _ = child.wait();
        panic!("hidane did not start");
    }

    fn interrupt(mut child: std::process::Child) -> i32 {
        assert!(
            Command::new("kill")
                .args(["-INT", &child.id().to_string()])
                .status()
                .unwrap()
                .success()
        );
        let status = child.wait().unwrap();
        let mut stderr = String::new();
        let _ = child.stderr.take().unwrap().read_to_string(&mut stderr);
        status.code().unwrap_or(-1)
    }

    #[tokio::test]
    async fn start_up_like_the_official_emulator() {
        let fixture = fixture();
        let dir = TempDir::new();
        let work = dir.text();
        unpack(&fixture["exports"]["default"], dir.path());
        damaged(&fixture, dir.path());
        for case in fixture["startup"].as_array().unwrap() {
            let name = case["name"].as_str().unwrap();
            match name {
                "--import-data seeds like --seed_from_export" => {
                    let (child, port) =
                        spawn(&["--import-data".to_owned(), format!("{work}/{META}")]);
                    let addr = SocketAddr::from(([127, 0, 0, 1], port));
                    let (status, _) = json_call(
                        addr,
                        "GET",
                        "/v1/projects/p/databases/(default)/documents/c/a",
                        &Json::Null,
                    )
                    .await;
                    interrupt(child);
                    assert_eq!(json!(status), case["status"], "{name}");
                }
                "--export-on-exit writes nothing" => {
                    let target = dir.path().join("on-exit");
                    fs::create_dir_all(&target).unwrap();
                    let (child, port) = spawn(&[
                        "--export-on-exit".to_owned(),
                        target.to_str().unwrap().to_owned(),
                        "--export-name".to_owned(),
                        "named".to_owned(),
                    ]);
                    let addr = SocketAddr::from(([127, 0, 0, 1], port));
                    json_call(
                        addr,
                        "PATCH",
                        "/v1/projects/p/databases/(default)/documents/c/a",
                        &json!({"fields": {"n": {"integerValue": "1"}}}),
                    )
                    .await;
                    let code = interrupt(child);
                    let files = fs::read_dir(&target).unwrap().count();
                    assert_eq!(json!(code), case["exit"], "{name}");
                    assert_eq!(files, case["files"].as_array().unwrap().len(), "{name}");
                }
                _ => {
                    let args: Vec<String> = case["args"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .map(|a| a.as_str().unwrap().replace("{dir}", &work))
                        .collect();
                    let (code, stderr) = run_to_exit(&args);
                    let message = case["message"].as_str().unwrap().replace("{dir}", &work);
                    assert_eq!(json!(code), case["exit"], "{name}: {stderr}");
                    assert!(stderr.contains(&message), "{name}: {stderr}");
                }
            }
        }
    }
}
