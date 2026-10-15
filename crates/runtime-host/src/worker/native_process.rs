//! Tracks the native executable reached by a confined launcher script.
//! Process arguments stay in the parent's private, zeroed buffer. Only the
//! selected launch's descendants are inspected, and no environment is logged.

use super::{ProcBsdInfo, TrackedGameProcess, proc_listallpids, proc_pidinfo, process_arguments};
use std::collections::{BTreeMap, BTreeSet};
use std::io::Read;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};

const MAX_PROCESSES: usize = 65_536;
const MAX_ANCESTRY: usize = 64;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ProcessSnapshot {
    pid: i32,
    parent: i32,
    group: i32,
    start: (u64, u64),
    zombie: bool,
}

impl ProcessSnapshot {
    fn capture(pid: i32) -> Option<Self> {
        let mut info = ProcBsdInfo::default();
        let size = std::mem::size_of::<ProcBsdInfo>() as i32;
        // PROC_PIDTBSDINFO: the PID/parent/status fields are supplied by the
        // kernel, not argv. The shared struct is checked against the SDK.
        let actual =
            unsafe { proc_pidinfo(pid, 3, 0, (&mut info as *mut ProcBsdInfo).cast(), size) };
        if actual != size || info.fields[3] != pid as u32 {
            return None;
        }
        Some(Self {
            pid,
            parent: i32::try_from(info.fields[4]).ok()?,
            // pbi_pgid follows pbi_nfiles in sys/proc_info.h.
            group: i32::try_from(info.tail[1]).ok()?,
            start: (info.start_seconds, info.start_microseconds),
            zombie: info.fields[1] == 5,
        })
    }

    fn start_micros(self) -> u128 {
        u128::from(self.start.0) * 1_000_000 + u128::from(self.start.1)
    }
}

pub(super) struct ScriptTracker {
    launcher: Option<ProcessSnapshot>,
    process_group: i32,
    worker: ProcessSnapshot,
    install_root: PathBuf,
    appid: u32,
    state_root: PathBuf,
    spawned_after: u128,
}

impl ScriptTracker {
    pub(super) fn capture(
        launcher_pid: i32,
        process_group: i32,
        spawned_after: u128,
        install_root: &Path,
        appid: u32,
        state_root: &Path,
    ) -> Result<Self, String> {
        let worker = ProcessSnapshot::capture(std::process::id() as i32)
            .ok_or("Cannot verify native launcher owner")?;
        // These two values come from this worker's native adapter, which used
        // POSIX_SPAWN_SETPGROUP with pgroup=0 for this successful script spawn.
        // They must never come from the selected game's argv or public appid.
        if process_group <= 0 || process_group != launcher_pid || spawned_after == 0 {
            return Err("Native script has no proven isolated process group".into());
        }
        let launcher = ProcessSnapshot::capture(launcher_pid);
        if launcher.is_some_and(|launcher| !owned_launcher(launcher, worker, spawned_after)) {
            return Err("Native launcher does not belong to this launch".into());
        }
        let install_root = install_root
            .canonicalize()
            .map_err(|_| "Cannot resolve native installation directory")?;
        if !install_root.is_dir() || !state_root.is_absolute() || appid == 0 {
            return Err("Invalid native process tracking context".into());
        }
        Ok(Self {
            launcher,
            process_group,
            worker,
            install_root,
            appid,
            state_root: state_root.to_owned(),
            spawned_after,
        })
    }

    pub(super) fn find_game(&self) -> Result<Option<TrackedGameProcess>, String> {
        let snapshots = snapshots()?;
        let mut candidates = Vec::new();
        for snapshot in snapshots.values() {
            let Some(chain) = launch_origin(
                snapshot.pid,
                &snapshots,
                self.launcher,
                self.process_group,
                self.worker,
                self.spawned_after,
            ) else {
                continue;
            };
            let Some(process) = TrackedGameProcess::capture(snapshot.pid) else {
                continue;
            };
            if process.start != snapshot.start {
                continue;
            }
            let Ok(executable) = process.executable.canonicalize() else {
                continue;
            };
            if !native_game_path(&executable, &self.install_root) || !is_macho(&executable) {
                continue;
            }
            // Only now read argv/environment: ownership, age, actual executable
            // and containment have already excluded unrelated processes.
            let Some(arguments) = process_arguments(process.pid) else {
                continue;
            };
            if !procargs_match_launch(
                &arguments.0,
                self.appid,
                self.state_root.as_os_str().as_bytes(),
            ) {
                continue;
            }
            drop(arguments);
            if !chain
                .iter()
                .all(|expected| ProcessSnapshot::capture(expected.pid) == Some(*expected))
                || ProcessSnapshot::capture(self.process_group)
                    != snapshots.get(&self.process_group).copied()
                || !process.is_current()
            {
                continue;
            }
            candidates.push(process);
        }
        candidates.retain(TrackedGameProcess::is_current);
        choose_candidate(candidates)
    }
}

