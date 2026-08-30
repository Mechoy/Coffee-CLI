//! Exact, bounded cleanup for process trees spawned by Coffee's PTYs.
//!
//! A PTY foreground process group is not a sufficient ownership boundary:
//! Agent CLIs may place stdio MCP children in separate process groups.  This
//! module therefore snapshots the verified descendant tree and the PTY child
//! session, identifies every process by `(pid, start_time)`, and signals only
//! identities that still match immediately before delivery.

#[cfg(unix)]
mod unix {
    use std::collections::{BTreeMap, BTreeSet, VecDeque};
    use std::time::{Duration, Instant};

    const POLL_INTERVAL: Duration = Duration::from_millis(50);
    const FORCE_WAIT: Duration = Duration::from_millis(500);

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    struct ProcessIdentity {
        pid: libc::pid_t,
        ppid: libc::pid_t,
        started: u128,
        zombie: bool,
    }

    impl ProcessIdentity {
        fn still_matches(self) -> bool {
            process_identity(self.pid)
                .is_some_and(|current| current.started == self.started && !current.zombie)
        }
    }

    #[derive(Debug)]
    pub struct ManagedProcessTree {
        root: ProcessIdentity,
        session_id: libc::pid_t,
    }

    #[derive(Debug, Clone)]
    pub struct TerminationReport {
        pub root_pid: u32,
        pub targeted: usize,
        pub forced: usize,
        pub remaining: Vec<u32>,
    }

    impl ManagedProcessTree {
        pub fn capture(root_pid: u32) -> Option<Self> {
            let root_pid = libc::pid_t::try_from(root_pid).ok()?;
            if root_pid <= 1 || root_pid == unsafe { libc::getpid() } {
                return None;
            }
            let root = process_identity(root_pid)?;
            let session_id = unsafe { libc::getsid(root_pid) };
            if session_id <= 1 || session_id == unsafe { libc::getsid(0) } {
                return None;
            }
            Some(Self { root, session_id })
        }

        pub fn terminate(&self, grace: Duration) -> TerminationReport {
            let mut known = BTreeMap::<libc::pid_t, ProcessIdentity>::new();
            self.merge_owned_processes(&mut known, true);
            if self.root.still_matches() {
                known.insert(self.root.pid, self.root);
            }

            let mut signalled_term = BTreeSet::new();
            signal_new(&known, &mut signalled_term, libc::SIGTERM, self.root.pid);

            let deadline = Instant::now() + grace;
            while Instant::now() < deadline {
                if !self.root.still_matches() && alive(&known).is_empty() {
                    break;
                }
                std::thread::sleep(POLL_INTERVAL);
                // Continue from every attributed live identity so descendants
                // created while the original root exits are still discovered.
                // A full session scan is repeated before escalation.
                self.merge_owned_processes(&mut known, false);
                signal_new(&known, &mut signalled_term, libc::SIGTERM, self.root.pid);
                if alive(&known).is_empty() {
                    break;
                }
            }

            self.merge_owned_processes(&mut known, true);
            signal_new(&known, &mut signalled_term, libc::SIGTERM, self.root.pid);

            let mut signalled_kill = BTreeSet::new();
            let force_deadline = Instant::now() + FORCE_WAIT;
            loop {
                // A TERM handler can still fork while its original root is
                // exiting. Continue walking every verified identity already
                // attributed to this run, including children that changed
                // process group or terminal session.
                self.merge_owned_processes(&mut known, true);
                signal_new(&known, &mut signalled_kill, libc::SIGKILL, self.root.pid);
                if alive(&known).is_empty() || Instant::now() >= force_deadline {
                    break;
                }
                std::thread::sleep(POLL_INTERVAL);
            }
            let remaining = alive(&known)
                .into_iter()
                .filter_map(|identity| u32::try_from(identity.pid).ok())
                .collect();

            TerminationReport {
                root_pid: self.root.pid as u32,
                targeted: known.len(),
                forced: signalled_kill.len(),
                remaining,
            }
        }

