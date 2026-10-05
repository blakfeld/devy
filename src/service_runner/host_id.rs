//! The value of the `sh.devy.host` label: an opaque id for this machine and user, so
//! `devy prune` never mistakes another machine's containers for this one's on a daemon
//! several machines share (WSL distros on Docker Desktop, a devcontainer using its
//! host's socket, a forwarded socket). It hashes a machine identifier with the user
//! (SHA-256 with a fixed, public devy-specific prefix for domain separation; not a
//! secret), so no raw identifier is ever written to a label.
//! When neither a machine identifier nor a host name can be read, or on Windows the
//! user is unknown, there is no id: devy leaves the label off rather than share one
//! with other unidentified machines, and `devy prune` removes no container.

use std::process::{Command, Stdio};
use std::sync::OnceLock;

/// The operating systems whose machine identifier `parts` knows how to read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Platform {
    Linux,
    MacOs,
    Windows,
    Other,
}

impl Platform {
    fn current() -> Platform {
        if cfg!(target_os = "linux") {
            Platform::Linux
        } else if cfg!(target_os = "macos") {
            Platform::MacOs
        } else if cfg!(windows) {
            Platform::Windows
        } else {
            Platform::Other
        }
    }
}

/// Where `parts` reads machine and user identifiers from; injected in tests.
struct Probe<'a> {
    /// A file's trimmed contents, or `None` when it is missing, unreadable or empty.
    read: &'a dyn Fn(&str) -> Option<String>,
    /// An environment variable, or `None` when unset or empty.
    var: &'a dyn Fn(&str) -> Option<String>,
    /// Whether a path exists.
    exists: &'a dyn Fn(&str) -> bool,
    /// The stdout of a system tool run with arguments, or `None` when it fails.
    run: &'a dyn Fn(&str, &[&str]) -> Option<String>,
    /// The real uid on unix.
    uid: Option<u32>,
}

/// This machine and user's id: 16 lowercase hex characters, computed once per process.
/// `None` when devy can't identify this machine and user (see the module docs).
pub(crate) fn current() -> Option<&'static str> {
    static ID: OnceLock<Option<String>> = OnceLock::new();
    ID.get_or_init(|| {
        let read = |path: &str| non_empty(std::fs::read_to_string(path).ok());
        let var = |name: &str| non_empty(std::env::var(name).ok());
        let exists = |path: &str| std::path::Path::new(path).exists();
        let probe = Probe {
            read: &read,
            var: &var,
            exists: &exists,
            run: &run_system_tool,
            uid: uid(),
        };
        parts(Platform::current(), &probe).map(|p| id_from(&p))
    })
    .as_deref()
}

#[cfg(unix)]
fn uid() -> Option<u32> {
    Some(crate::fs_safe::current_uid())
}

#[cfg(not(unix))]
fn uid() -> Option<u32> {
    None
}

/// `s` trimmed, or `None` when that leaves nothing.
fn non_empty(s: Option<String>) -> Option<String> {
    s.map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
}

/// Runs system tool `name` (an absolute path, or on Windows a name in the system
/// directory) with `args` and no stdin, returning its stdout when it succeeds.
#[cfg_attr(test, mutants::skip)] // runs real system tools
fn run_system_tool(name: &str, args: &[&str]) -> Option<String> {
    let program = if cfg!(windows) {
        // Only an absolute system directory: a relative one would resolve against the
        // working directory, e.g. a repository's own `System32\reg.exe`. (A project's
        // devy.yml can't set `SystemRoot`: see `validate::reserved_env_key`.)
        let root = std::path::PathBuf::from(non_empty(std::env::var("SystemRoot").ok())?);
        if !root.is_absolute() {
            return None;
        }
        let path = root.join("System32").join(format!("{name}.exe"));
        path.is_file().then_some(path)?
    } else {
        crate::package_manager::system_tool(name)?
    };
    let out = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    non_empty(Some(String::from_utf8_lossy(&out.stdout).into_owned()))
}

/// The value of `"key" = "value"` in `ioreg -rd1 -c IOPlatformExpertDevice` output.
fn ioreg_value(output: &str, key: &str) -> Option<String> {
    let quoted = format!("\"{key}\"");
    output
        .lines()
        .find_map(|line| {
            let (k, v) = line.split_once('=')?;
            (k.trim() == quoted).then(|| v.trim().trim_matches('"').to_string())
        })
        .filter(|v| !v.is_empty())
}