fn owned_launcher(launcher: ProcessSnapshot, worker: ProcessSnapshot, spawned_after: u128) -> bool {
    launcher.pid > 0
        && launcher.pid != worker.pid
        && launcher.parent == worker.pid
        && launcher.group == launcher.pid
        && launcher.start >= worker.start
        && launcher.start_micros() >= spawned_after
}

fn launch_origin(
    pid: i32,
    snapshots: &BTreeMap<i32, ProcessSnapshot>,
    launcher: Option<ProcessSnapshot>,
    process_group: i32,
    worker: ProcessSnapshot,
    spawned_after: u128,
) -> Option<Vec<ProcessSnapshot>> {
    if *snapshots.get(&worker.pid)? != worker {
        return None;
    }
    let current_leader = snapshots.get(&process_group).copied();
    match (launcher, current_leader) {
        (Some(expected), Some(actual))
            if expected.pid == actual.pid
                && expected.start == actual.start
                && owned_launcher(actual, worker, spawned_after) => {}
        (_, None) => {}
        // A leader that appeared after we captured an already-dead launcher,
        // or whose start changed, is a reused PID/group rather than our spawn.
        _ => return None,
    }
    let candidate = *snapshots.get(&pid)?;
    if candidate.zombie || candidate.start_micros() < spawned_after {
        return None;
    }
    if let Some(launcher) = launcher
        && let Some(chain) = ancestry(pid, snapshots, launcher, worker)
    {
        return Some(chain);
    }
    // A fast script may background its child and exit before the first poll.
    // Kernel group membership preserves the adapter's launch ownership even
    // after reparenting. The caller additionally requires the private launch
    // environment, exact current PID/start, and a contained native executable.
    if candidate.group != process_group {
        return None;
    }
    let mut proof = vec![candidate, worker];
    if let Some(leader) = current_leader {
        proof.push(leader);
    }
    Some(proof)
}

fn choose_candidate(
    mut candidates: Vec<TrackedGameProcess>,
) -> Result<Option<TrackedGameProcess>, String> {
    match candidates.len() {
        0 => Ok(None),
        1 => Ok(candidates.pop()),
        _ => Err("Launcher produced multiple possible game processes; refusing to guess".into()),
    }
}

fn snapshots() -> Result<BTreeMap<i32, ProcessSnapshot>, String> {
    let count = unsafe { proc_listallpids(std::ptr::null_mut(), 0) };
    if count <= 0 || count as usize > MAX_PROCESSES - 128 {
        return Err("Cannot enumerate native game processes within the process limit".into());
    }
    let mut pids = vec![0i32; count as usize + 128];
    let capacity = std::mem::size_of_val(pids.as_slice()) as i32;
    let filled = unsafe { proc_listallpids(pids.as_mut_ptr().cast(), capacity) };
    if filled < 0 || filled as usize >= pids.len() {
        return Err("Native process list changed during enumeration; retry verification".into());
    }
    Ok(pids
        .into_iter()
        .take(filled as usize)
        .filter(|pid| *pid > 0)
        .filter_map(|pid| ProcessSnapshot::capture(pid).map(|snapshot| (pid, snapshot)))
        .collect())
}