        fn merge_owned_processes(
            &self,
            known: &mut BTreeMap<libc::pid_t, ProcessIdentity>,
            include_session_scan: bool,
        ) {
            let mut sources = alive(known);
            if self.root.still_matches()
                && !sources.iter().any(|identity| identity.pid == self.root.pid)
            {
                sources.push(self.root);
            }
            for source in sources {
                for identity in descendant_tree(source) {
                    known.entry(identity.pid).or_insert(identity);
                }
            }
            if include_session_scan {
                for pid in all_pids() {
                    if pid <= 1 || unsafe { libc::getsid(pid) } != self.session_id {
                        continue;
                    }
                    if let Some(identity) = process_identity(pid) {
                        known.entry(pid).or_insert(identity);
                    }
                }
            }
        }
    }

    fn descendant_tree(root: ProcessIdentity) -> Vec<ProcessIdentity> {
        if !root.still_matches() {
            return Vec::new();
        }
        let mut result = vec![root];
        let mut seen = BTreeSet::from([root.pid]);
        let mut queue = VecDeque::from([root.pid]);
        while let Some(parent) = queue.pop_front() {
            for pid in immediate_children(parent) {
                if !seen.insert(pid) {
                    continue;
                }
                let Some(identity) = process_identity(pid) else {
                    continue;
                };
                if identity.ppid != parent {
                    continue;
                }
                result.push(identity);
                queue.push_back(pid);
            }
        }
        result
    }

    fn alive(known: &BTreeMap<libc::pid_t, ProcessIdentity>) -> Vec<ProcessIdentity> {
        known
            .values()
            .copied()
            .filter(|identity| identity.still_matches())
            .collect()
    }

    fn signal_new(
        known: &BTreeMap<libc::pid_t, ProcessIdentity>,
        signalled: &mut BTreeSet<libc::pid_t>,
        signal: libc::c_int,
        root_pid: libc::pid_t,
    ) {
        let identities: Vec<_> = known
            .values()
            .copied()
            .filter(|identity| identity.still_matches())
            .filter(|identity| signalled.insert(identity.pid))
            .collect();
        signal_identities(&identities, signal, root_pid);
    }

    fn signal_identities(
        identities: &[ProcessIdentity],
        signal: libc::c_int,
        root_pid: libc::pid_t,
    ) {
        // Descendants first keeps the parent relationship available until the
        // full target snapshot has been signalled. The root is always last.
        for identity in identities
            .iter()
            .filter(|identity| identity.pid != root_pid)
            .chain(
                identities
                    .iter()
                    .filter(|identity| identity.pid == root_pid),
            )
        {
            if identity.pid <= 1
                || identity.pid == unsafe { libc::getpid() }
                || !identity.still_matches()
            {
                continue;
            }
            unsafe {
                libc::kill(identity.pid, signal);
            }
        }
    }

    #[cfg(target_os = "macos")]
    #[repr(C)]
    #[derive(Clone, Copy)]
    struct ProcBsdInfo {
        flags: u32,
        status: u32,
        xstatus: u32,
        pid: u32,
        ppid: u32,
        uid: libc::uid_t,
        gid: libc::gid_t,
        ruid: libc::uid_t,
        rgid: libc::gid_t,
        svuid: libc::uid_t,
        svgid: libc::gid_t,
        reserved: u32,
        comm: [libc::c_char; 16],
        name: [libc::c_char; 32],
        nfiles: u32,
        pgid: u32,
        pjobc: u32,
        tty_device: u32,
        tty_foreground_pgid: u32,
        nice: i32,
        start_seconds: u64,
        start_microseconds: u64,
    }

    #[cfg(target_os = "macos")]
    #[link(name = "proc")]
    extern "C" {
        fn proc_listallpids(buffer: *mut libc::c_void, buffer_size: libc::c_int) -> libc::c_int;
        fn proc_listchildpids(
            parent: libc::pid_t,
            buffer: *mut libc::c_void,
            buffer_size: libc::c_int,
        ) -> libc::c_int;
        fn proc_pidinfo(
            pid: libc::c_int,
            flavor: libc::c_int,
            arg: u64,
            buffer: *mut libc::c_void,
            buffer_size: libc::c_int,
        ) -> libc::c_int;
    }

