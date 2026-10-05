//! Loopback-only listener configuration for services that Homebrew and apt start with
//! their stock configs: Kafka, ZooKeeper and RabbitMQ otherwise listen on every interface.
//!
//! The rule is the one MySQL's `my.cnf.d/devy.cnf` follows: devy writes files it owns
//! (marked with [`DEVY_MARKER`] on the first line), and never silently rewrites a config
//! the user or the formula owns. Where a foreign file would need editing, devy warns once
//! per distinct file content instead, naming the lines to add. The one exception is a
//! Homebrew Kafka `server.properties` whose listeners are still the stock
//! `PLAINTEXT://:9092,CONTROLLER://:9093`: there the listeners line is rebound to
//! `127.0.0.1`, keeping the ports, because nothing else can point `brew services` at
//! another file.
//!
//! Every write goes through `fs_safe::write_atomic`; a write that fails (for example
//! `/etc` is not writable without root under apt) becomes a warning, never a failed `up`.

use std::path::{Path, PathBuf};

use crate::package_manager::PackageManager;

/// First line of every file devy owns here.
pub(super) const DEVY_MARKER: &str = "# devy-managed";

const LOOPBACK_HOSTS: &[&str] = &["127.0.0.1", "localhost", "[::1]", "::1"];

/// The stock Kafka 4 listeners (Homebrew's `etc/kafka/server.properties` and upstream's
/// `config/server.properties`): an empty host binds every interface.
const KAFKA_STOCK_LISTENERS: &str = "PLAINTEXT://:9092,CONTROLLER://:9093";
const KAFKA_LOOPBACK_LISTENERS: &str = "PLAINTEXT://127.0.0.1:9092,CONTROLLER://127.0.0.1:9093";

/// Where the warn-once fingerprints live: devy's private per-user directory. `None`
/// (and nothing created) on backends this module doesn't cover.
pub(super) fn default_state_dir(pm: &dyn PackageManager) -> Option<PathBuf> {
    matches!(pm.name(), "brew" | "apt")
        .then(crate::fs_safe::user_dir)
        .and_then(Result::ok)
}

/// The global `etc` directory the service's package reads its config from, for the
/// backends whose stock configs bind every interface. `None` elsewhere (nix launches
/// with devy's own loopback config, docker publishes on 127.0.0.1 only, winget is not
/// covered).
fn etc_dir(pm: &dyn PackageManager, service: &str) -> Option<PathBuf> {
    match pm.name() {
        "brew" | "apt" => pm.service_config_dir(service),
        _ => None,
    }
}

/// Shows `message` unless the same `key` was already warned about with the same
/// `fingerprint` (recorded in `state_dir`). Without a state dir it always warns.
fn warn_once(state_dir: Option<&Path>, key: &str, fingerprint: &str, message: &str) {
    let fingerprint = format!(
        "{:016x}",
        super::helpers::fnv1a(&format!("{fingerprint}\n{message}"))
    );
    let stamp = state_dir.map(|d| d.join(format!("loopback-warned-{key}")));
    if let Some(stamp) = &stamp
        && std::fs::symlink_metadata(stamp).is_ok_and(|m| m.is_file())
        && read_small(stamp).as_deref() == Some(fingerprint.as_str())
    {
        return;
    }
    crate::output::warn(message);
    if let Some(stamp) = stamp {
        let _ = crate::fs_safe::write_atomic(&stamp, fingerprint.as_bytes(), 0o600);
    }
}

/// The first few bytes of `path` as text, so a huge planted file can't exhaust memory.
fn read_small(path: &Path) -> Option<String> {
    use std::io::Read;
    let mut text = String::new();
    std::fs::File::open(path)
        .ok()?
        .take(64)
        .read_to_string(&mut text)
        .ok()?;
    Some(text)
}

/// The permission bits of an existing regular file at `path` (so a rewrite never widens a
/// `0600` file that holds secrets), else `0o644`.
fn mode_for(path: &Path) -> u32 {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Ok(meta) = std::fs::symlink_metadata(path)
            && meta.is_file()
        {
            return meta.permissions().mode() & 0o7777;
        }
    }
    let _ = path;
    0o644
}

/// Writes `content` to `path` when it differs, creating the parent directory and keeping
/// an existing file's mode. Returns whether the file changed. A failure is a warning
/// (shown once per `hint`) naming only `hint` — the lines to add — never the whole file,
/// which may be the user's own config with credentials in it.
fn write_owned(
    path: &Path,
    content: &str,
    hint: &str,
    what: &str,
    state_dir: Option<&Path>,
) -> bool {
    if std::fs::read_to_string(path).is_ok_and(|old| old == content) {
        return false;
    }
    let mode = mode_for(path);
    let result = path
        .parent()
        .map_or(Ok(()), |dir| {
            std::fs::create_dir_all(dir).map_err(anyhow::Error::from)
        })
        .and_then(|()| crate::fs_safe::write_atomic(path, content.as_bytes(), mode));
    match result {
        Ok(()) => {
            crate::output::info(&format!(
                "{what}: wrote {} — restart the service if it is already running",
                path.display()
            ));
            true
        }
        Err(e) => {
            warn_once(
                state_dir,
                &format!(
                    "{what}-write-{}",
                    path.file_name().unwrap_or_default().to_string_lossy()
                ),
                &path.to_string_lossy(),
                &format!(
                    "{what}: could not write {} ({e:#}), so the service may listen on every interface — add:\n{hint}",
                    path.display()
                ),
            );
            false
        }
    }
}

/// Whether the file at `path` is one devy wrote (its first line is the marker).
fn devy_owned(text: &str) -> bool {
    text.lines()
        .next()
        .is_some_and(|l| l.starts_with(DEVY_MARKER))
}

/// `key=value` pairs of a `.properties` / `zoo.cfg` style file (comments and blank lines
/// skipped, whitespace around `=` trimmed). Later duplicates win, as in Java.
fn properties(text: &str) -> Vec<(&str, &str)> {
    text.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#') && !l.starts_with('!'))
        .filter_map(|l| l.split_once('='))
        .map(|(k, v)| (k.trim(), v.trim()))
        .collect()
}

/// Whether every setting line (not blank, not a `#`/`!` comment) is `key=value` as Java
/// reads it, so [`properties`] sees the same keys. Java ends a key at the first `=`, `:`
/// or whitespace, then takes one `=` or `:` after any whitespace as the separator: so
/// `key: value`, `key value` and `key value=x` (key `key`) can't be judged, while
/// `key = value` can. A line ending in an odd number of backslashes continues on the
/// next line in Java, which [`properties`] does not follow, so it can't be judged either
/// (comments included, to stay on the safe side), nor can a key with a `\` in it: Java
/// unescapes it (`listen\ers=` sets `listeners`).
fn only_key_eq_value(text: &str) -> bool {
    text.lines().all(|raw| {
        let trailing = raw
            .trim_end_matches('\r')
            .bytes()
            .rev()
            .take_while(|b| *b == b'\\')
            .count();
        if trailing % 2 == 1 {
            return false;
        }
        let l = raw.trim();
        if l.is_empty() || l.starts_with('#') || l.starts_with('!') {
            return true;
        }
        // Java reads `\` in a key as an escape (`listen\ers` is `listeners`, and `\=`,
        // `\:` or `\ ` keep a terminator in the key), which [`properties`] does not.
        match l.find(|c: char| c == '=' || c == ':' || c == '\\' || c.is_whitespace()) {
            Some(i) if l[i..].starts_with('=') => true,
            Some(i) if l[i..].starts_with(['\\', ':']) => false,
            Some(i) => l[i..].trim_start().starts_with('='),
            None => false,
        }
    })
}