/// Every link must still be the captured instance and older than its child.
/// A reparented orphan cannot prove its former ancestry and is not adopted.
fn ancestry(
    pid: i32,
    snapshots: &BTreeMap<i32, ProcessSnapshot>,
    launcher: ProcessSnapshot,
    worker: ProcessSnapshot,
) -> Option<Vec<ProcessSnapshot>> {
    let actual_launcher = *snapshots.get(&launcher.pid)?;
    let actual_worker = *snapshots.get(&worker.pid)?;
    if actual_launcher != launcher || actual_worker != worker || launcher.parent != worker.pid {
        return None;
    }
    let mut current = *snapshots.get(&pid)?;
    let mut seen = BTreeSet::new();
    let mut chain = Vec::new();
    for _ in 0..MAX_ANCESTRY {
        if current.zombie || !seen.insert(current.pid) || current.start < launcher.start {
            return None;
        }
        chain.push(current);
        if current.pid == launcher.pid {
            chain.push(worker);
            return Some(chain);
        }
        let parent = *snapshots.get(&current.parent)?;
        if parent.start > current.start {
            return None;
        }
        current = parent;
    }
    None
}

fn native_game_path(executable: &Path, install_root: &Path) -> bool {
    let Ok(relative) = executable.strip_prefix(install_root) else {
        return false;
    };
    if relative.as_os_str().is_empty() {
        return false;
    }
    if relative.components().any(|component| {
        let name = component.as_os_str().to_string_lossy().to_ascii_lowercase();
        name.ends_with(".framework") || name.ends_with(".xpc")
    }) {
        return false;
    }
    let Some(name) = executable.file_name() else {
        return false;
    };
    let name = name.to_string_lossy().to_ascii_lowercase();
    !matches!(
        name.as_str(),
        "sh" | "bash" | "zsh" | "dash" | "fish" | "env" | "updater" | "launcher" | "bootstrap"
    ) && !name.ends_with(" helper")
        && !name.ends_with("_helper")
        && !name.ends_with("-helper")
        && ![
            "crashpad",
            "crashhandler",
            "crashreport",
            "steamwebhelper",
            "gameoverlayui",
        ]
        .iter()
        .any(|helper| name.contains(helper))
}

fn is_macho(path: &Path) -> bool {
    let mut magic = [0u8; 4];
    if !std::fs::File::open(path).is_ok_and(|mut file| file.read_exact(&mut magic).is_ok()) {
        return false;
    }
    matches!(
        u32::from_be_bytes(magic),
        0xfeedfacf | 0xcffaedfe | 0xcafebabe | 0xbebafeca | 0xcafebabf | 0xbfbafeca
    )
}

fn take_string<'a>(bytes: &'a [u8], offset: &mut usize) -> Option<&'a [u8]> {
    let tail = bytes.get(*offset..)?;
    let end = tail.iter().position(|byte| *byte == 0)?;
    *offset += end + 1;
    Some(&tail[..end])
}

