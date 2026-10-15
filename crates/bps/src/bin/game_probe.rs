//! Probe the vanilla Minecraft client and server jars for every schema version.
//!
//! The server probe boots Mojang's dedicated server (the JDK that version needs) and speaks
//! status and login with the BPS schema: serverbound packets are what a client sends, clientbound
//! packets are what the server sends. The client probe reads the client jar's protocol classes
//! and compares them with the server jar when both ship readable names (26.1 and later).
//!
//! ```text
//! cargo run -q -p bps --bin game_probe -- --only 26.3
//! cargo run -q -p bps --bin game_probe
//! ```
//!
//! Reports land in `--out` (default `target/game-probes/<release>.json`). Jars and worlds are
//! cached under `~/.cache/minecraft-vanilla`. Nothing here is a unit test: it needs a network
//! and a few minutes of server startup per version.

use bps::ast::{BlockItem, FieldType, WireField};
use bps::interp::{decode_frame_payload, encode_packet};
use bps::json::{dumps_sorted, loads};
use bps::ordmap::OrderedMap;
use bps::protodef::versions::{VersionEntry, parse_manifest};
use bps::value::{Env, Value};
use bps::{ResolvedSchema, analyze, load_schema};
use flate2::read::ZlibDecoder;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const MANIFEST_URL: &str = "https://piston-meta.mojang.com/mc/game/version_manifest_v2.json";

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn cache_root() -> PathBuf {
    std::env::var_os("GAME_PROBE_CACHE")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| "/tmp".into())).join(".cache/minecraft-vanilla"))
}

fn main() -> Exit {
    let mut only: Vec<String> = Vec::new();
    let mut out = repo_root().join("target/game-probes");
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--only" => {
                let Some(v) = args.next() else {
                    eprintln!("game_probe: --only needs a release");
                    return Exit(2);
                };
                only.push(v);
            }
            "--out" => {
                let Some(v) = args.next() else {
                    eprintln!("game_probe: --out needs a directory");
                    return Exit(2);
                };
                out = PathBuf::from(v);
            }
            "-h" | "--help" => {
                println!("usage: game_probe [--only RELEASE]... [--out DIR]");
                return Exit(0);
            }
            other => {
                eprintln!("game_probe: unrecognized argument {other}");
                return Exit(2);
            }
        }
    }
    let manifest_text = fs::read_to_string(repo_root().join("schemas/versions/manifest.json"))
        .unwrap_or_else(|e| panic!("manifest: {e}"));
    let entries = parse_manifest(&manifest_text).unwrap_or_else(|e| panic!("manifest: {e}"));
    let selected: Vec<&VersionEntry> = if only.is_empty() {
        entries.iter().collect()
    } else {
        entries.iter().filter(|e| only.iter().any(|o| o == &e.release)).collect()
    };
    if selected.is_empty() {
        eprintln!("game_probe: no manifest entry matches --only");
        return Exit(2);
    }
    fs::create_dir_all(&out).unwrap_or_else(|e| panic!("{}: {e}", out.display()));
    let mut failed = 0;
    for entry in selected {
        let report = probe_version(entry);
        let path = out.join(format!("{}.json", entry.release));
        fs::write(&path, dumps_sorted(&report)).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        let ok = matches!(report.get("ok"), Some(Value::Int(1)));
        if !ok {
            failed += 1;
        }
        let err = report.get("error").and_then(|v| match v {
            Value::Str(s) => Some(s.to_string()),
            _ => None,
        });
        println!(
            "{} {} {}",
            entry.release,
            if ok { "ok" } else { "FAIL" },
            err.unwrap_or_default()
        );
    }
    Exit(if failed == 0 { 0 } else { 1 })
}

struct Exit(i32);
impl std::process::Termination for Exit {
    fn report(self) -> std::process::ExitCode {
        std::process::ExitCode::from(self.0 as u8)
    }
}