fn property<'a>(props: &[(&'a str, &'a str)], key: &str) -> Option<&'a str> {
    props.iter().rev().find(|(k, _)| *k == key).map(|(_, v)| *v)
}

// ── Kafka ────────────────────────────────────────────────────────────────────

/// Whether every listener in a Kafka `listeners` value binds a loopback host. A value
/// with a `\` is not judged: Java unescapes it, so `…:9092,PLAINTEXT://:9093`
/// is a second listener on every interface.
fn kafka_listeners_loopback(value: &str) -> bool {
    !value.contains('\\')
        && value.split(',').map(str::trim).all(|listener| {
            listener
                .split_once("://")
                .and_then(|(_, addr)| addr.rsplit_once(':'))
                .is_some_and(|(host, _)| LOOPBACK_HOSTS.contains(&host))
        })
}

/// What to do with a Kafka `server.properties`.
#[derive(Debug, PartialEq, Eq)]
enum KafkaPlan {
    /// Every listener already binds loopback.
    Ok,
    /// Stock listeners: rewrite the file with this content.
    Rewrite(String),
    /// Anything else: warn.
    Warn,
}

fn kafka_plan(text: &str) -> KafkaPlan {
    // A `listeners: …` or `listeners …` line would override what devy reads.
    if !only_key_eq_value(text) {
        return KafkaPlan::Warn;
    }
    let props = properties(text);
    match property(&props, "listeners") {
        Some(v) if kafka_listeners_loopback(v) => KafkaPlan::Ok,
        Some(v) if v == KAFKA_STOCK_LISTENERS => {
            let advertised_ok =
                property(&props, "advertised.listeners").is_none_or(kafka_listeners_loopback);
            if !advertised_ok {
                return KafkaPlan::Warn;
            }
            let mut out = String::with_capacity(text.len() + 96);
            for line in text.split_inclusive('\n') {
                let bare = line.trim();
                let is_listeners = bare.split_once('=').is_some_and(|(k, v)| {
                    k.trim() == "listeners" && v.trim() == KAFKA_STOCK_LISTENERS
                });
                if is_listeners && !bare.starts_with('#') {
                    out.push_str(&format!(
                        "# devy: bound to 127.0.0.1 (was listeners={KAFKA_STOCK_LISTENERS})\nlisteners={KAFKA_LOOPBACK_LISTENERS}\n"
                    ));
                } else {
                    out.push_str(line);
                }
            }
            KafkaPlan::Rewrite(out)
        }
        _ => KafkaPlan::Warn,
    }
}

/// Kafka's `server.properties` under the package manager's `etc`.
fn kafka_config_candidates(etc: &Path) -> [PathBuf; 2] {
    [
        etc.join("kafka").join("server.properties"),
        etc.join("kafka").join("kraft").join("server.properties"),
    ]
}

/// Makes a Homebrew/apt Kafka listen on loopback only (see the module docs).
pub(super) fn secure_kafka(pm: &dyn PackageManager, state_dir: Option<&Path>) {
    let Some(etc) = etc_dir(pm, "kafka") else {
        return;
    };
    let Some(conf) = kafka_config_candidates(&etc)
        .into_iter()
        .find(|p| p.is_file())
    else {
        warn_once(
            state_dir,
            "kafka-missing",
            &etc.to_string_lossy(),
            &format!(
                "kafka: no server.properties found under {} — make sure its listeners bind 127.0.0.1 (listeners={KAFKA_LOOPBACK_LISTENERS})",
                etc.join("kafka").display()
            ),
        );
        return;
    };
    let Ok(text) = std::fs::read_to_string(&conf) else {
        warn_once(
            state_dir,
            "kafka-unreadable",
            &conf.to_string_lossy(),
            &format!(
                "kafka: could not read {} — make sure its listeners bind 127.0.0.1 (listeners={KAFKA_LOOPBACK_LISTENERS})",
                conf.display()
            ),
        );
        return;
    };
    match kafka_plan(&text) {
        KafkaPlan::Ok => {}
        KafkaPlan::Rewrite(new) if pm.name() == "brew" => {
            let hint = format!("listeners={KAFKA_LOOPBACK_LISTENERS}");
            write_owned(&conf, &new, &hint, "kafka", state_dir);
        }
        KafkaPlan::Rewrite(_) | KafkaPlan::Warn => warn_once(
            state_dir,
            "kafka",
            &text,
            &format!(
                "kafka: {} lets Kafka listen on every interface, and devy does not edit a config it doesn't own — set listeners=PLAINTEXT://127.0.0.1:<port>,CONTROLLER://127.0.0.1:<controller port> (and keep advertised.listeners on localhost)",
                conf.display()
            ),
        ),
    }
}

// ── ZooKeeper ────────────────────────────────────────────────────────────────

/// Settings a loopback-only ZooKeeper needs in `zoo.cfg`.
const ZOOKEEPER_LINES: &str = "clientPortAddress=127.0.0.1\nadmin.enableServer=false\n";

/// Keys of the stock `zoo_sample.cfg` (which Homebrew and Debian install as `zoo.cfg`).
const ZOOKEEPER_STOCK_KEYS: &[&str] = &[
    "tickTime",
    "initLimit",
    "syncLimit",
    "dataDir",
    "clientPort",
];

/// Whether `zoo.cfg` binds every listener to loopback: the client port, the TLS client
/// port when `secureClientPort` is set, and the Prometheus metrics endpoint when
/// `metricsProvider.httpPort` is set; and the AdminServer is off.
fn zookeeper_ok(props: &[(&str, &str)]) -> bool {
    let loopback = |key: &str| property(props, key).is_some_and(|h| LOOPBACK_HOSTS.contains(&h));
    let bound_if_set = |port: &str, host: &str| property(props, port).is_none() || loopback(host);
    // The Prometheus provider serves on port 7000 by default once `className` names it,
    // even without `httpPort`.
    let metrics = property(props, "metricsProvider.className").is_some()
        || property(props, "metricsProvider.httpPort").is_some();
    loopback("clientPortAddress")
        && property(props, "admin.enableServer") == Some("false")
        && bound_if_set("secureClientPort", "secureClientPortAddress")
        && (!metrics || loopback("metricsProvider.httpHost"))
}

fn zookeeper_cfg(etc: &Path, pm_name: &str) -> PathBuf {
    match pm_name {
        "apt" => etc.join("zookeeper").join("conf").join("zoo.cfg"),
        _ => etc.join("zookeeper").join("zoo.cfg"),
    }
}

/// Makes a Homebrew/apt ZooKeeper (started for Kafka without KRaft) listen on loopback
/// only and turns off its AdminServer (HTTP on 0.0.0.0:8080 by default). A `zoo.cfg`
/// that still holds only the stock sample's keys gets devy's lines appended; any other
/// gets a warning.
pub(super) fn secure_zookeeper(pm: &dyn PackageManager, state_dir: Option<&Path>) {
    let Some(etc) = etc_dir(pm, "zookeeper") else {
        return;
    };
    let cfg = zookeeper_cfg(&etc, pm.name());
    let Ok(text) = std::fs::read_to_string(&cfg) else {
        warn_once(
            state_dir,
            "zookeeper-missing",
            &cfg.to_string_lossy(),
            &format!(
                "zookeeper: could not read {} — make sure ZooKeeper listens on 127.0.0.1 only:\n{ZOOKEEPER_LINES}",
                cfg.display()
            ),
        );
        return;
    };
    let props = properties(&text);
    // A `key: value` or `key value` line (which Java also reads) can override what devy
    // reads, and means the file is not the stock sample.
    let parsed = only_key_eq_value(&text);
    if parsed && zookeeper_ok(&props) {
        return;
    }
    let stock = parsed && props.iter().all(|(k, _)| ZOOKEEPER_STOCK_KEYS.contains(k));
    if stock && pm.name() == "brew" {
        let mut new = text.clone();
        if !new.is_empty() && !new.ends_with('\n') {
            new.push('\n');
        }
        new.push_str("# devy: loopback only, no AdminServer\n");
        new.push_str(ZOOKEEPER_LINES);
        write_owned(&cfg, &new, ZOOKEEPER_LINES, "zookeeper", state_dir);
        return;
    }
    warn_once(
        state_dir,
        "zookeeper",
        &text,
        &format!(
            "zookeeper: {} lets ZooKeeper listen on every interface, and devy does not edit a config it doesn't own — add:\n{ZOOKEEPER_LINES}",
            cfg.display()
        ),
    );
}