fn procargs_match_launch(bytes: &[u8], appid: u32, state_root: &[u8]) -> bool {
    let Some(raw) = bytes.get(..4).and_then(|bytes| bytes.try_into().ok()) else {
        return false;
    };
    let argc = i32::from_ne_bytes(raw);
    if !(1..=65_536).contains(&argc) {
        return false;
    }
    let mut offset = 4;
    if take_string(bytes, &mut offset).is_none_or(<[u8]>::is_empty) {
        return false;
    }
    while bytes.get(offset) == Some(&0) {
        offset += 1;
    }
    for _ in 0..argc {
        let Some(argument) = take_string(bytes, &mut offset) else {
            return false;
        };
        // Chromium/Electron renderer and utility children are not the game.
        if argument == b"--type" || argument.starts_with(b"--type=") {
            return false;
        }
    }
    // Preserve exact argc including empty arguments before skipping padding.
    while bytes.get(offset) == Some(&0) {
        offset += 1;
    }
    let expected_appid = appid.to_string();
    let mut appid_seen = false;
    let mut state_seen = false;
    while let Some(entry) = take_string(bytes, &mut offset) {
        if entry.is_empty() {
            return appid_seen && state_seen;
        }
        if let Some(value) = entry.strip_prefix(b"SteamAppId=") {
            if appid_seen || value != expected_appid.as_bytes() {
                return false;
            }
            appid_seen = true;
        } else if let Some(value) = entry.strip_prefix(b"SKINOS_RUNTIME_STATE_ROOT=") {
            if state_seen || value != state_root {
                return false;
            }
            state_seen = true;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kernel_tracks_own_background_fixture_after_script_exits() {
        use std::os::unix::process::CommandExt;
        use std::process::{Command, Stdio};
        use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

        struct Fixture {
            root: PathBuf,
            spawned: bool,
        }
        impl Drop for Fixture {
            fn drop(&mut self) {
                if self.spawned {
                    // Release only this fixture through its private marker;
                    // never signal a PID after a failed assertion.
                    let _ = std::fs::write(self.root.join("release"), b"done");
                    let deadline = Instant::now() + Duration::from_secs(5);
                    while !self.root.join("finished").exists() {
                        if Instant::now() > deadline {
                            // The child has its own watchdog. Keep its files
                            // intact rather than unlinking a running fixture.
                            return;
                        }
                        std::thread::sleep(Duration::from_millis(10));
                    }
                }
                let _ = std::fs::remove_dir_all(&self.root);
            }
        }
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let mut fixture = Fixture {
            root: std::env::temp_dir().join(format!(
                "skinos-script-group-{}-{nonce}",
                std::process::id()
            )),
            spawned: false,
        };
        let install = fixture.root.join("install");
        let state = fixture.root.join("private-state");
        std::fs::create_dir_all(&install).unwrap();
        std::fs::create_dir(&state).unwrap();
        let game = install.join("game");
        let source = fixture.root.join("game.c");
        // Use our own Mach-O, not an Apple platform binary whose environment
        // inspection can differ under SIP between macOS versions.
        std::fs::write(
            &source,
            r#"
#include <fcntl.h>
#include <unistd.h>
static int mark(const char *path) {
    int fd = open(path, O_WRONLY | O_CREAT | O_EXCL, 0600);
    if (fd < 0) return 1;
    return close(fd) != 0;
}
int main(int argc, char **argv) {
    if (argc != 4 || mark(argv[1])) return 10;
    for (int i = 0; i < 3000; ++i) {
        if (access(argv[2], F_OK) == 0) return mark(argv[3]);
        usleep(10000);
    }
    mark(argv[3]);
    return 11;
}
"#,
        )
        .unwrap();
        assert!(
            Command::new("/usr/bin/clang")
                .args(["-Wall", "-Wextra", "-Werror"])
                .arg(&source)
                .arg("-o")
                .arg(&game)
                .status()
                .unwrap()
                .success()
        );
        let ready = fixture.root.join("ready");
        let release = fixture.root.join("release");
        let finished = fixture.root.join("finished");
        let script = install.join("launch.sh");
        let pid_file = fixture.root.join("fixture.pid");
        std::fs::write(
            &script,
            b"#!/bin/sh\n\"$1\" \"$2\" \"$3\" \"$4\" &\nprintf '%s\\n' \"$!\" > \"$5\"\n",
        )
        .unwrap();
        let spawned_after = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_micros();
        let mut launcher = Command::new("/bin/sh")
            .arg(&script)
            .arg(&game)
            .arg(&ready)
            .arg(&release)
            .arg(&finished)
            .arg(&pid_file)
            .env("SteamAppId", "424242")
            .env("SKINOS_RUNTIME_STATE_ROOT", &state)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .process_group(0)
            .spawn()
            .unwrap();
        fixture.spawned = true;
        let launcher_pid = launcher.id() as i32;
        assert!(launcher.wait().unwrap().success());
        let game_pid: i32 = std::fs::read_to_string(&pid_file)
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while !ready.exists() {
            assert!(
                Instant::now() < deadline,
                "native fixture did not enter main"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        let tracker = ScriptTracker::capture(
            launcher_pid,
            launcher_pid,
            spawned_after,
            &install,
            424242,
            &state,
        )
        .unwrap();
        assert!(
            tracker.launcher.is_none(),
            "launcher must be reaped before this check"
        );
        // Reproduce a scheduling delay longer than the old fixture's lifetime.
        // Readiness and lifetime now have explicit handshakes, not a 1s race.
        std::thread::sleep(Duration::from_millis(1300));
        let deadline = Instant::now() + Duration::from_secs(5);
        let selected = loop {
            if let Some(process) = tracker.find_game().unwrap() {
                break process;
            }
            assert!(
                Instant::now() < deadline,
                "background fixture must remain identifiable after shell exit"
            );
            std::thread::sleep(Duration::from_millis(10));
        };
        assert_eq!(selected.pid, game_pid);
        assert_eq!(
            selected.executable.canonicalize().unwrap(),
            game.canonicalize().unwrap()
        );
    }

    fn process(pid: i32, parent: i32, seconds: u64) -> ProcessSnapshot {
        ProcessSnapshot {
            pid,
            parent,
            group: pid,
            start: (seconds, 0),
            zombie: false,
        }
    }

    fn tree() -> BTreeMap<i32, ProcessSnapshot> {
        [
            process(1, 0, 1),
            process(10, 1, 10),
            process(20, 10, 20),
            process(30, 20, 21),
            process(40, 30, 22),
            process(50, 10, 23),
        ]
        .into_iter()
        .map(|snapshot| (snapshot.pid, snapshot))
        .collect()
    }

    #[test]
    fn same_pid_exec_and_proven_descendants_keep_identity() {
        let tree = tree();
        let launcher = tree[&20];
        let worker = tree[&10];
        assert!(ancestry(20, &tree, launcher, worker).is_some());
        assert!(ancestry(40, &tree, launcher, worker).is_some());
        assert!(ancestry(50, &tree, launcher, worker).is_none());
    }

    #[test]
    fn launcher_must_be_created_after_request_by_current_worker() {
        let worker = process(10, 1, 10);
        let launcher = process(20, 10, 20);
        assert!(owned_launcher(launcher, worker, 20_000_000));
        assert!(!owned_launcher(launcher, worker, 20_000_001));
        assert!(!owned_launcher(process(20, 99, 20), worker, 0));
        assert!(!owned_launcher(worker, worker, 0));
    }

    #[test]
    fn multiple_real_game_candidates_are_ambiguous() {
        fn candidate(pid: i32) -> TrackedGameProcess {
            TrackedGameProcess {
                pid,
                start: (20, 0),
                executable: PathBuf::from("/Games/Game/game"),
                executable_identity: (1, 2),
            }
        }
        assert!(choose_candidate(Vec::new()).unwrap().is_none());
        assert_eq!(
            choose_candidate(vec![candidate(20)]).unwrap().unwrap().pid,
            20
        );
        assert!(choose_candidate(vec![candidate(20), candidate(30)]).is_err());
    }

    #[test]
    fn reused_or_reparented_processes_cannot_be_adopted() {
        let mut tree = tree();
        let launcher = tree[&20];
        let worker = tree[&10];
        tree.get_mut(&20).unwrap().start = (30, 0);
        assert!(ancestry(20, &tree, launcher, worker).is_none());
        assert!(ancestry(40, &tree, launcher, worker).is_none());
        tree.insert(20, launcher);
        tree.get_mut(&30).unwrap().start = (30, 0);
        assert!(ancestry(40, &tree, launcher, worker).is_none());
        tree.insert(30, process(30, 1, 21));
        assert!(ancestry(40, &tree, launcher, worker).is_none());
    }

    #[test]
    fn orphaned_child_is_found_only_in_the_proven_adapter_group() {
        let mut tree = tree();
        let launcher = tree[&20];
        let worker = tree[&10];
        tree.remove(&20);
        let orphan = tree.get_mut(&40).unwrap();
        orphan.parent = 1;
        orphan.group = 20;
        // Both cases are valid: we observed the launcher, or it had already
        // exited before capture and only the adapter's group proof remains.
        assert!(launch_origin(40, &tree, Some(launcher), 20, worker, 20_000_000).is_some());
        assert!(launch_origin(40, &tree, None, 20, worker, 20_000_000).is_some());
        tree.get_mut(&40).unwrap().group = 99;
        assert!(launch_origin(40, &tree, None, 20, worker, 20_000_000).is_none());
        tree.get_mut(&40).unwrap().group = 20;
        tree.get_mut(&40).unwrap().start = (19, 0);
        assert!(launch_origin(40, &tree, None, 20, worker, 20_000_000).is_none());
    }

    #[test]
    fn reused_group_leader_and_stale_worker_invalidate_group_proof() {
        let mut tree = tree();
        let launcher = tree[&20];
        let worker = tree[&10];
        tree.get_mut(&40).unwrap().group = 20;
        assert!(launch_origin(40, &tree, Some(launcher), 20, worker, 20_000_000).is_some());
        tree.get_mut(&20).unwrap().start = (30, 0);
        assert!(launch_origin(40, &tree, Some(launcher), 20, worker, 20_000_000).is_none());
        assert!(launch_origin(40, &tree, None, 20, worker, 20_000_000).is_none());
        tree.remove(&20);
        tree.get_mut(&10).unwrap().start = (30, 0);
        assert!(launch_origin(40, &tree, None, 20, worker, 20_000_000).is_none());
    }

    #[test]
    fn zombie_group_leader_does_not_hide_a_live_child() {
        let mut tree = tree();
        let launcher = tree[&20];
        let worker = tree[&10];
        tree.get_mut(&20).unwrap().zombie = true;
        tree.get_mut(&40).unwrap().group = 20;
        assert!(launch_origin(40, &tree, Some(launcher), 20, worker, 20_000_000).is_some());
        assert!(launch_origin(20, &tree, Some(launcher), 20, worker, 20_000_000).is_none());
    }

    #[test]
    fn zombies_and_ancestry_cycles_fail_closed() {
        let mut tree = tree();
        let launcher = tree[&20];
        let worker = tree[&10];
        tree.get_mut(&40).unwrap().zombie = true;
        assert!(ancestry(40, &tree, launcher, worker).is_none());
        tree.get_mut(&40).unwrap().zombie = false;
        tree.get_mut(&30).unwrap().parent = 40;
        tree.get_mut(&30).unwrap().start = (22, 0);
        assert!(ancestry(40, &tree, launcher, worker).is_none());
    }

    fn procargs(arguments: &[&[u8]], environment: &[&[u8]]) -> Vec<u8> {
        let mut bytes = (arguments.len() as i32).to_ne_bytes().to_vec();
        bytes.extend_from_slice(b"/Games/Game/game\0\0\0");
        for argument in arguments {
            bytes.extend_from_slice(argument);
            bytes.push(0);
        }
        bytes.extend_from_slice(&[0, 0]);
        for entry in environment {
            bytes.extend_from_slice(entry);
            bytes.push(0);
        }
        bytes.push(0);
        bytes
    }

    #[test]
    fn exact_environment_is_required_and_argv_cannot_spoof_it() {
        let env: &[&[u8]] = &[
            b"SteamAppId=42",
            b"SKINOS_RUNTIME_STATE_ROOT=/launch/unique",
        ];
        let bytes = procargs(&[b"game", b""], env);
        assert!(procargs_match_launch(&bytes, 42, b"/launch/unique"));
        assert!(!procargs_match_launch(&bytes, 43, b"/launch/unique"));
        assert!(!procargs_match_launch(&bytes, 42, b"/other/launch"));
        assert!(!procargs_match_launch(
            &procargs(env, &[]),
            42,
            b"/launch/unique"
        ));
        assert!(!procargs_match_launch(
            &procargs(&[b"game"], &[env[0], env[1], env[0]]),
            42,
            b"/launch/unique"
        ));
        assert!(!procargs_match_launch(
            &procargs(&[b"game", b"--type=renderer"], env),
            42,
            b"/launch/unique"
        ));
    }

    #[test]
    fn outside_install_shells_and_helpers_are_not_game_candidates() {
        let root = Path::new("/Games/Game");
        assert!(native_game_path(
            Path::new("/Games/Game/Game.app/Contents/MacOS/Game"),
            root
        ));
        for path in [
            "/Games/Other/game",
            "/Games/Game2/game",
            "/Games/Game/sh",
            "/Games/Game/UnityCrashHandler64",
            "/Games/Game/Game Helper.app/Contents/MacOS/crashpad_handler",
            "/Games/Game/Engine.framework/engine",
            "/Games/Game/Renderer.xpc/renderer",
            "/Games/Game/updater",
            "/Games/Game/Game Helper",
        ] {
            assert!(!native_game_path(Path::new(path), root), "{path}");
        }
    }
}