    #[cfg(target_os = "macos")]
    fn process_identity(pid: libc::pid_t) -> Option<ProcessIdentity> {
        const PROC_PIDTBSDINFO: libc::c_int = 3;
        const SZOMB: u32 = 5;
        let mut info = std::mem::MaybeUninit::<ProcBsdInfo>::zeroed();
        let size = std::mem::size_of::<ProcBsdInfo>();
        let written = unsafe {
            proc_pidinfo(
                pid,
                PROC_PIDTBSDINFO,
                0,
                info.as_mut_ptr().cast(),
                size as libc::c_int,
            )
        };
        if written != size as libc::c_int {
            return None;
        }
        let info = unsafe { info.assume_init() };
        Some(ProcessIdentity {
            pid: libc::pid_t::try_from(info.pid).ok()?,
            ppid: libc::pid_t::try_from(info.ppid).ok()?,
            started: (u128::from(info.start_seconds) << 64) | u128::from(info.start_microseconds),
            zombie: info.status == SZOMB,
        })
    }

    #[cfg(target_os = "macos")]
    fn immediate_children(parent: libc::pid_t) -> Vec<libc::pid_t> {
        list_macos_pids(|buffer, size| unsafe { proc_listchildpids(parent, buffer, size) })
    }

    #[cfg(target_os = "macos")]
    fn all_pids() -> Vec<libc::pid_t> {
        list_macos_pids(|buffer, size| unsafe { proc_listallpids(buffer, size) })
    }

    #[cfg(target_os = "macos")]
    fn list_macos_pids(
        list: impl Fn(*mut libc::c_void, libc::c_int) -> libc::c_int,
    ) -> Vec<libc::pid_t> {
        let required = list(std::ptr::null_mut(), 0);
        if required <= 0 {
            return Vec::new();
        }
        // libproc list APIs report a PID count. Leave spare slots for forks
        // between the sizing and fill calls; zero entries are filtered below.
        let mut pids = vec![0; required as usize + 32];
        let filled = list(
            pids.as_mut_ptr().cast(),
            (pids.len() * std::mem::size_of::<libc::pid_t>()) as libc::c_int,
        );
        if filled <= 0 {
            return Vec::new();
        }
        pids.truncate((filled as usize).min(pids.len()));
        pids.into_iter().filter(|pid| *pid > 1).collect()
    }

    #[cfg(target_os = "linux")]
    fn process_identity(pid: libc::pid_t) -> Option<ProcessIdentity> {
        let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
        parse_linux_stat(pid, &stat)
    }

    #[cfg(target_os = "linux")]
    fn parse_linux_stat(pid: libc::pid_t, stat: &str) -> Option<ProcessIdentity> {
        let tail = stat.get(stat.rfind(')')? + 1..)?.trim();
        let fields: Vec<_> = tail.split_whitespace().collect();
        if fields.len() <= 19 {
            return None;
        }
        Some(ProcessIdentity {
            pid,
            ppid: fields[1].parse().ok()?,
            started: fields[19].parse::<u128>().ok()?,
            zombie: fields[0] == "Z",
        })
    }

    #[cfg(target_os = "linux")]
    fn immediate_children(parent: libc::pid_t) -> Vec<libc::pid_t> {
        std::fs::read_to_string(format!("/proc/{parent}/task/{parent}/children"))
            .ok()
            .map(|children| {
                children
                    .split_whitespace()
                    .filter_map(|pid| pid.parse().ok())
                    .collect()
            })
            .unwrap_or_default()
    }