// ── RabbitMQ ─────────────────────────────────────────────────────────────────

/// The node name devy's env file sets. Erlang distribution can only be bound to
/// loopback when the node's host part resolves there: `rabbitmqctl` reaches a
/// `rabbit@<hostname>` node at the hostname's address (Debian maps it to 127.0.1.1).
const RABBITMQ_NODENAME: &str = "rabbit@localhost";

/// `rabbitmq-env.conf` written when there is none (or it is devy's own). epmd's address
/// can only be set through the environment, so it is exported for epmd itself. There is
/// no `NODE_IP_ADDRESS`: it would override the conf.d listener and its port.
fn rabbitmq_env_file() -> String {
    format!(
        "{DEVY_MARKER} — rewritten by devy up\n\
         NODENAME=\"{RABBITMQ_NODENAME}\"\n\
         export ERL_EPMD_ADDRESS=\"127.0.0.1\"\n"
    )
}

/// `KEY=value` pairs of a `rabbitmq-env.conf` (keys without `export` and the `RABBITMQ_`
/// prefix; values unquoted, with a trailing ` # comment` dropped). Later duplicates win.
fn rabbitmq_env_vars(text: &str) -> Vec<(String, String)> {
    text.lines()
        .map(str::trim)
        .filter(|l| !l.starts_with('#'))
        .filter_map(|l| l.strip_prefix("export ").unwrap_or(l).split_once('='))
        .map(|(k, v)| {
            let k = k.trim();
            let k = k.strip_prefix("RABBITMQ_").unwrap_or(k);
            (k.to_string(), shell_value(v.trim()).to_string())
        })
        .collect()
}

/// A shell assignment's value: the text inside matching leading quotes, else the text
/// before an unquoted ` #` comment.
fn shell_value(v: &str) -> &str {
    for q in ['"', '\''] {
        if let Some(rest) = v.strip_prefix(q) {
            return rest.split(q).next().unwrap_or(rest);
        }
    }
    v.split(" #").next().unwrap_or(v).trim()
}

fn env_var<'a>(vars: &'a [(String, String)], key: &str) -> Option<&'a str> {
    vars.iter()
        .rev()
        .find(|(k, _)| k == key)
        .map(|(_, v)| v.as_str())
}

/// Whether the node is named `<name>@localhost` (see `RABBITMQ_NODENAME`).
fn rabbitmq_node_local(vars: &[(String, String)]) -> bool {
    env_var(vars, "NODENAME").is_some_and(|n| n.ends_with("@localhost"))
}

/// The settings a foreign `rabbitmq-env.conf` lacks, as lines to add. epmd must be
/// pinned to loopback. AMQP is bound by devy's conf.d — unless the file sets
/// `NODE_IP_ADDRESS` or `NODE_PORT`, either of which RabbitMQ turns into a listener (on
/// `NODE_IP_ADDRESS`, default every interface, and `NODE_PORT`, default 5672) that
/// overrides the config file; then the address must be loopback and the port the
/// project's.
fn rabbitmq_env_missing(text: &str, port: u16) -> Vec<String> {
    let vars = rabbitmq_env_vars(text);
    let mut missing = Vec::new();
    let set = |key: &str| env_var(&vars, key).filter(|v| !v.is_empty());
    let ip = set("NODE_IP_ADDRESS");
    let node_port = set("NODE_PORT");
    if ip.is_some() || node_port.is_some() {
        if !ip.is_some_and(|ip| LOOPBACK_HOSTS.contains(&ip)) {
            missing.push("NODE_IP_ADDRESS=\"127.0.0.1\"".to_string());
        }
        if node_port.unwrap_or("5672") != port.to_string() {
            missing.push(format!("NODE_PORT=\"{port}\""));
        }
    }
    if !epmd_address_exported(text) {
        missing.push("export ERL_EPMD_ADDRESS=\"127.0.0.1\"".to_string());
    }
    missing
}

/// Whether `rabbitmq-env.conf` leaves `ERL_EPMD_ADDRESS` at a loopback address and
/// exported. The file is sourced without `set -a`, so a plain assignment stays a shell
/// variable epmd never sees: it needs `export` on the line, a later `export
/// ERL_EPMD_ADDRESS`, or a `set -a` before it. Read top to bottom as the shell would.
fn epmd_address_exported(text: &str) -> bool {
    const KEY: &str = "ERL_EPMD_ADDRESS";
    let mut value: Option<String> = None;
    let mut exported = false;
    let mut allexport = false;
    for line in text.lines().map(str::trim) {
        if line.starts_with('#') {
            continue;
        }
        match line {
            "set -a" | "set -o allexport" => allexport = true,
            "set +a" | "set +o allexport" => allexport = false,
            _ => {}
        }
        let (export, rest) = match line.strip_prefix("export ") {
            Some(rest) => (true, rest.trim()),
            None => (false, line),
        };
        match rest.split_once('=') {
            Some((k, v)) if k.trim() == KEY => {
                value = Some(shell_value(v.trim()).to_string());
                exported |= export || allexport;
            }
            Some(_) => {}
            None if export => {
                let names = rest.split(" #").next().unwrap_or(rest);
                exported |= names.split_whitespace().any(|n| n == KEY);
            }
            None => {}
        }
    }
    exported && value.is_some_and(|v| v == "127.0.0.1")
}

/// Plugin names in an `enabled_plugins` file (`[rabbitmq_management,rabbitmq_stomp].`),
/// plus the listener plugins RabbitMQ starts as their dependencies.
fn enabled_plugins(text: &str) -> Vec<String> {
    let body = text.trim().trim_end_matches('.').trim();
    let body = body
        .strip_prefix('[')
        .and_then(|b| b.strip_suffix(']'))
        .unwrap_or("");
    let mut plugins: Vec<String> = body
        .split(',')
        .map(|p| p.trim().to_string())
        .filter(|p| !p.is_empty())
        .collect();
    let mut implied = Vec::new();
    for p in &plugins {
        if p.ends_with("_management") {
            implied.push("rabbitmq_management");
        }
        match p.as_str() {
            "rabbitmq_web_stomp" => implied.push("rabbitmq_stomp"),
            "rabbitmq_web_stomp_examples" => {
                implied.extend(["rabbitmq_web_stomp", "rabbitmq_stomp"]);
            }
            "rabbitmq_web_mqtt_examples" => {
                implied.extend(["rabbitmq_web_mqtt", "rabbitmq_mqtt"]);
            }
            "rabbitmq_top" | "rabbitmq_tracing" => implied.push("rabbitmq_management"),
            "rabbitmq_web_mqtt" => implied.push("rabbitmq_mqtt"),
            "rabbitmq_stream_management" => implied.push("rabbitmq_stream"),
            _ => {}
        }
    }
    for p in implied {
        if !plugins.iter().any(|q| q == p) {
            plugins.push(p.to_string());
        }
    }
    plugins
}