fn probe_version(entry: &VersionEntry) -> Value {
    let mut fields = OrderedMap::new();
    fields.insert("release", Value::str(&entry.release));
    fields.insert("protocol", Value::Int(i128::from(entry.protocol)));
    let result = (|| -> Result<Value, String> {
        let java = java_bin(&entry.release)?;
        let server_jar = ensure_download(&entry.release, "server")?;
        let client_jar = ensure_download(&entry.release, "client")?;
        let schema_path = repo_root().join("schemas/versions").join(entry.file_name());
        let resolved = analyze(&load_schema(&[schema_path]).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
        let _slot = ServerSlot::acquire()?;
        let mut server = Server::start(&entry.release, &server_jar, &java)?;
        let status = status_session(&resolved, entry.protocol, server.port)?;
        let login = login_session(&resolved, entry.protocol, server.port)?;
        server.stop();
        let client = client_jar_probe(&client_jar, &server_jar);
        let mut live = OrderedMap::new();
        live.insert("status", status);
        live.insert("login", login);
        live.insert("client_jar", client);
        Ok(Value::map(live))
    })();
    match result {
        Ok(v) => {
            fields.insert("ok", Value::Int(1));
            if let Value::Map(m) = v {
                for (k, val) in m.iter() {
                    fields.insert(k, val.clone());
                }
            }
        }
        Err(e) => {
            fields.insert("ok", Value::Int(0));
            fields.insert("error", Value::str(e));
        }
    }
    Value::map(fields)
}

fn ver_parts(v: &str) -> Vec<u32> {
    v.split('.').map(|p| p.parse().unwrap_or(0)).collect()
}

fn ver_ge(v: &str, min: &str) -> bool {
    let a = ver_parts(v);
    let b = ver_parts(min);
    let n = a.len().max(b.len());
    for i in 0..n {
        let av = a.get(i).copied().unwrap_or(0);
        let bv = b.get(i).copied().unwrap_or(0);
        if av != bv {
            return av > bv;
        }
    }
    true
}

/// JDK the dedicated server for `release` actually starts on.
fn java_major(release: &str) -> u32 {
    if ver_ge(release, "26") {
        25
    } else if ver_ge(release, "1.20.5") {
        21
    } else if ver_ge(release, "1.17") {
        17
    } else {
        8
    }
}

fn java_bin(release: &str) -> Result<PathBuf, String> {
    let major = java_major(release);
    if major == 25 {
        let path = PathBuf::from("/usr/lib/jvm/java-25-openjdk/bin/java");
        if path.is_file() {
            return Ok(path);
        }
        return Err("Java 25 is required for 26.x and was not at /usr/lib/jvm/java-25-openjdk".into());
    }
    let path = cache_root().join(format!("jdk/{major}/bin/java"));
    if path.is_file() {
        return Ok(path);
    }
    Err(format!("JDK {major} is required for {release} and was not at {}", path.display()))
}

fn ensure_download(release: &str, kind: &str) -> Result<PathBuf, String> {
    let dest_dir = cache_root().join("jars").join(release);
    fs::create_dir_all(&dest_dir).map_err(|e| e.to_string())?;
    let dest = dest_dir.join(format!("{kind}.jar"));
    let meta = version_json(release)?;
    let downloads = meta.get("downloads").ok_or("version json has no downloads")?;
    let Value::Map(dmap) = downloads else { return Err("downloads is not an object".into()) };
    let spec = dmap.get(kind).ok_or_else(|| format!("no {kind} download for {release}"))?;
    let url = str_field(spec, "url")?;
    let sha1 = str_field(spec, "sha1")?;
    if dest.is_file() && file_sha1(&dest)? == sha1 {
        return Ok(dest);
    }
    let partial = dest.with_extension("jar.partial");
    curl(&url, &partial)?;
    let got = file_sha1(&partial)?;
    if got != sha1 {
        let _ = fs::remove_file(&partial);
        return Err(format!("{release} {kind}: sha1 {got} != {sha1}"));
    }
    fs::rename(&partial, &dest).map_err(|e| e.to_string())?;
    Ok(dest)
}

fn version_json(release: &str) -> Result<Value, String> {
    let dir = cache_root().join("version-json");
    fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let path = dir.join(format!("{release}.json"));
    if !path.is_file() {
        let manifest_path = cache_root().join("version_manifest_v2.json");
        if !manifest_path.is_file() {
            curl(MANIFEST_URL, &manifest_path)?;
        }
        let text = fs::read_to_string(&manifest_path).map_err(|e| e.to_string())?;
        let manifest = loads(&text).map_err(|e| e.to_string())?;
        let versions = manifest.get("versions").ok_or("manifest has no versions")?;
        let Value::List(list) = versions else { return Err("versions is not a list".into()) };
        let url = list.iter().find_map(|v| {
            let Value::Map(_) = v else { return None };
            if str_field(v, "id").ok()?.as_str() == release { str_field(v, "url").ok() } else { None }
        }).ok_or_else(|| format!("{release} is not in Mojang's version manifest"))?;
        curl(&url, &path)?;
    }
    loads(&fs::read_to_string(&path).map_err(|e| e.to_string())?).map_err(|e| e.to_string())
}

fn str_field(v: &Value, key: &str) -> Result<String, String> {
    match v.get(key) {
        Some(Value::Str(s)) => Ok(s.to_string()),
        _ => Err(format!("missing string {key}")),
    }
}

fn curl(url: &str, dest: &Path) -> Result<(), String> {
    if let Some(parent) = dest.parent() {
        fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let status = Command::new("curl")
        .args(["-fsSL", "--retry", "3", "--retry-delay", "2", "-o"])
        .arg(dest)
        .arg(url)
        .status()
        .map_err(|e| format!("curl: {e}"))?;
    if !status.success() {
        return Err(format!("curl failed for {url}"));
    }
    Ok(())
}

fn file_sha1(path: &Path) -> Result<String, String> {
    let out = Command::new("sha1sum").arg(path).output().map_err(|e| format!("sha1sum: {e}"))?;
    if !out.status.success() {
        return Err(format!("sha1sum {}", path.display()));
    }
    let text = String::from_utf8_lossy(&out.stdout);
    Ok(text.split_whitespace().next().unwrap_or("").to_string())
}

/// At most three dedicated servers at once, so a full-version run does not exhaust RAM.
struct ServerSlot(PathBuf);

impl ServerSlot {
    fn acquire() -> Result<Self, String> {
        let dir = cache_root().join("slots");
        fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        let start = Instant::now();
        loop {
            for n in 0..3 {
                let path = dir.join(n.to_string());
                if OpenOptions::new().write(true).create_new(true).open(&path).is_ok() {
                    return Ok(Self(path));
                }
            }
            if start.elapsed() > Duration::from_secs(3600) {
                return Err("timed out waiting for a server slot".into());
            }
            std::thread::sleep(Duration::from_millis(400));
        }
    }
}

impl Drop for ServerSlot {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

struct Server {
    child: std::process::Child,
    port: u16,
}

impl Server {
    fn start(release: &str, jar: &Path, java: &Path) -> Result<Self, String> {
        let port = free_port()?;
        let dir = cache_root().join("servers").join(release);
        fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        fs::write(dir.join("eula.txt"), "eula=true\n").map_err(|e| e.to_string())?;
        fs::write(
            dir.join("server.properties"),
            format!(
                "online-mode=false\nserver-port={port}\nmotd=bps-probe\nlevel-type=flat\n\
                 spawn-monsters=false\nspawn-animals=false\nview-distance=2\nmax-players=2\n\
                 white-list=false\nenforce-whitelist=false\nenforce-secure-profile=false\n\
                 level-seed=bps\n"
            ),
        )
        .map_err(|e| e.to_string())?;
        let log_path = dir.join("server.log");
        let log = File::create(&log_path).map_err(|e| e.to_string())?;
        let log_err = log.try_clone().map_err(|e| e.to_string())?;
        let child = Command::new(java)
            .args(["-Xmx768M", "-Duser.language=en", "-jar"])
            .arg(jar)
            .arg("nogui")
            .current_dir(&dir)
            .stdin(Stdio::null())
            .stdout(Stdio::from(log))
            .stderr(Stdio::from(log_err))
            .spawn()
            .map_err(|e| format!("spawn {}: {e}", java.display()))?;
        let start = Instant::now();
        loop {
            if start.elapsed() > Duration::from_secs(240) {
                let _ = child_kill(child.id());
                let tail = log_tail(&log_path);
                return Err(format!("server did not finish starting\n{tail}"));
            }
            let text = fs::read_to_string(&log_path).unwrap_or_default();
            if text.contains("Done (") || text.contains("For help, type") {
                break;
            }
            if text.contains("You need to agree to the EULA") {
                let _ = child_kill(child.id());
                return Err("server refused the EULA".into());
            }
            if text.contains("UnsupportedClassVersionError") || text.contains("requires Java") {
                let _ = child_kill(child.id());
                return Err(format!("server jar refused this JDK\n{}", log_tail(&log_path)));
            }
            std::thread::sleep(Duration::from_millis(300));
        }
        Ok(Self { child, port })
    }

    fn stop(&mut self) {
        if self.child.try_wait().ok().flatten().is_none() {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.stop();
    }
}

fn child_kill(pid: u32) -> std::io::Result<()> {
    Command::new("kill").args(["-9", &pid.to_string()]).status().map(|_| ())
}

fn log_tail(path: &Path) -> String {
    let text = fs::read_to_string(path).unwrap_or_default();
    let lines: Vec<&str> = text.lines().collect();
    let start = lines.len().saturating_sub(25);
    lines[start..].join("\n")
}

fn free_port() -> Result<u16, String> {
    let listener = TcpListener::bind("127.0.0.1:0").map_err(|e| e.to_string())?;
    let port = listener.local_addr().map_err(|e| e.to_string())?.port();
    drop(listener);
    Ok(port)
}

fn status_session(resolved: &ResolvedSchema, protocol: u32, port: u16) -> Result<Value, String> {
    let mut sock = connect(port)?;
    send_packet(&mut sock, resolved, "handshaking", "serverbound", "set_protocol", &handshake(protocol, port, 1))?;
    send_packet(&mut sock, resolved, "status", "serverbound", "ping_start", &Env::new())?;
    let info = read_decoded(&mut sock, resolved, "status", "clientbound", None)?;
    send_packet(
        &mut sock,
        resolved,
        "status",
        "serverbound",
        "ping_status_serverbound",
        &one("time", Value::Int(1)),
    )?;
    let pong = read_decoded(&mut sock, resolved, "status", "clientbound", None)?;
    let mut out = OrderedMap::new();
    out.insert("server_info", info);
    out.insert("pong", pong);
    Ok(Value::map(out))
}

fn login_session(resolved: &ResolvedSchema, protocol: u32, port: u16) -> Result<Value, String> {
    let mut sock = connect(port)?;
    send_packet(&mut sock, resolved, "handshaking", "serverbound", "set_protocol", &handshake(protocol, port, 2))?;
    let start = login_start_value(resolved)?;
    send_packet(&mut sock, resolved, "login", "serverbound", "login_start", &start)?;
    let mut packets = Vec::new();
    let mut compression: Option<i128> = None;
    let mut saw_success = false;
    let deadline = Instant::now() + Duration::from_secs(20);
    while packets.len() < 24 && Instant::now() < deadline {
        match read_decoded(&mut sock, resolved, "login", "clientbound", compression) {
            Ok(v) => {
                let name = match v.get("packet") {
                    Some(Value::Str(s)) => s.to_string(),
                    _ => String::new(),
                };
                if name == "compress"
                    && let Some(Value::Map(fields)) = v.get("fields")
                    && let Some(Value::Int(n)) = fields.get("threshold")
                {
                    compression = Some(*n);
                }
                if name == "success" || name == "login_success" {
                    saw_success = true;
                }
                packets.push(v);
                if name == "disconnect" || name.starts_with("disconnect") || name == "encryption_begin_login_clientbound" {
                    break;
                }
                if saw_success {
                    break;
                }
            }
            Err(e) => {
                if packets.is_empty() {
                    return Err(e);
                }
                break;
            }
        }
    }
    let mut out = OrderedMap::new();
    out.insert("success", Value::Bool(saw_success));
    out.insert("packets", Value::list(packets));
    Ok(Value::map(out))
}

fn handshake(protocol: u32, port: u16, next_state: i128) -> Env {
    let mut env = Env::new();
    env.insert("protocol_version", Value::Int(i128::from(protocol)));
    env.insert("server_host", Value::str("127.0.0.1"));
    env.insert("server_port", Value::Int(i128::from(port)));
    env.insert("next_state", Value::Int(next_state));
    env
}

fn one(name: &str, value: Value) -> Env {
    let mut env = Env::new();
    env.insert(name, value);
    env
}

/// Fields `login_start` needs, with the optional groups left off.
fn login_start_value(resolved: &ResolvedSchema) -> Result<Env, String> {
    let pkt = resolved.packets.get("login_start").ok_or("schema has no login_start")?;
    let mut env = Env::new();
    fill_items(&pkt.items, &mut env);
    env.insert("username", Value::str("Probe"));
    Ok(env)
}

fn fill_items(items: &[BlockItem], env: &mut Env) {
    for item in items {
        match item {
            BlockItem::Field(field) => fill_field(field, env),
            BlockItem::Group { cond: None, items } => fill_items(items, env),
            BlockItem::Group { cond: Some(_), .. } => {}
            _ => {}
        }
    }
}

fn fill_field(field: &WireField, env: &mut Env) {
    if field.if_expr.is_some() {
        return;
    }
    let value = match &field.type_ref {
        FieldType::Ref(t) if t.name == "string" => Value::str(""),
        FieldType::Ref(t) if t.name == "boolean" => Value::Bool(false),
        FieldType::Ref(t) if t.name == "uuid" => {
            let mut uuid = OrderedMap::new();
            uuid.insert("msb", Value::Int(0));
            uuid.insert("lsb", Value::Int(0));
            Value::map(uuid)
        }
        _ => Value::Int(0),
    };
    env.insert(&field.name, value);
}

fn send_packet(
    sock: &mut TcpStream,
    resolved: &ResolvedSchema,
    state: &str,
    direction: &str,
    name: &str,
    value: &Env,
) -> Result<(), String> {
    let id = packet_id(resolved, state, direction, name)?;
    let payload = encode_packet(resolved, name, value).map_err(|e| format!("encode {name}: {e}"))?;
    let bytes = frame(id, &payload);
    sock.write_all(&bytes).map_err(|e| format!("write {name}: {e}"))?;
    Ok(())
}

fn packet_id(resolved: &ResolvedSchema, state: &str, direction: &str, name: &str) -> Result<i128, String> {
    resolved
        .routes
        .iter()
        .find(|(route, pkt)| route.state == state && route.direction == direction && pkt.name == name)
        .map(|(route, _)| route.packet_id)
        .ok_or_else(|| format!("no route {state}.{direction} {name}"))
}

fn frame(id: i128, payload: &[u8]) -> Vec<u8> {
    let mut body = write_varint(id);
    body.extend(payload);
    let mut out = write_varint(body.len() as i128);
    out.extend(body);
    out
}

fn connect(port: u16) -> Result<TcpStream, String> {
    let sock = TcpStream::connect(("127.0.0.1", port)).map_err(|e| format!("connect {port}: {e}"))?;
    sock.set_read_timeout(Some(Duration::from_secs(8))).map_err(|e| e.to_string())?;
    sock.set_write_timeout(Some(Duration::from_secs(8))).map_err(|e| e.to_string())?;
    Ok(sock)
}

fn read_decoded(
    sock: &mut TcpStream,
    resolved: &ResolvedSchema,
    state: &str,
    direction: &str,
    compression: Option<i128>,
) -> Result<Value, String> {
    let raw = read_frame(sock)?;
    let plain = if let Some(threshold) = compression {
        inflate_frame(&raw, threshold)?
    } else {
        raw
    };
    let mut framed = write_varint(plain.len() as i128);
    framed.extend(plain);
    let (id, decoded) = decode_frame_payload(resolved, state, direction, &framed).map_err(|e| e.to_string())?;
    let mut out = OrderedMap::new();
    out.insert("id", Value::Int(id));
    if let Some(packet) = decoded.get("packet") {
        out.insert("packet", packet.clone());
    }
    if let Some(fields) = decoded.get("fields") {
        out.insert("fields", fields.clone());
    }
    Ok(Value::map(out))
}

fn read_frame(sock: &mut TcpStream) -> Result<Vec<u8>, String> {
    let len = read_varint(sock)?;
    let n = usize::try_from(len).map_err(|_| "negative frame".to_string())?;
    if n > 2_000_000 {
        return Err(format!("frame length {n} is not a packet"));
    }
    let mut buf = vec![0u8; n];
    sock.read_exact(&mut buf).map_err(|e| format!("read frame: {e}"))?;
    Ok(buf)
}

/// A compressed frame is `data_length: varint` then either raw bytes (`data_length == 0`) or
/// a zlib blob of `data_length` bytes.
fn inflate_frame(frame: &[u8], _threshold: i128) -> Result<Vec<u8>, String> {
    let mut i = 0;
    let (data_len, next) = read_varint_buf(frame, i)?;
    i = next;
    if data_len == 0 {
        return Ok(frame[i..].to_vec());
    }
    let mut dec = ZlibDecoder::new(&frame[i..]);
    let mut out = Vec::new();
    dec.read_to_end(&mut out).map_err(|e| format!("inflate: {e}"))?;
    Ok(out)
}

fn read_varint(sock: &mut TcpStream) -> Result<i128, String> {
    let mut n = 0i128;
    let mut shift = 0;
    for _ in 0..5 {
        let mut b = [0u8; 1];
        sock.read_exact(&mut b).map_err(|e| format!("read varint: {e}"))?;
        n |= i128::from(b[0] & 0x7f) << shift;
        if b[0] & 0x80 == 0 {
            return Ok(n);
        }
        shift += 7;
    }
    Err("varint is too long".into())
}

fn read_varint_buf(buf: &[u8], mut i: usize) -> Result<(i128, usize), String> {
    let mut n = 0i128;
    let mut shift = 0;
    for _ in 0..5 {
        if i >= buf.len() {
            return Err("truncated varint".into());
        }
        let b = buf[i];
        i += 1;
        n |= i128::from(b & 0x7f) << shift;
        if b & 0x80 == 0 {
            return Ok((n, i));
        }
        shift += 7;
    }
    Err("varint is too long".into())
}

fn write_varint(mut n: i128) -> Vec<u8> {
    let mut out = Vec::new();
    loop {
        let mut b = (n & 0x7f) as u8;
        n >>= 7;
        if n != 0 {
            b |= 0x80;
            out.push(b);
        } else {
            out.push(b);
            break;
        }
    }
    out
}

/// Readable protocol class names in the client jar, and whether the server jar has the same set.
/// Obfuscated jars (1.21 and earlier) have no `net/minecraft/network/protocol/` entries; that is
/// reported, not treated as a decode failure.
fn client_jar_probe(client: &Path, server: &Path) -> Value {
    let mut out = OrderedMap::new();
    match (jar_class_names(client), jar_class_names(server)) {
        (Ok(c), Ok(s)) => {
            let cp = protocol_names(&c);
            let sp = protocol_names(&s);
            out.insert("client_protocol_classes", Value::Int(cp.len() as i128));
            out.insert("server_protocol_classes", Value::Int(sp.len() as i128));
            if cp.is_empty() && sp.is_empty() {
                out.insert("readable", Value::Bool(false));
            } else {
                out.insert("readable", Value::Bool(true));
                let only_client: Vec<Value> = cp.difference(&sp).take(12).map(|n| Value::str(*n)).collect();
                let only_server: Vec<Value> = sp.difference(&cp).take(12).map(|n| Value::str(*n)).collect();
                out.insert("only_in_client", Value::list(only_client));
                out.insert("only_in_server", Value::list(only_server));
            }
        }
        (Err(e), _) | (_, Err(e)) => {
            out.insert("error", Value::str(e));
        }
    }
    Value::map(out)
}

/// Class names in a jar. A modern server download is a bundler whose classes live in
/// `META-INF/versions/<ver>/server-*.jar`; open that when the outer jar has none.
fn jar_class_names(path: &Path) -> Result<Vec<String>, String> {
    let names = zip_names(path)?;
    if !protocol_names(&names).is_empty() {
        return Ok(names);
    }
    let Some(inner) = names.iter().find(|n| n.starts_with("META-INF/versions/") && n.ends_with(".jar")) else {
        return Ok(names);
    };
    let out = Command::new("unzip")
        .args(["-p"])
        .arg(path)
        .arg(inner)
        .output()
        .map_err(|e| format!("unzip: {e}"))?;
    if !out.status.success() {
        return Err(format!("unzip {} {inner}", path.display()));
    }
    let tmp = std::env::temp_dir().join(format!("bps-inner-{}-{}.jar", std::process::id(), path.file_name().and_then(|s| s.to_str()).unwrap_or("jar")));
    fs::write(&tmp, &out.stdout).map_err(|e| e.to_string())?;
    let inner_names = zip_names(&tmp);
    let _ = fs::remove_file(&tmp);
    inner_names
}

fn protocol_names(names: &[String]) -> std::collections::BTreeSet<&str> {
    names
        .iter()
        .filter(|n| n.starts_with("net/minecraft/network/protocol/") && n.ends_with(".class"))
        .map(|n| n.as_str())
        .collect()
}

fn zip_names(path: &Path) -> Result<Vec<String>, String> {
    let data = fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let eocd = data
        .windows(4)
        .rposition(|w| w == [0x50, 0x4b, 0x05, 0x06])
        .ok_or_else(|| format!("{}: not a zip", path.display()))?;
    if eocd + 22 > data.len() {
        return Err("truncated zip".into());
    }
    let cd_size = u32::from_le_bytes(data[eocd + 12..eocd + 16].try_into().unwrap()) as usize;
    let cd_off = u32::from_le_bytes(data[eocd + 16..eocd + 20].try_into().unwrap()) as usize;
    let cd = data.get(cd_off..cd_off + cd_size).ok_or("zip central directory is out of range")?;
    let mut names = Vec::new();
    let mut i = 0;
    while i + 46 <= cd.len() {
        if cd[i..i + 4] != [0x50, 0x4b, 0x01, 0x02] {
            break;
        }
        let name_len = u16::from_le_bytes(cd[i + 28..i + 30].try_into().unwrap()) as usize;
        let extra_len = u16::from_le_bytes(cd[i + 30..i + 32].try_into().unwrap()) as usize;
        let comment_len = u16::from_le_bytes(cd[i + 32..i + 34].try_into().unwrap()) as usize;
        let start = i + 46;
        let end = start + name_len;
        if end > cd.len() {
            break;
        }
        if let Ok(name) = std::str::from_utf8(&cd[start..end]) {
            names.push(name.to_string());
        }
        i = end + extra_len + comment_len;
    }
    Ok(names)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn java_era_matches_what_the_server_starts_on() {
        assert_eq!(java_major("1.13"), 8);
        assert_eq!(java_major("1.16.5"), 8);
        assert_eq!(java_major("1.17"), 17);
        assert_eq!(java_major("1.20.4"), 17);
        assert_eq!(java_major("1.20.5"), 21);
        assert_eq!(java_major("1.21.11"), 21);
        assert_eq!(java_major("26.1"), 25);
        assert_eq!(java_major("26.3"), 25);
    }

    #[test]
    fn varints_round_trip() {
        for n in [0, 1, 127, 128, 255, 25565, 2_000_000] {
            let bytes = write_varint(n);
            let (got, next) = read_varint_buf(&bytes, 0).unwrap();
            assert_eq!(got, n);
            assert_eq!(next, bytes.len());
        }
    }
}