/// The data of value `name` in `reg query <key> /v <name>` output, whose line reads
/// `    <name>    REG_SZ    <data>`.
fn reg_value(output: &str, name: &str) -> Option<String> {
    output.lines().find_map(|line| {
        let mut fields = line.split_whitespace();
        if fields.next()? != name || !fields.next()?.starts_with("REG_") {
            return None;
        }
        let data = fields.collect::<Vec<_>>().join(" ");
        (!data.is_empty()).then_some(data)
    })
}

/// The OS's own `hostname`, by absolute path so a PATH entry (brew, nix) can't change
/// the id between shells.
const HOSTNAME: &str = "/bin/hostname";
/// macOS's `ioreg`, by absolute path (see `HOSTNAME`).
const IOREG: &str = "/usr/sbin/ioreg";

/// The host name: the kernel's on Linux (a container's is its own, for docker its id),
/// else the `hostname` tool's, else `COMPUTERNAME` on Windows.
fn hostname(os: Platform, probe: &Probe) -> Option<String> {
    match os {
        Platform::Linux => (probe.read)("/proc/sys/kernel/hostname")
            .or_else(|| (probe.read)("/etc/hostname"))
            .or_else(|| (probe.run)(HOSTNAME, &[])),
        Platform::Windows => (probe.var)("COMPUTERNAME"),
        Platform::MacOs | Platform::Other => (probe.run)(HOSTNAME, &[]),
    }
}

/// Whether this Linux runs under WSL: its kernel release names Microsoft, or it has the
/// WSL interop handler.
fn is_wsl(probe: &Probe) -> bool {
    (probe.read)("/proc/sys/kernel/osrelease")
        .is_some_and(|r| r.to_ascii_lowercase().contains("microsoft"))
        || (probe.exists)("/proc/sys/fs/binfmt_misc/WSLInterop")
}

/// What identifies this machine and user, each part prefixed with its source so values
/// from different sources never collide. `None` when a part it needs can't be read: a
/// host name (without a machine identifier, or in a container), the distro name under
/// WSL, or the user.
fn parts(os: Platform, probe: &Probe) -> Option<Vec<String>> {
    let mut parts = Vec::new();
    let machine = match os {
        Platform::Linux => (probe.read)("/etc/machine-id")
            .or_else(|| (probe.read)("/var/lib/dbus/machine-id"))
            .map(|id| format!("machine-id:{id}")),
        // The hardware UUID. (Not `sysctl kern.uuid`: that is the kernel build's, which
        // changes with OS updates.) The host name, the fallback, can change with the
        // network on macOS.
        Platform::MacOs => (probe.run)(IOREG, &["-rd1", "-c", "IOPlatformExpertDevice"])
            .and_then(|out| ioreg_value(&out, "IOPlatformUUID"))
            .map(|id| format!("platform-uuid:{id}")),
        // `/reg:64`: a 32-bit build would otherwise read the 32-bit registry view, which
        // has no `MachineGuid`.
        Platform::Windows => (probe.run)(
            "reg",
            &[
                "query",
                r"HKLM\SOFTWARE\Microsoft\Cryptography",
                "/v",
                "MachineGuid",
                "/reg:64",
            ],
        )
        .and_then(|out| reg_value(&out, "MachineGuid"))
        .map(|id| format!("machine-guid:{id}")),
        Platform::Other => None,
    };
    let has_machine_id = machine.is_some();
    parts.extend(machine);
    // A container may share its host's machine-id (bind-mounted) or have none; its host
    // name (for docker, the container id) tells it apart from the host and other
    // containers.
    let in_container = os == Platform::Linux
        && ((probe.exists)("/.dockerenv") || (probe.exists)("/run/.containerenv"));
    if !has_machine_id || in_container {
        parts.push(format!("hostname:{}", hostname(os, probe)?));
    }
    // WSL distros share the Windows host name, and a distro cloned with `wsl --import`
    // keeps its source's machine-id, so they're told apart by name. The name is only
    // in the environment of `wsl.exe` sessions; without it (ssh, cron) there is no id,
    // rather than one another distro may share.
    // A container on a WSL2 kernel sees the same kernel release but no distro name; its
    // host name already tells it apart.
    if os == Platform::Linux && !in_container && is_wsl(probe) {
        parts.push(format!("wsl-distro:{}", (probe.var)("WSL_DISTRO_NAME")?));
    }
    parts.push(match os {
        Platform::Windows => format!(
            "user:{}\\{}",
            (probe.var)("USERDOMAIN").unwrap_or_default(),
            (probe.var)("USERNAME")?
        ),
        _ => format!("uid:{}", probe.uid?),
    });
    Some(parts)
}