/// Listener settings for enabled plugins: `(plugin, key prefix, devy's lines)`. Only
/// enabled plugins get lines (RabbitMQ refuses to boot on a setting whose plugin isn't
/// enabled), and a prefix the user's own config already sets is left to them.
const RABBITMQ_PLUGIN_LISTENERS: &[(&str, &str, &str)] = &[
    (
        "rabbitmq_management",
        "management.tcp.ip",
        "management.tcp.ip = 127.0.0.1",
    ),
    (
        "rabbitmq_prometheus",
        "prometheus.tcp.ip",
        "prometheus.tcp.ip = 127.0.0.1",
    ),
    (
        "rabbitmq_stomp",
        "stomp.listeners.tcp",
        "stomp.listeners.tcp.1 = 127.0.0.1:61613",
    ),
    (
        "rabbitmq_mqtt",
        "mqtt.listeners.tcp",
        "mqtt.listeners.tcp.default = 127.0.0.1:1883",
    ),
    (
        "rabbitmq_stream",
        "stream.listeners.tcp",
        "stream.listeners.tcp.default = 127.0.0.1:5552",
    ),
    // Not a listener, but what stream clients are told to connect to; a user's own
    // setting is theirs.
    (
        "rabbitmq_stream",
        "stream.advertised_host",
        "stream.advertised_host = localhost",
    ),
    (
        "rabbitmq_web_stomp",
        "web_stomp.tcp.ip",
        "web_stomp.tcp.ip = 127.0.0.1",
    ),
    (
        "rabbitmq_web_mqtt",
        "web_mqtt.tcp.ip",
        "web_mqtt.tcp.ip = 127.0.0.1",
    ),
];

/// Lines of the user's own RabbitMQ config that set a key starting with `prefix`.
fn user_settings<'a>(user_conf: &'a str, prefix: &str) -> Vec<&'a str> {
    user_conf
        .lines()
        .map(str::trim)
        .filter(|l| !l.starts_with('#'))
        .filter(|l| {
            l.split_once('=')
                .is_some_and(|(k, _)| k.trim().starts_with(prefix))
        })
        .collect()
}

/// Whether a `key = value` line's value is a loopback host or `host:port`.
fn loopback_value(line: &str) -> bool {
    line.split_once('=').is_some_and(|(_, v)| {
        let v = v.trim();
        // `listeners.tcp = none` turns the listener off.
        if v == "none" {
            return true;
        }
        LOOPBACK_HOSTS
            .iter()
            .any(|h| v == *h || v.starts_with(&format!("{h}:")))
    })
}

/// devy's `conf.d/90-devy.conf`, and the user's own listener lines it left alone that
/// don't bind loopback. AMQP goes on `127.0.0.1:<port>`, distribution on loopback when
/// the node is `@localhost`, and each enabled plugin's listener on loopback — except a
/// listener the user's own config already sets, which devy never overrides.
fn rabbitmq_conf_d(
    plugins: &[String],
    port: u16,
    node_local: bool,
    user_conf: &str,
) -> (String, Vec<String>) {
    let mut out = format!(
        "{DEVY_MARKER} — rewritten by devy up; re-run it after enabling or disabling plugins\n"
    );
    let mut foreign = Vec::new();
    let mut add = |prefix: &str, lines: &str| {
        let theirs = user_settings(user_conf, prefix);
        if theirs.is_empty() {
            out.push_str(lines);
            out.push('\n');
        } else if !prefix.ends_with("advertised_host") {
            foreign.extend(
                theirs
                    .into_iter()
                    .filter(|l| !loopback_value(l))
                    .map(String::from),
            );
        }
    };
    add(
        "listeners.tcp",
        &format!("listeners.tcp.default = 127.0.0.1:{port}"),
    );
    if node_local {
        add(
            "distribution.listener.interface",
            "distribution.listener.interface = 127.0.0.1",
        );
    }
    for (plugin, prefix, lines) in RABBITMQ_PLUGIN_LISTENERS {
        if plugins.iter().any(|p| p == plugin) {
            add(prefix, lines);
        }
    }
    foreign.extend(rabbitmq_tls_listeners(user_conf));
    (out, foreign)
}

/// TLS listeners devy never configures, so the user's own lines that set them must bind
/// loopback themselves: `listeners.ssl.*` (and the plugins' `*.listeners.ssl.*`) give
/// an address or port each, and a `*.ssl.port` (`management.ssl.port`,
/// `web_stomp.ssl.port`, …) listens on every interface unless its `*.ssl.ip` is
/// loopback. Returns the lines that don't bind loopback.
fn rabbitmq_tls_listeners(user_conf: &str) -> Vec<String> {
    let mut foreign = Vec::new();
    for prefix in [
        "listeners.ssl",
        "stomp.listeners.ssl",
        "mqtt.listeners.ssl",
        "stream.listeners.ssl",
    ] {
        foreign.extend(
            user_settings(user_conf, prefix)
                .into_iter()
                .filter(|l| !loopback_value(l))
                .map(String::from),
        );
    }
    let key = |l: &str| l.split_once('=').map(|(k, _)| k.trim().to_string());
    let mut seen = Vec::new();
    for line in user_settings(user_conf, "") {
        let Some(k) = key(line) else { continue };
        let Some(base) = k.strip_suffix(".ssl.port") else {
            continue;
        };
        let web = base.starts_with("web_");
        if !(web || matches!(base, "management" | "prometheus")) || seen.contains(&k) {
            continue;
        }
        seen.push(k.clone());
        let ip_key = format!("{base}.ssl.ip");
        let ip_ok = user_settings(user_conf, &ip_key)
            .into_iter()
            .rfind(|l| key(l).as_deref() == Some(ip_key.as_str()))
            .is_some_and(loopback_value);
        if !ip_ok {
            foreign.push(line.to_string());
        }
    }
    foreign
}

/// The user's own RabbitMQ config text: `rabbitmq.conf` and every `conf.d/*.conf` but
/// devy's, each read capped so a huge file can't exhaust memory.
fn rabbitmq_user_conf(dir: &Path, ours: &Path) -> String {
    use std::io::Read;
    let mut files = vec![dir.join("rabbitmq.conf")];
    if let Ok(entries) = std::fs::read_dir(dir.join("conf.d")) {
        let mut more: Vec<PathBuf> = entries
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.extension().is_some_and(|x| x == "conf") && p != ours)
            .collect();
        more.sort();
        files.extend(more);
    }
    let mut text = String::new();
    for f in files {
        if let Ok(file) = std::fs::File::open(&f) {
            let _ = file.take(1 << 20).read_to_string(&mut text);
            text.push('\n');
        }
    }
    text
}

/// Whether the path `value` names `expected`: the same after trailing separators are
/// dropped, or the same file when both exist.
fn same_path(value: &str, expected: &Path) -> bool {
    let trimmed = value.trim_end_matches(['/', '\\']);
    let value = Path::new(if trimmed.is_empty() { value } else { trimmed });
    if value == expected {
        return true;
    }
    match (
        std::fs::canonicalize(value),
        std::fs::canonicalize(expected),
    ) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    }
}

/// Warns (once per setting) when a foreign `rabbitmq-env.conf` points RabbitMQ at a
/// config or plugins file other than the ones in `dir` that devy inspects.
fn warn_custom_rabbitmq_paths(
    vars: &[(String, String)],
    dir: &Path,
    env: &Path,
    state_dir: Option<&Path>,
) {
    let default_config =
        |v: &str| same_path(v, &dir.join("rabbitmq")) || same_path(v, &dir.join("rabbitmq.conf"));
    let custom: Vec<String> = ["CONFIG_FILE", "CONFIG_FILES", "ENABLED_PLUGINS_FILE"]
        .into_iter()
        .filter_map(|k| {
            let v = env_var(vars, k).filter(|v| !v.is_empty())?;
            let default = match k {
                "ENABLED_PLUGINS_FILE" => same_path(v, &dir.join("enabled_plugins")),
                "CONFIG_FILE" => default_config(v),
                // RabbitMQ's default `conf.d`, which devy reads.
                _ => same_path(v, &dir.join("conf.d")),
            };
            (!default).then(|| format!("{k}={v}"))
        })
        .collect();
    if custom.is_empty() {
        return;
    }
    warn_once(
        state_dir,
        "rabbitmq-custom-paths",
        &custom.join("\n"),
        &format!(
            "rabbitmq: {} points RabbitMQ at files devy does not inspect ({}) — devy only reads rabbitmq.conf, conf.d/*.conf and enabled_plugins in {}, so make sure the config you use binds its listeners to 127.0.0.1",
            env.display(),
            custom.join(", "),
            dir.display()
        ),
    );
}