    #[cfg(target_os = "linux")]
    fn all_pids() -> Vec<libc::pid_t> {
        std::fs::read_dir("/proc")
            .ok()
            .into_iter()
            .flatten()
            .filter_map(Result::ok)
            .filter_map(|entry| entry.file_name().to_string_lossy().parse().ok())
            .collect()
    }

    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    fn process_identity(pid: libc::pid_t) -> Option<ProcessIdentity> {
        (pid > 1 && unsafe { libc::kill(pid, 0) } == 0).then_some(ProcessIdentity {
            pid,
            ppid: 0,
            started: 0,
            zombie: false,
        })
    }

    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    fn immediate_children(_parent: libc::pid_t) -> Vec<libc::pid_t> {
        Vec::new()
    }

    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    fn all_pids() -> Vec<libc::pid_t> {
        Vec::new()
    }

    #[cfg(all(test, target_os = "linux"))]
    mod linux_tests {
        use super::parse_linux_stat;

        #[test]
        fn parses_parent_and_start_time_after_parenthesized_command() {
            let identity = parse_linux_stat(
                42,
                "42 (command with spaces) S 7 42 42 0 -1 0 0 0 0 0 0 0 0 0 0 20 0 1 0 123456 0",
            )
            .unwrap();
            assert_eq!(identity.ppid, 7);
            assert_eq!(identity.started, 123456);
        }
    }

    #[cfg(test)]
    mod process_tree_tests {
        use super::ManagedProcessTree;
        use std::time::Duration;

        struct FixtureGuard {
            root: libc::pid_t,
            child: libc::pid_t,
        }

        impl Drop for FixtureGuard {
            fn drop(&mut self) {
                unsafe {
                    libc::kill(self.child, libc::SIGKILL);
                    libc::kill(self.root, libc::SIGKILL);
                    libc::waitpid(self.root, std::ptr::null_mut(), 0);
                }
            }
        }

        #[test]
        fn terminates_descendant_in_a_separate_process_group() {
            let mut fds = [0; 2];
            assert_eq!(unsafe { libc::pipe(fds.as_mut_ptr()) }, 0);

            let root = unsafe { libc::fork() };
            assert!(root >= 0);
            if root == 0 {
                unsafe {
                    libc::close(fds[0]);
                    if libc::setsid() < 0 {
                        libc::_exit(101);
                    }
                    let child = libc::fork();
                    if child < 0 {
                        libc::_exit(102);
                    }
                    if child == 0 {
                        libc::setpgid(0, 0);
                        libc::signal(libc::SIGTERM, libc::SIG_IGN);
                        let pid = libc::getpid();
                        let _ = libc::write(
                            fds[1],
                            (&pid as *const libc::pid_t).cast(),
                            std::mem::size_of::<libc::pid_t>(),
                        );
                        loop {
                            libc::pause();
                        }
                    }
                    libc::signal(libc::SIGTERM, libc::SIG_IGN);
                    libc::close(fds[1]);
                    loop {
                        libc::pause();
                    }
                }
            }

            unsafe { libc::close(fds[1]) };
            let mut child = 0;
            let read = unsafe {
                libc::read(
                    fds[0],
                    (&mut child as *mut libc::pid_t).cast(),
                    std::mem::size_of::<libc::pid_t>(),
                )
            };
            unsafe { libc::close(fds[0]) };
            assert_eq!(read as usize, std::mem::size_of::<libc::pid_t>());
            let guard = FixtureGuard { root, child };

            let tree = (0..100)
                .find_map(|_| {
                    let tree = ManagedProcessTree::capture(root as u32);
                    if tree.is_none() {
                        std::thread::sleep(Duration::from_millis(5));
                    }
                    tree
                })
                .expect("fixture root should establish its own terminal session");
            assert_ne!(unsafe { libc::getpgid(root) }, unsafe {
                libc::getpgid(child)
            });

            let report = tree.terminate(Duration::from_millis(150));
            assert!(report.targeted >= 2, "report: {report:?}");
            assert!(report.forced >= 2, "report: {report:?}");
            assert!(report.remaining.is_empty(), "report: {report:?}");
            drop(guard);
        }
    }
}

#[cfg(unix)]
pub use unix::ManagedProcessTree;