/// The id for `parts`: the first 64 bits, in hex, of the SHA-256 of a fixed devy-specific
/// prefix and the parts, one per line. The prefix is public, not a secret: it only
/// separates this hash from other hashes of the same identifiers, so the label can't be
/// matched against their plain hashes elsewhere.
fn id_from(parts: &[String]) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(b"sh.devy.host v1\n");
    hasher.update(parts.join("\n").as_bytes());
    hasher.finalize()[..8]
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    /// A machine whose files, environment and tool output are fixed.
    #[derive(Default, Clone)]
    struct Machine {
        files: HashMap<&'static str, &'static str>,
        vars: HashMap<&'static str, &'static str>,
        tools: HashMap<&'static str, &'static str>,
        uid: Option<u32>,
    }

    impl Machine {
        fn parts(&self, os: Platform) -> Vec<String> {
            self.try_parts(os).expect("an identified machine")
        }

        fn try_parts(&self, os: Platform) -> Option<Vec<String>> {
            let read = |p: &str| non_empty(self.files.get(p).map(|s| s.to_string()));
            let var = |k: &str| non_empty(self.vars.get(k).map(|s| s.to_string()));
            let exists = |p: &str| self.files.contains_key(p);
            let run = |t: &str, _: &[&str]| non_empty(self.tools.get(t).map(|s| s.to_string()));
            parts(
                os,
                &Probe {
                    read: &read,
                    var: &var,
                    exists: &exists,
                    run: &run,
                    uid: self.uid,
                },
            )
        }

        fn id(&self, os: Platform) -> String {
            id_from(&self.parts(os))
        }
    }

    fn linux() -> Machine {
        Machine {
            files: HashMap::from([
                ("/etc/machine-id", "0123456789abcdef0123456789abcdef\n"),
                ("/proc/sys/kernel/hostname", "box\n"),
            ]),
            uid: Some(1000),
            ..Default::default()
        }
    }

    #[test]
    fn the_id_is_sixteen_hex_characters_and_stable() {
        // The machine running the tests may not be identifiable (no machine-id, e.g.
        // WSL over ssh or cron); when it is, its id is stable and well-formed.
        let id = current();
        assert_eq!(current(), id);
        let fake = linux().id(Platform::Linux);
        assert_eq!(fake, linux().id(Platform::Linux));
        for id in id.into_iter().chain([fake.as_str()]) {
            assert_eq!(id.len(), 16, "{id}");
            assert!(
                id.chars()
                    .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()),
                "{id}"
            );
        }
        // Pinned, so a change to the derivation (which would orphan every labeled
        // container from `devy prune`) is deliberate.
        assert_eq!(
            id_from(&["machine-id:x".into(), "uid:1".into()]),
            "42aea28cf48c3652"
        );
        assert_ne!(
            id_from(&["machine-id:x".into(), "uid:1".into()]),
            id_from(&["machine-id:x".into(), "uid:2".into()])
        );
    }

    #[test]
    fn an_unidentifiable_machine_or_user_has_no_id() {
        // No machine-id and no host name anywhere.
        let mut nameless = linux();
        nameless.files.remove("/etc/machine-id");
        nameless.files.remove("/proc/sys/kernel/hostname");
        assert_eq!(nameless.try_parts(Platform::Linux), None);
        // A container whose host name can't be read may share its host's machine-id.
        let mut container = linux();
        container.files.insert("/.dockerenv", "");
        container.files.remove("/proc/sys/kernel/hostname");
        assert_eq!(container.try_parts(Platform::Linux), None);
        // No uid (non-unix) on a unix-like platform.
        let mut no_uid = linux();
        no_uid.uid = None;
        assert_eq!(no_uid.try_parts(Platform::Linux), None);
        // Other platforms need a host name.
        assert_eq!(Machine::default().try_parts(Platform::Other), None);
        let mut bsd = Machine::default();
        bsd.tools.insert(HOSTNAME, "bsd-box\n");
        bsd.uid = Some(1001);
        assert_eq!(
            bsd.try_parts(Platform::Other),
            Some(vec!["hostname:bsd-box".to_string(), "uid:1001".to_string()])
        );
    }

    #[test]
    fn linux_uses_machine_id_then_dbus_then_hostname() {
        assert_eq!(
            linux().parts(Platform::Linux),
            ["machine-id:0123456789abcdef0123456789abcdef", "uid:1000"]
        );
        let mut dbus = linux();
        dbus.files.insert("/etc/machine-id", "  \n");
        dbus.files.insert("/var/lib/dbus/machine-id", "feed\n");
        assert_eq!(dbus.parts(Platform::Linux), ["machine-id:feed", "uid:1000"]);
        let mut bare = linux();
        bare.files.remove("/etc/machine-id");
        assert_eq!(bare.parts(Platform::Linux), ["hostname:box", "uid:1000"]);
        // The host name: the kernel's, else /etc/hostname, else the tool's.
        bare.files.remove("/proc/sys/kernel/hostname");
        bare.files.insert("/etc/hostname", "etc-box\n");
        bare.tools.insert(HOSTNAME, "tool-box\n");
        assert_eq!(
            bare.parts(Platform::Linux),
            ["hostname:etc-box", "uid:1000"]
        );
        bare.files.remove("/etc/hostname");
        assert_eq!(
            bare.parts(Platform::Linux),
            ["hostname:tool-box", "uid:1000"]
        );
        // A container without a machine-id names its host name once.
        bare.files.insert("/.dockerenv", "");
        assert_eq!(
            bare.parts(Platform::Linux),
            ["hostname:tool-box", "uid:1000"]
        );
    }

    #[test]
    fn wsl_always_needs_the_distro_name() {
        // Outside WSL the variable means nothing.
        let mut plain = linux();
        plain.vars.insert("WSL_DISTRO_NAME", "Ubuntu");
        assert_eq!(plain.id(Platform::Linux), linux().id(Platform::Linux));
        // A distro cloned with `wsl --import` keeps the source's machine-id.
        let mut ubuntu = linux();
        ubuntu.files.insert(
            "/proc/sys/kernel/osrelease",
            "5.15.153.1-microsoft-standard-WSL2\n",
        );
        ubuntu.vars.insert("WSL_DISTRO_NAME", "Ubuntu");
        let mut clone = ubuntu.clone();
        clone.vars.insert("WSL_DISTRO_NAME", "Ubuntu-clone");
        assert_ne!(ubuntu.id(Platform::Linux), clone.id(Platform::Linux));
        assert_eq!(
            ubuntu.parts(Platform::Linux),
            [
                "machine-id:0123456789abcdef0123456789abcdef",
                "wsl-distro:Ubuntu",
                "uid:1000"
            ]
        );
        // ssh or cron into a distro leaves the name unset: no id, rather than a shared one.
        let mut ssh = ubuntu.clone();
        ssh.vars.remove("WSL_DISTRO_NAME");
        assert_eq!(ssh.try_parts(Platform::Linux), None);
        // A devcontainer on Docker Desktop for Windows shares the WSL2 kernel but has
        // no distro name: its host name identifies it.
        let mut devcontainer = ssh.clone();
        devcontainer.files.insert("/.dockerenv", "");
        devcontainer
            .files
            .insert("/proc/sys/kernel/hostname", "3f2a9c1d0b7e");
        assert_eq!(
            devcontainer.parts(Platform::Linux),
            [
                "machine-id:0123456789abcdef0123456789abcdef",
                "hostname:3f2a9c1d0b7e",
                "uid:1000"
            ]
        );
        // WSL1 is detected by its interop handler.
        let mut wsl1 = linux();
        wsl1.files
            .insert("/proc/sys/fs/binfmt_misc/WSLInterop", "enabled");
        assert_eq!(wsl1.try_parts(Platform::Linux), None);
    }

    #[test]
    fn the_id_differs_by_user_machine_wsl_distro_and_container() {
        let base = linux();
        let mut root = base.clone();
        root.uid = Some(0);
        let mut other = base.clone();
        other.files.insert("/etc/machine-id", "ffff");
        // Two WSL distros without a machine-id share the Windows host name.
        let mut ubuntu = base.clone();
        ubuntu.files.remove("/etc/machine-id");
        ubuntu
            .files
            .insert("/proc/sys/kernel/osrelease", "6.6-microsoft-standard-WSL2");
        ubuntu.vars.insert("WSL_DISTRO_NAME", "Ubuntu");
        let mut debian = ubuntu.clone();
        debian.vars.insert("WSL_DISTRO_NAME", "Debian");
        // A devcontainer with its host's machine-id bind-mounted, and the same uid.
        let mut container = base.clone();
        container.files.insert("/.dockerenv", "");
        container
            .files
            .insert("/proc/sys/kernel/hostname", "3f2a9c1d0b7e");
        let mut podman = container.clone();
        podman.files.remove("/.dockerenv");
        podman.files.insert("/run/.containerenv", "");
        podman.files.insert("/proc/sys/kernel/hostname", "8d1e");
        let ids: Vec<String> = [&base, &root, &other, &ubuntu, &debian, &container, &podman]
            .iter()
            .map(|m| m.id(Platform::Linux))
            .collect();
        let unique: std::collections::HashSet<_> = ids.iter().collect();
        assert_eq!(unique.len(), ids.len(), "{ids:?}");
        // Raw identifiers never appear in the id.
        assert!(!ids[0].contains("0123456789abcdef0123"));
    }

    #[test]
    fn macos_uses_the_platform_uuid_else_the_hostname() {
        let ioreg = r#"+-o J316sAP  <class IOPlatformExpertDevice, id 0x100000201, registered>
    {
      "IOPlatformSerialNumber" = "C02XXXXX"
      "IOPlatformUUID" = "1A2B3C4D-0000-1111-2222-333344445555"
    }
"#;
        let mac = Machine {
            tools: HashMap::from([(IOREG, ioreg), (HOSTNAME, "laptop.local\n")]),
            uid: Some(501),
            ..Default::default()
        };
        assert_eq!(
            mac.parts(Platform::MacOs),
            [
                "platform-uuid:1A2B3C4D-0000-1111-2222-333344445555",
                "uid:501"
            ]
        );
        let mut no_ioreg = mac.clone();
        no_ioreg.tools.remove(IOREG);
        assert_eq!(
            no_ioreg.parts(Platform::MacOs),
            ["hostname:laptop.local", "uid:501"]
        );
        // WSL and container markers only count on Linux.
        let mut odd = mac.clone();
        odd.files.insert("/.dockerenv", "");
        odd.vars.insert("WSL_DISTRO_NAME", "Ubuntu");
        assert_eq!(odd.parts(Platform::MacOs), mac.parts(Platform::MacOs));
    }

    #[test]
    fn windows_uses_the_machine_guid_else_computername_and_the_user() {
        let reg = "\r\nHKEY_LOCAL_MACHINE\\SOFTWARE\\Microsoft\\Cryptography\r\n    \
                   MachineGuid    REG_SZ    6f1e2d3c-aaaa-bbbb-cccc-ddddeeeeffff\r\n\r\n";
        let win = Machine {
            tools: HashMap::from([("reg", reg)]),
            vars: HashMap::from([
                ("COMPUTERNAME", "DESKTOP-1"),
                ("USERDOMAIN", "CORP"),
                ("USERNAME", "ana"),
            ]),
            ..Default::default()
        };
        assert_eq!(
            win.parts(Platform::Windows),
            [
                "machine-guid:6f1e2d3c-aaaa-bbbb-cccc-ddddeeeeffff",
                "user:CORP\\ana"
            ]
        );
        let mut no_reg = win.clone();
        no_reg.tools.remove("reg");
        assert_eq!(
            no_reg.parts(Platform::Windows),
            ["hostname:DESKTOP-1", "user:CORP\\ana"]
        );
        let mut bob = win.clone();
        bob.vars.insert("USERNAME", "bob");
        assert_ne!(bob.id(Platform::Windows), win.id(Platform::Windows));
        // An unknown user has no id, rather than one shared by every unknown user.
        let mut nobody = win.clone();
        nobody.vars.remove("USERNAME");
        assert_eq!(nobody.try_parts(Platform::Windows), None);
    }

    #[test]
    fn parsers_ignore_unrelated_lines() {
        assert_eq!(
            ioreg_value("\"IOPlatformUUID\" = \"\"", "IOPlatformUUID"),
            None
        );
        assert_eq!(ioreg_value("\"Other\" = \"x\"", "IOPlatformUUID"), None);
        assert_eq!(reg_value("    MachineGuid    REG_SZ", "MachineGuid"), None);
        assert_eq!(reg_value("MachineGuidX REG_SZ x", "MachineGuid"), None);
        assert_eq!(reg_value("ERROR: not found", "MachineGuid"), None);
    }
}