/// Makes a Homebrew/apt RabbitMQ listen on loopback only: devy owns
/// `conf.d/90-devy.conf` (read automatically by RabbitMQ 3.9+), and owns
/// `rabbitmq-env.conf` only when there is none; an existing foreign one that lacks the
/// loopback settings gets a warning naming them.
pub(super) fn secure_rabbitmq(pm: &dyn PackageManager, port: u16, state_dir: Option<&Path>) {
    let Some(etc) = etc_dir(pm, "rabbitmq") else {
        return;
    };
    let dir = etc.join("rabbitmq");
    let plugins = std::fs::read_to_string(dir.join("enabled_plugins"))
        .map(|t| enabled_plugins(&t))
        .unwrap_or_default();

    let env = dir.join("rabbitmq-env.conf");
    let devy_env = rabbitmq_env_file();
    let node_local = match std::fs::read_to_string(&env) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            let written = write_owned(&env, &devy_env, &devy_env, "rabbitmq", state_dir);
            if written {
                crate::output::info(&format!(
                    "rabbitmq: the node is now named {RABBITMQ_NODENAME}; its data directory follows the node name, so data from a node with another name stays in place but is no longer used. epmd binds 127.0.0.1 only if RabbitMQ starts it (an epmd another Erlang tool already started keeps its address)"
                ));
            }
            written
        }
        Err(_) => false,
        Ok(text) if devy_owned(&text) => {
            write_owned(&env, &devy_env, &devy_env, "rabbitmq", state_dir);
            std::fs::read_to_string(&env).is_ok_and(|t| rabbitmq_node_local(&rabbitmq_env_vars(&t)))
        }
        Ok(text) => {
            let vars = rabbitmq_env_vars(&text);
            warn_custom_rabbitmq_paths(&vars, &dir, &env, state_dir);
            let node_local = rabbitmq_node_local(&vars);
            let mut missing = rabbitmq_env_missing(&text, port);
            if !node_local {
                missing.push(format!("NODENAME=\"{RABBITMQ_NODENAME}\""));
            }
            if !missing.is_empty() {
                warn_once(
                    state_dir,
                    "rabbitmq-env",
                    &text,
                    &format!(
                        "rabbitmq: {} leaves a listener on every interface or off the project's port, and devy does not edit a config it doesn't own — add:\n{}{}",
                        env.display(),
                        missing.join("\n"),
                        if node_local {
                            ""
                        } else {
                            "\n(NODENAME lets devy bind Erlang distribution to loopback; the node's data directory follows its name)"
                        }
                    ),
                );
            }
            node_local
        }
    };

    let conf_d = dir.join("conf.d").join("90-devy.conf");
    if std::fs::read_to_string(&conf_d).is_ok_and(|t| !devy_owned(&t)) {
        warn_once(
            state_dir,
            "rabbitmq-conf-d",
            &conf_d.to_string_lossy(),
            &format!(
                "rabbitmq: {} exists and was not written by devy, so devy left it alone — make sure it binds listeners to 127.0.0.1",
                conf_d.display()
            ),
        );
        return;
    }
    let user_conf = rabbitmq_user_conf(&dir, &conf_d);
    let (conf, foreign) = rabbitmq_conf_d(&plugins, port, node_local, &user_conf);
    write_owned(&conf_d, &conf, &conf, "rabbitmq", state_dir);
    if !foreign.is_empty() {
        warn_once(
            state_dir,
            "rabbitmq-listeners",
            &foreign.join("\n"),
            &format!(
                "rabbitmq: your own config sets listeners that devy leaves alone, and they don't bind 127.0.0.1:\n{}",
                foreign.join("\n")
            ),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::package_manager::MockPackageManager;

    fn pm(name: &'static str, etc: &Path) -> MockPackageManager {
        MockPackageManager {
            name,
            config_dir: Some(etc.to_path_buf()),
            ..Default::default()
        }
    }

    const BREW_KAFKA: &str = "# Licensed to the Apache Software Foundation (ASF) under one or more\n\
        process.roles=broker,controller\n\
        node.id=1\n\
        controller.quorum.bootstrap.servers=localhost:9093\n\
        listeners=PLAINTEXT://:9092,CONTROLLER://:9093\n\
        advertised.listeners=PLAINTEXT://localhost:9092,CONTROLLER://localhost:9093\n\
        log.dirs=/opt/homebrew/var/lib/kraft-combined-logs\n";

    #[test]
    fn kafka_listener_hosts() {
        assert!(kafka_listeners_loopback(KAFKA_LOOPBACK_LISTENERS));
        assert!(kafka_listeners_loopback("PLAINTEXT://localhost:9092"));
        assert!(kafka_listeners_loopback("PLAINTEXT://[::1]:9092"));
        assert!(!kafka_listeners_loopback(KAFKA_STOCK_LISTENERS));
        assert!(!kafka_listeners_loopback("PLAINTEXT://0.0.0.0:9092"));
        assert!(!kafka_listeners_loopback(
            "PLAINTEXT://127.0.0.1:9092,EXTERNAL://:9094"
        ));
    }

    #[test]
    fn stock_brew_kafka_listeners_are_rebound_and_the_rest_is_kept() {
        let KafkaPlan::Rewrite(new) = kafka_plan(BREW_KAFKA) else {
            panic!("stock listeners must be rewritten");
        };
        assert!(new.contains(&format!("\nlisteners={KAFKA_LOOPBACK_LISTENERS}\n")));
        assert!(!new.contains("\nlisteners=PLAINTEXT://:9092"));
        for kept in [
            "process.roles=broker,controller",
            "advertised.listeners=PLAINTEXT://localhost:9092,CONTROLLER://localhost:9093",
            "log.dirs=/opt/homebrew/var/lib/kraft-combined-logs",
        ] {
            assert!(new.contains(kept), "{kept}");
        }
        // Rewritten once, then left alone.
        assert_eq!(kafka_plan(&new), KafkaPlan::Ok);
    }

    #[test]
    fn modified_kafka_listeners_are_warned_about_not_edited() {
        let edited = BREW_KAFKA.replace(
            "listeners=PLAINTEXT://:9092,CONTROLLER://:9093",
            "listeners=PLAINTEXT://:9092,CONTROLLER://:9093,EXTERNAL://:9094",
        );
        assert_eq!(kafka_plan(&edited), KafkaPlan::Warn);
        let public_advertised =
            BREW_KAFKA.replace("PLAINTEXT://localhost:9092,", "PLAINTEXT://10.0.0.5:9092,");
        assert_eq!(kafka_plan(&public_advertised), KafkaPlan::Warn);
        // Kafka 3's sample leaves `listeners` commented out, which binds everything.
        assert_eq!(
            kafka_plan("#listeners=PLAINTEXT://:9092\n"),
            KafkaPlan::Warn
        );
    }

    #[test]
    fn secure_kafka_rewrites_stock_brew_file_but_never_apt() {
        let state = crate::test_support::tmp_dir();
        let etc = crate::test_support::tmp_dir();
        std::fs::create_dir_all(etc.join("kafka")).unwrap();
        let conf = etc.join("kafka/server.properties");
        std::fs::write(&conf, BREW_KAFKA).unwrap();
        secure_kafka(&pm("apt", &etc), Some(&state));
        assert_eq!(std::fs::read_to_string(&conf).unwrap(), BREW_KAFKA);
        assert!(state.join("loopback-warned-kafka").is_file(), "apt warns");
        secure_kafka(&pm("brew", &etc), Some(&state));
        let text = std::fs::read_to_string(&conf).unwrap();
        assert_eq!(kafka_plan(&text), KafkaPlan::Ok);
        // Other backends are untouched.
        std::fs::write(&conf, BREW_KAFKA).unwrap();
        for name in ["nix", "winget"] {
            secure_kafka(&pm(name, &etc), Some(&state));
        }
        assert_eq!(std::fs::read_to_string(&conf).unwrap(), BREW_KAFKA);
    }

    #[test]
    fn warn_once_records_a_fingerprint_per_content() {
        let state = crate::test_support::tmp_dir();
        let stamp = state.join("loopback-warned-k");
        warn_once(Some(&state), "k", "v1", "msg");
        let first = std::fs::read_to_string(&stamp).unwrap();
        warn_once(Some(&state), "k", "v1", "msg");
        assert_eq!(std::fs::read_to_string(&stamp).unwrap(), first);
        warn_once(Some(&state), "k", "v2", "msg");
        assert_ne!(std::fs::read_to_string(&stamp).unwrap(), first);
    }

    #[test]
    fn stock_brew_zoo_cfg_gets_loopback_lines_and_edited_one_is_left_alone() {
        let state = crate::test_support::tmp_dir();
        let etc = crate::test_support::tmp_dir();
        std::fs::create_dir_all(etc.join("zookeeper")).unwrap();
        let cfg = etc.join("zookeeper/zoo.cfg");
        let stock = "# The number of milliseconds of each tick\ntickTime=2000\ninitLimit=10\nsyncLimit=5\ndataDir=/opt/homebrew/var/run/zookeeper/data\nclientPort=2181\n";
        std::fs::write(&cfg, stock).unwrap();
        secure_zookeeper(&pm("brew", &etc), Some(&state));
        let text = std::fs::read_to_string(&cfg).unwrap();
        assert!(text.starts_with(stock));
        assert!(zookeeper_ok(&properties(&text)), "{text}");

        // A `key value` line (valid for Java) means it is not the stock file.
        let spaced = format!("{stock}clientPortAddress 0.0.0.0\n");
        std::fs::write(&cfg, &spaced).unwrap();
        secure_zookeeper(&pm("brew", &etc), Some(&state));
        assert_eq!(std::fs::read_to_string(&cfg).unwrap(), spaced);

        let edited = format!("{stock}4lw.commands.whitelist=*\n");
        std::fs::write(&cfg, &edited).unwrap();
        secure_zookeeper(&pm("brew", &etc), Some(&state));
        assert_eq!(std::fs::read_to_string(&cfg).unwrap(), edited);
        assert!(state.join("loopback-warned-zookeeper").is_file());

        // apt's lives in conf/ and is never edited.
        std::fs::create_dir_all(etc.join("zookeeper/conf")).unwrap();
        let apt_cfg = etc.join("zookeeper/conf/zoo.cfg");
        std::fs::write(&apt_cfg, stock).unwrap();
        secure_zookeeper(&pm("apt", &etc), Some(&state));
        assert_eq!(std::fs::read_to_string(&apt_cfg).unwrap(), stock);
    }

    #[test]
    fn rabbitmq_conf_d_lists_enabled_and_implied_plugin_listeners() {
        let plugins = enabled_plugins(
            "[rabbitmq_management,rabbitmq_stomp,rabbitmq_amqp1_0,rabbitmq_mqtt,rabbitmq_stream].\n",
        );
        assert_eq!(plugins.len(), 5);
        let (conf, foreign) = rabbitmq_conf_d(&plugins, 5673, true, "");
        assert!(foreign.is_empty());
        assert!(conf.starts_with(DEVY_MARKER));
        for line in [
            "listeners.tcp.default = 127.0.0.1:5673",
            "distribution.listener.interface = 127.0.0.1",
            "management.tcp.ip = 127.0.0.1",
            "stomp.listeners.tcp.1 = 127.0.0.1:61613",
            "mqtt.listeners.tcp.default = 127.0.0.1:1883",
            "stream.listeners.tcp.default = 127.0.0.1:5552",
            "stream.advertised_host = localhost",
        ] {
            assert!(conf.contains(line), "{line}");
        }
        assert!(!conf.contains("prometheus"));
        // Without an @localhost node name, distribution isn't bound (rabbitmqctl would
        // fail to reach the node).
        let (conf, _) = rabbitmq_conf_d(&[], 5672, false, "");
        assert!(!conf.contains("management"));
        assert!(!conf.contains("distribution"));
        assert!(enabled_plugins("").is_empty());
        // Dependencies RabbitMQ starts implicitly.
        let implied = enabled_plugins(
            "[rabbitmq_federation_management,rabbitmq_web_stomp,rabbitmq_web_mqtt,rabbitmq_stream_management].",
        );
        for p in [
            "rabbitmq_management",
            "rabbitmq_stomp",
            "rabbitmq_mqtt",
            "rabbitmq_stream",
        ] {
            assert!(implied.iter().any(|q| q == p), "{p}: {implied:?}");
        }
        let examples = enabled_plugins("[rabbitmq_web_stomp_examples,rabbitmq_top].");
        for p in [
            "rabbitmq_web_stomp",
            "rabbitmq_stomp",
            "rabbitmq_management",
        ] {
            assert!(examples.iter().any(|q| q == p), "{p}: {examples:?}");
        }
    }

    #[test]
    fn rabbitmq_user_listeners_are_never_overridden() {
        assert!(loopback_value("listeners.tcp = none"));
        let user = "listeners.tcp.default = 5673\nmanagement.tcp.ip = 127.0.0.1\n";
        let plugins = vec!["rabbitmq_management".to_string()];
        let (conf, foreign) = rabbitmq_conf_d(&plugins, 5672, true, user);
        assert!(!conf.contains("listeners.tcp.default"), "{conf}");
        assert!(!conf.contains("management.tcp.ip"), "{conf}");
        assert_eq!(foreign, ["listeners.tcp.default = 5673"]);
        // A port-only setting doesn't stop devy binding the plugin's address.
        let web = vec!["rabbitmq_web_stomp".to_string()];
        let (conf, foreign) = rabbitmq_conf_d(&web, 5672, true, "web_stomp.tcp.port = 15675\n");
        assert!(conf.contains("web_stomp.tcp.ip = 127.0.0.1"), "{conf}");
        assert!(foreign.is_empty(), "{foreign:?}");
    }

    #[test]
    fn rabbitmq_env_missing_reads_prefixed_quoted_and_exported_keys() {
        // Homebrew's stock file only sets the AMQP address (and an @localhost node).
        let brew = "CONFIG_FILE=/opt/homebrew/etc/rabbitmq/rabbitmq\nNODE_IP_ADDRESS=127.0.0.1\nNODENAME=rabbit@localhost\n";
        assert_eq!(
            rabbitmq_env_missing(brew, 5672),
            ["export ERL_EPMD_ADDRESS=\"127.0.0.1\""]
        );
        // NODE_IP_ADDRESS makes RabbitMQ ignore the conf.d listener, so the port must
        // come from NODE_PORT.
        assert_eq!(
            rabbitmq_env_missing(brew, 5673),
            [
                "NODE_PORT=\"5673\"",
                "export ERL_EPMD_ADDRESS=\"127.0.0.1\""
            ]
        );
        assert!(rabbitmq_node_local(&rabbitmq_env_vars(brew)));
        let full =
            "export RABBITMQ_NODE_IP_ADDRESS='127.0.0.1'\nexport ERL_EPMD_ADDRESS=127.0.0.1\n";
        // NODE_PORT alone also replaces the conf.d listener, on every interface.
        assert_eq!(
            rabbitmq_env_missing("NODE_PORT=5672\nexport ERL_EPMD_ADDRESS=127.0.0.1\n", 5672),
            ["NODE_IP_ADDRESS=\"127.0.0.1\""]
        );
        assert_eq!(
            rabbitmq_env_missing("NODE_PORT=5672\nexport ERL_EPMD_ADDRESS=127.0.0.1\n", 5673),
            ["NODE_IP_ADDRESS=\"127.0.0.1\"", "NODE_PORT=\"5673\""]
        );
        assert!(rabbitmq_env_missing(full, 5672).is_empty());
        assert!(!rabbitmq_node_local(&rabbitmq_env_vars(full)));
        assert_eq!(
            rabbitmq_env_missing(
                "NODE_IP_ADDRESS=0.0.0.0\nexport ERL_EPMD_ADDRESS=127.0.0.1\n",
                5672
            ),
            ["NODE_IP_ADDRESS=\"127.0.0.1\""]
        );
        // Debian's stock file is all comments.
        assert_eq!(
            rabbitmq_env_missing("#NODE_IP_ADDRESS=127.0.0.1\n", 5672),
            ["export ERL_EPMD_ADDRESS=\"127.0.0.1\""]
        );
        // A line pasted with a trailing comment still counts.
        let pasted = "NODENAME=\"rabbit@localhost\"  # added for devy\nexport ERL_EPMD_ADDRESS=127.0.0.1 # epmd\n";
        assert!(rabbitmq_node_local(&rabbitmq_env_vars(pasted)));
        assert!(rabbitmq_env_missing(pasted, 5672).is_empty());
        let devy = rabbitmq_env_file();
        assert!(!devy.contains("NODE_IP_ADDRESS"));
        assert!(rabbitmq_env_missing(&devy, 5673).is_empty());
        assert!(rabbitmq_node_local(&rabbitmq_env_vars(&devy)));
        assert!(devy.contains("export ERL_EPMD_ADDRESS="));
    }

    #[test]
    fn epmd_address_counts_only_when_exported() {
        let epmd = "export ERL_EPMD_ADDRESS=\"127.0.0.1\"";
        // Sourced without `set -a`: a plain assignment never reaches epmd.
        assert!(!epmd_address_exported("ERL_EPMD_ADDRESS=127.0.0.1\n"));
        assert_eq!(
            rabbitmq_env_missing("ERL_EPMD_ADDRESS=127.0.0.1\n", 5672),
            [epmd]
        );
        assert!(epmd_address_exported("export ERL_EPMD_ADDRESS=127.0.0.1\n"));
        assert!(epmd_address_exported(
            "ERL_EPMD_ADDRESS=127.0.0.1\nexport NODENAME ERL_EPMD_ADDRESS\n"
        ));
        assert!(epmd_address_exported(
            "set -a\nERL_EPMD_ADDRESS='127.0.0.1'\nset +a\n"
        ));
        // The last assignment decides the value; the export attribute sticks.
        assert!(!epmd_address_exported(
            "export ERL_EPMD_ADDRESS=127.0.0.1\nERL_EPMD_ADDRESS=0.0.0.0\n"
        ));
        assert!(!epmd_address_exported(
            "# export ERL_EPMD_ADDRESS=127.0.0.1\n"
        ));
    }

    #[test]
    fn rabbitmq_tls_listeners_must_bind_loopback() {
        let user = concat!(
            "listeners.ssl.default = 5671\n",
            "listeners.ssl.local = 127.0.0.1:5672\n",
            "management.ssl.port = 15671\n",
            "web_stomp.ssl.port = 15673\n",
            "web_stomp.ssl.ip = 127.0.0.1\n",
            "prometheus.ssl.port = 15691\n",
            "prometheus.ssl.ip = 0.0.0.0\n",
        );
        let (_, foreign) = rabbitmq_conf_d(&[], 5672, true, user);
        assert_eq!(
            foreign,
            [
                "listeners.ssl.default = 5671",
                "management.ssl.port = 15671",
                "prometheus.ssl.port = 15691",
            ]
        );
    }

    #[test]
    fn user_stream_advertised_host_is_kept() {
        let plugins = vec!["rabbitmq_stream".to_string()];
        let (conf, foreign) = rabbitmq_conf_d(
            &plugins,
            5672,
            true,
            "stream.advertised_host = broker.lan\n",
        );
        assert!(
            conf.contains("stream.listeners.tcp.default = 127.0.0.1:5552"),
            "{conf}"
        );
        assert!(!conf.contains("stream.advertised_host"), "{conf}");
        assert!(foreign.is_empty(), "{foreign:?}");
    }

    #[test]
    fn kafka_lines_without_equals_are_warned_about() {
        for line in [
            "listeners: PLAINTEXT://:9092",
            "listeners PLAINTEXT://:9092",
        ] {
            let text = format!("listeners={KAFKA_LOOPBACK_LISTENERS}\n{line}\n");
            assert_eq!(kafka_plan(&text), KafkaPlan::Warn, "{line}");
        }
        assert_eq!(
            kafka_plan(&format!("{BREW_KAFKA}log.retention.hours 1\n")),
            KafkaPlan::Warn
        );
    }

    #[test]
    fn only_key_eq_value_follows_javas_key_terminators_and_continuations() {
        for ok in [
            "a=b\n",
            "a = b\n",
            "a=b:c d\n",
            "  a\t=b\n",
            "# comment: x\n! other\n\na=b\n",
            "a=C:\\\\\n",
        ] {
            assert!(only_key_eq_value(ok), "{ok:?}");
        }
        for bad in [
            // Java's key is `listeners` and the value `x=PLAINTEXT://:9092`.
            "listeners x=PLAINTEXT://:9092\n",
            "listeners:PLAINTEXT://:9092=x\n",
            "a : b\n",
            "a b\n",
            // Continued on the next line, which then becomes part of the value.
            "a=b\\\nlisteners=PLAINTEXT://:9092\n",
            "a=b\\\\\\\r\nc=d\n",
            "# trailing \\\nlisteners=x\n",
            // Java unescapes keys: these set `listeners` and `clientPortAddress`.
            "listen\\ers=PLAINTEXT://:9092\n",
            "clientPortAddr\\ess=0.0.0.0\n",
            "a\\=b=c\n",
        ] {
            assert!(!only_key_eq_value(bad), "{bad:?}");
        }
    }

    #[test]
    fn escaped_keys_and_values_are_warned_about() {
        let text = format!("{BREW_KAFKA}listen\\ers=PLAINTEXT://:9092\n");
        assert_eq!(kafka_plan(&text), KafkaPlan::Warn);
        let text = "listeners=PLAINTEXT://127.0.0.1:9092\\u002cPLAINTEXT\\u003a//\\u003a9093\n";
        assert_eq!(kafka_plan(text), KafkaPlan::Warn);
        let zoo =
            "clientPortAddress=127.0.0.1\nadmin.enableServer=false\nclientPortAddr\\ess=0.0.0.0\n";
        assert!(!only_key_eq_value(zoo));
    }

    #[test]
    fn zookeeper_tls_and_metrics_ports_need_loopback_hosts() {
        let base = vec![
            ("clientPortAddress", "127.0.0.1"),
            ("admin.enableServer", "false"),
        ];
        assert!(zookeeper_ok(&base));
        let mut tls = base.clone();
        tls.push(("secureClientPort", "2281"));
        assert!(!zookeeper_ok(&tls));
        tls.push(("secureClientPortAddress", "127.0.0.1"));
        assert!(zookeeper_ok(&tls));
        let mut metrics = base.clone();
        metrics.push(("metricsProvider.httpPort", "7000"));
        assert!(!zookeeper_ok(&metrics));
        metrics.push(("metricsProvider.httpHost", "0.0.0.0"));
        assert!(!zookeeper_ok(&metrics));
        metrics.push(("metricsProvider.httpHost", "localhost"));
        assert!(zookeeper_ok(&metrics));
        // The provider alone listens on its default port.
        let mut provider = base.clone();
        provider.push((
            "metricsProvider.className",
            "org.apache.zookeeper.metrics.prometheus.PrometheusMetricsProvider",
        ));
        assert!(!zookeeper_ok(&provider));
        provider.push(("metricsProvider.httpHost", "127.0.0.1"));
        assert!(zookeeper_ok(&provider));
    }

    #[test]
    fn missing_zoo_cfg_and_unreadable_kafka_config_warn() {
        let state = crate::test_support::tmp_dir();
        let etc = crate::test_support::tmp_dir();
        secure_zookeeper(&pm("brew", &etc), Some(&state));
        assert!(state.join("loopback-warned-zookeeper-missing").is_file());
        // A server.properties that is not UTF-8 can't be read as text.
        std::fs::create_dir_all(etc.join("kafka")).unwrap();
        std::fs::write(etc.join("kafka/server.properties"), [0xff, 0xfe, 0x00]).unwrap();
        secure_kafka(&pm("brew", &etc), Some(&state));
        assert!(state.join("loopback-warned-kafka-unreadable").is_file());
    }

    #[test]
    fn custom_rabbitmq_config_locations_are_named() {
        let state = crate::test_support::tmp_dir();
        let dir = crate::test_support::tmp_dir();
        let env = dir.join("rabbitmq-env.conf");
        // Homebrew's stock CONFIG_FILE is the default location.
        let stock = rabbitmq_env_vars(&format!("CONFIG_FILE={}\n", dir.join("rabbitmq").display()));
        warn_custom_rabbitmq_paths(&stock, &dir, &env, Some(&state));
        assert!(!state.join("loopback-warned-rabbitmq-custom-paths").exists());
        // The default conf.d (with or without a trailing slash, or through a symlink to
        // it) is what devy reads.
        std::fs::create_dir(dir.join("conf.d")).unwrap();
        let mut defaults = vec![
            format!("CONFIG_FILES={}\n", dir.join("conf.d").display()),
            format!("CONFIG_FILES={}/\n", dir.join("conf.d").display()),
            format!(
                "ENABLED_PLUGINS_FILE={}/\n",
                dir.join("enabled_plugins").display()
            ),
        ];
        let alias = crate::test_support::tmp_dir();
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(&*dir, alias.join("r")).unwrap();
            defaults.push(format!(
                "CONFIG_FILES={}\n",
                alias.join("r/conf.d").display()
            ));
        }
        for text in defaults {
            warn_custom_rabbitmq_paths(&rabbitmq_env_vars(&text), &dir, &env, Some(&state));
            assert!(
                !state.join("loopback-warned-rabbitmq-custom-paths").exists(),
                "{text}"
            );
        }
        let elsewhere = rabbitmq_env_vars("CONFIG_FILES=/srv/rabbit/conf.d\n");
        warn_custom_rabbitmq_paths(&elsewhere, &dir, &env, Some(&state));
        assert!(
            state
                .join("loopback-warned-rabbitmq-custom-paths")
                .is_file()
        );
        std::fs::remove_file(state.join("loopback-warned-rabbitmq-custom-paths")).unwrap();
        let custom = rabbitmq_env_vars("ENABLED_PLUGINS_FILE=/srv/plugins\n");
        warn_custom_rabbitmq_paths(&custom, &dir, &env, Some(&state));
        assert!(
            state
                .join("loopback-warned-rabbitmq-custom-paths")
                .is_file()
        );
    }

    #[test]
    fn secure_rabbitmq_owns_its_files_and_never_edits_a_foreign_env_file() {
        let state = crate::test_support::tmp_dir();
        let etc = crate::test_support::tmp_dir();
        let dir = etc.join("rabbitmq");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("enabled_plugins"), "[rabbitmq_management].\n").unwrap();

        // No env file: devy writes (and owns) both.
        secure_rabbitmq(&pm("brew", &etc), 5672, Some(&state));
        let conf_d = std::fs::read_to_string(dir.join("conf.d/90-devy.conf")).unwrap();
        assert!(conf_d.contains("management.tcp.ip = 127.0.0.1"));
        assert!(conf_d.contains("distribution.listener.interface = 127.0.0.1"));
        assert_eq!(
            std::fs::read_to_string(dir.join("rabbitmq-env.conf")).unwrap(),
            rabbitmq_env_file()
        );

        // A user's env file is left as is and warned about; without an @localhost node
        // name distribution is not bound.
        let user_env = "NODE_IP_ADDRESS=127.0.0.1\nNODENAME=rabbit@myhost\n";
        std::fs::write(dir.join("rabbitmq-env.conf"), user_env).unwrap();
        secure_rabbitmq(&pm("brew", &etc), 5672, Some(&state));
        assert_eq!(
            std::fs::read_to_string(dir.join("rabbitmq-env.conf")).unwrap(),
            user_env
        );
        assert!(state.join("loopback-warned-rabbitmq-env").is_file());
        let conf_d = std::fs::read_to_string(dir.join("conf.d/90-devy.conf")).unwrap();
        assert!(!conf_d.contains("distribution"), "{conf_d}");

        // A foreign file at devy's conf.d path is never overwritten.
        std::fs::write(
            dir.join("conf.d/90-devy.conf"),
            "listeners.tcp.default = 5672\n",
        )
        .unwrap();
        secure_rabbitmq(&pm("brew", &etc), 5672, Some(&state));
        assert_eq!(
            std::fs::read_to_string(dir.join("conf.d/90-devy.conf")).unwrap(),
            "listeners.tcp.default = 5672\n"
        );
    }

    #[cfg(unix)]
    #[test]
    fn rewrite_keeps_a_private_file_private() {
        use std::os::unix::fs::PermissionsExt;
        let state = crate::test_support::tmp_dir();
        let etc = crate::test_support::tmp_dir();
        std::fs::create_dir_all(etc.join("kafka")).unwrap();
        let conf = etc.join("kafka/server.properties");
        std::fs::write(&conf, BREW_KAFKA).unwrap();
        std::fs::set_permissions(&conf, std::fs::Permissions::from_mode(0o600)).unwrap();
        secure_kafka(&pm("brew", &etc), Some(&state));
        assert_eq!(
            kafka_plan(&std::fs::read_to_string(&conf).unwrap()),
            KafkaPlan::Ok
        );
        let mode = std::fs::metadata(&conf).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_kafka_config_is_not_followed() {
        let state = crate::test_support::tmp_dir();
        let etc = crate::test_support::tmp_dir();
        let elsewhere = crate::test_support::tmp_dir();
        std::fs::create_dir_all(etc.join("kafka")).unwrap();
        let target = elsewhere.join("server.properties");
        std::fs::write(&target, BREW_KAFKA).unwrap();
        std::os::unix::fs::symlink(&target, etc.join("kafka/server.properties")).unwrap();
        secure_kafka(&pm("brew", &etc), Some(&state));
        assert_eq!(std::fs::read_to_string(&target).unwrap(), BREW_KAFKA);
        assert!(
            state
                .join("loopback-warned-kafka-write-server.properties")
                .is_file()
        );
    }
}
