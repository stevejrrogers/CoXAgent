//! COX-B015 regression guard: the command shims must never rewrite the output
//! of a `git` content-retrieval subcommand.
//!
//! The bug: every shim piped non-tty output through `coxagent compress`, which
//! dedupes repeated lines and elides the middle of anything over 6000 chars.
//! For `git show <rev>:<path>` that silently deleted whole function bodies from
//! the file an agent was reading — `git status` reported no changes, so nothing
//! signalled the corruption. Porcelain output only stayed exact because it is
//! small enough to hit the passthrough, not because `git` was special-cased.
//!
//! These tests drive the real `coxagent` binary the shim script invokes, with
//! the real argv the shim forwards, over real repository content.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::fmt::Write as _;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// Big enough that `clip_middle` (6000 chars) would elide a middle chunk, so a
/// passing test proves the bypass rather than the small-output passthrough.
const MIN_BLOB_BYTES: u64 = 8_000;

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap()
}

/// The true `git` output, taken with the shims disabled — this is the oracle
/// the shimmed output must match byte for byte. `COX_COMPRESS=0` makes any shim
/// that happens to be on `PATH` `exec` the real binary instead of piping it.
fn git(root: &Path, args: &[&str]) -> Vec<u8> {
    let out = Command::new("git")
        .args(args)
        .current_dir(root)
        .env("COX_COMPRESS", "0")
        .output()
        .unwrap_or_else(|e| panic!("`git {}` failed to spawn: {e}", args.join(" ")));
    assert!(
        out.status.success(),
        "`git {}` failed ({}):\n{}",
        args.join(" "),
        out.status,
        String::from_utf8_lossy(&out.stderr)
    );
    out.stdout
}

/// A tracked Rust file whose HEAD blob is large enough to be compressed.
/// Chosen from the tree rather than hard-coded so the guard survives the file
/// in the original bug report being renamed or shrunk.
fn large_tracked_file(root: &Path) -> String {
    let listing = git(root, &["ls-tree", "-r", "-l", "HEAD"]);
    String::from_utf8(listing)
        .unwrap()
        .lines()
        .find_map(|l| {
            let (meta, path) = l.split_once('\t')?;
            let size: u64 = meta.split_whitespace().nth(3)?.parse().ok()?;
            let is_rust = Path::new(path).extension().is_some_and(|e| e == "rs");
            (size >= MIN_BLOB_BYTES && is_rust).then(|| path.to_owned())
        })
        .expect("repo has no tracked .rs blob over the compression threshold")
}

/// Run `coxagent compress --cmd <cmd> -- <args>` over `input`, exactly as the
/// generated shim script pipes a wrapped command's output into it.
fn compress(cmd: &str, args: &[&str], input: &[u8]) -> Vec<u8> {
    let mut child = Command::new(env!("CARGO_BIN_EXE_coxagent"))
        .arg("compress")
        .args(["--cmd", cmd])
        .arg("--")
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("failed to spawn the coxagent binary");
    // Feed stdin from its own thread. A payload larger than the pipe buffer
    // (64 KiB on Linux/macOS) deadlocks otherwise: the child blocks writing
    // stdout while the parent blocks writing stdin. The real shim has the same
    // shape, so a single-threaded helper would only ever test small inputs.
    let mut sink = child.stdin.take().unwrap();
    let payload = input.to_vec();
    let writer = std::thread::spawn(move || {
        sink.write_all(&payload).unwrap();
        drop(sink);
    });
    let out = child.wait_with_output().unwrap();
    writer.join().unwrap();
    assert!(out.status.success(), "`coxagent compress` failed: {out:?}");
    out.stdout
}

/// Byte-for-byte comparison that reports *where* the output diverged instead of
/// dumping two multi-megabyte byte vectors into the test log.
fn assert_byte_exact(label: &str, real: &[u8], shimmed: &[u8]) {
    if real == shimmed {
        return;
    }
    let at = real
        .iter()
        .zip(shimmed)
        .position(|(a, b)| a != b)
        .unwrap_or_else(|| real.len().min(shimmed.len()));
    #[allow(clippy::naive_bytecount)] // a panic message, not a hot path
    let lines = |b: &[u8]| b.iter().filter(|c| **c == b'\n').count();
    panic!(
        "`{label}` was altered by the shim: {} bytes / {} lines in, {} bytes / {} lines out, \
         first difference at byte {at}",
        real.len(),
        lines(real),
        shimmed.len(),
        lines(shimmed),
    );
}

#[test]
fn git_show_through_the_shim_returns_the_file_byte_exact() {
    let root = repo_root();
    let path = large_tracked_file(&root);
    let rev = format!("HEAD:{path}");
    let real = git(&root, &["show", &rev]);

    let shimmed = compress("git", &["show", &rev], &real);

    assert_byte_exact(&format!("git show {rev}"), &real, &shimmed);
}

#[test]
fn git_diff_and_log_p_are_byte_exact_too() {
    let root = repo_root();
    // A patch big enough to trip the compressor: the whole file added.
    let path = large_tracked_file(&root);
    for args in [
        vec![
            "diff",
            "--no-color",
            "4b825dc642cb6eb9a060e54bf8d69288fbee4904",
            "HEAD",
            "--",
            &path,
        ],
        vec!["log", "--no-color", "-p", "--", &path],
    ] {
        let real = git(&root, &args);
        assert!(
            real.len() > 6_000,
            "`git {}` produced only {} bytes — too small to exercise clipping",
            args.join(" "),
            real.len()
        );
        let shimmed = compress("git", &args, &real);
        assert_byte_exact(&format!("git {}", args.join(" ")), &real, &shimmed);
    }
}

#[test]
fn binary_blob_content_survives_the_shim_intact() {
    // `git show HEAD:logo.png`, `git cat-file -p <blob>` and `git archive` all
    // emit bytes that are not valid UTF-8. Reading stdin as a `String` rejected
    // them outright and produced an empty result — corruption worse than the
    // clipping this ticket is about, on the very subcommands the fix claims to
    // keep byte-exact.
    let blob: Vec<u8> = (0..=255u8).cycle().take(40_000).collect();
    assert!(
        std::str::from_utf8(&blob).is_err(),
        "payload must be binary"
    );

    for args in [
        vec!["show", "HEAD:assets/logo.png"],
        vec!["cat-file", "-p", "f7f19a4"],
        vec!["archive", "HEAD"],
    ] {
        let out = compress("git", &args, &blob);
        assert_byte_exact(&format!("git {}", args.join(" ")), &blob, &out);
    }
}

#[test]
fn binary_output_from_other_commands_is_not_mangled_either() {
    // Compression is line/char based, so it cannot round-trip bytes at all —
    // a non-UTF-8 payload must pass through whatever command produced it.
    let blob: Vec<u8> = (0..=255u8).cycle().take(40_000).collect();
    let out = compress("docker", &["save", "img"], &blob);
    assert_byte_exact("docker save img", &blob, &out);
}

#[test]
fn exact_output_larger_than_the_pipe_buffer_is_not_truncated() {
    // The shim streams through two pipes with a 64 KiB buffer each. A file
    // bigger than that is where a naive read-then-write loses the tail, and it
    // is the realistic size for the `git log -p` / `git archive` calls agents
    // make. 4 MiB clears the buffer by two orders of magnitude.
    let root = repo_root();
    let unit = git(
        &root,
        &["show", &format!("HEAD:{}", large_tracked_file(&root))],
    );
    let mut real = Vec::with_capacity(4 << 20);
    while real.len() < (4 << 20) {
        real.extend_from_slice(&unit);
    }

    let shimmed = compress("git", &["show", "HEAD:big.rs"], &real);

    assert_byte_exact("git show (4 MiB)", &real, &shimmed);
}

/// One line of the fake binary's stdout (newline added when emitted). Repeated
/// so compression would collapse it to a single `(×N)` line — if the payload
/// survives intact, no compressor touched it.
const FAKE_LINE: &str = "warning: unused variable";
const FAKE_LINES: usize = 500;
/// What the fake `git` writes to *stderr*. Real `git show` does the same
/// (`warning: …`, `fatal: …`) while still succeeding or failing usefully.
const FAKE_DIAGNOSTIC: &str = "git-stderr-diagnostic";
const FAKE_EXIT: i32 = 3;

/// The real shim script plus a fake `git` for it to find, wired so the shim
/// resolves the fake instead of the system binary.
///
/// The fake is deterministic across git versions and platforms: what is under
/// test is the *shim's* stream plumbing, not git's. `shim_script` is imported
/// from the crate rather than copied so this guard cannot drift away from the
/// script actually shipped.
struct Shimmed {
    _dir: tempfile::TempDir,
    shim: PathBuf,
    path: std::ffi::OsString,
}

impl Shimmed {
    fn new() -> Self {
        Self::with_cmd_and_exe("git", env!("CARGO_BIN_EXE_coxagent"))
    }

    /// A shim for `cmd` whose baked compress binary is `exe` — pass a
    /// nonexistent path to reproduce CXA-B109's purged-worktree state. The
    /// fake binary ignores its argv, so one fake serves every subcommand.
    fn with_cmd_and_exe(cmd: &str, exe: &str) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let shim_dir = dir.path().join("shims");
        let fake_dir = dir.path().join("bin");
        std::fs::create_dir_all(&shim_dir).unwrap();
        std::fs::create_dir_all(&fake_dir).unwrap();

        let shim = shim_dir.join(cmd);
        write_exec(
            &shim,
            &coxagent_app::shim_script(cmd, &shim_dir.display().to_string(), exe),
        );
        write_exec(
            &fake_dir.join(cmd),
            &format!(
                "#!/usr/bin/env bash\n\
                 for ((i=0;i<{FAKE_LINES};i++)); do printf '%s\\n' '{FAKE_LINE}'; done\n\
                 printf '%s\\n' '{FAKE_DIAGNOSTIC}' >&2\n\
                 exit {FAKE_EXIT}\n"
            ),
        );

        // The fake must precede the system `git`; `/usr/bin:/bin` stays on the
        // tail because the shim's `#!/usr/bin/env bash` resolves `bash` there.
        let path = format!(
            "{}:{}:/usr/bin:/bin",
            shim_dir.display(),
            fake_dir.display()
        );
        Self {
            _dir: dir,
            shim,
            path: path.into(),
        }
    }

    /// Invoke the shim exactly as an agent's shell would, with stdout and
    /// stderr on separate pipes (so neither is a tty, and a merge is visible).
    fn run(&self, args: &[&str]) -> std::process::Output {
        Command::new(&self.shim)
            .args(args)
            .env("PATH", &self.path)
            .env_remove("COX_COMPRESS")
            .stdin(Stdio::null())
            .output()
            .expect("failed to run the shim")
    }
}

fn write_exec(path: &Path, body: &str) {
    std::fs::write(path, body).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
}

fn fake_stdout() -> Vec<u8> {
    format!("{FAKE_LINE}\n").repeat(FAKE_LINES).into_bytes()
}

fn fake_stderr() -> Vec<u8> {
    format!("{FAKE_DIAGNOSTIC}\n").into_bytes()
}

/// COX-B015 regression guard, at the shim level rather than the `compress`
/// level: the pipeline merged the wrapped command's stderr into its stdout
/// (`2>&1`) before compressing. For a content-retrieval `git` subcommand that
/// merge *is* the corruption this ticket is about — a `warning:`/`fatal:` line
/// git wrote to stderr is spliced into the file content the agent reads, and
/// the caller's stderr comes back empty. Fails on pre-fix code.
#[test]
fn the_real_shim_leaves_git_show_stdout_stderr_and_exit_code_native() {
    let sh = Shimmed::new();

    let out = sh.run(&["show", "HEAD:big.rs"]);

    assert_byte_exact("git show stdout", &fake_stdout(), &out.stdout);
    assert_byte_exact("git show stderr", &fake_stderr(), &out.stderr);
    assert_eq!(
        out.status.code(),
        Some(FAKE_EXIT),
        "the shim did not pass the wrapped command's exit code through"
    );
}

/// The same guard for the other subcommands named in the ticket, including one
/// reached past a global value flag (`git -C <dir> diff`).
#[test]
fn the_real_shim_leaves_every_content_subcommand_native() {
    let sh = Shimmed::new();
    for args in [
        vec!["diff", "HEAD"],
        vec!["cat-file", "-p", "abc123"],
        vec!["log", "-p"],
        vec!["-C", "/somewhere", "diff"],
    ] {
        let out = sh.run(&args);
        let label = format!("git {}", args.join(" "));
        assert_byte_exact(&format!("{label} stdout"), &fake_stdout(), &out.stdout);
        assert_byte_exact(&format!("{label} stderr"), &fake_stderr(), &out.stderr);
        assert_eq!(out.status.code(), Some(FAKE_EXIT), "{label}: exit code");
    }
}

/// COX-B015: a global flag that takes a *separate* value hides the subcommand
/// behind its value. Every such flag must be skipped with its value, or the
/// value is read as the subcommand, no content subcommand matches, and the
/// file the agent asked for comes back deduped and clipped. The fake `git`
/// emits ~12 KB, twice the 6000-char clip threshold, so a byte-exact result
/// can only come from the exactness bypass — never from the small-output
/// passthrough. Fails on pre-fix code for `--super-prefix`/`--config-env`.
#[test]
fn the_real_shim_sees_past_every_global_value_flag() {
    let sh = Shimmed::new();
    assert!(
        fake_stdout().len() > 6_000,
        "fake payload must exceed the clip threshold to prove the bypass"
    );
    for args in [
        vec!["-C", "/repo", "show", "HEAD:big.rs"],
        vec!["-c", "core.pager=cat", "show", "HEAD:big.rs"],
        vec!["--git-dir", "/repo/.git", "diff", "HEAD"],
        vec!["--work-tree", "/repo", "diff", "HEAD"],
        vec!["--namespace", "ns", "log", "-p"],
        vec!["--super-prefix", "sub/", "show", "HEAD:big.rs"],
        vec![
            "--config-env",
            "core.pager=PAGER_ENV",
            "cat-file",
            "-p",
            "abc",
        ],
    ] {
        let out = sh.run(&args);
        let label = format!("git {}", args.join(" "));
        assert_byte_exact(&format!("{label} stdout"), &fake_stdout(), &out.stdout);
        assert_byte_exact(&format!("{label} stderr"), &fake_stderr(), &out.stderr);
        assert_eq!(out.status.code(), Some(FAKE_EXIT), "{label}: exit code");
    }
}

/// The other half of the fix (AC5): a non-content subcommand still gets the
/// full token saving — same merge, same compression, same exit code as before.
#[test]
fn the_real_shim_still_compresses_a_non_exact_subcommand() {
    let sh = Shimmed::new();

    let out = sh.run(&["status"]);

    let text = String::from_utf8_lossy(&out.stdout);
    assert!(
        text.contains("output compressed"),
        "`git status` was not compressed: {text:.200}"
    );
    assert!(
        text.contains(&format!("(×{FAKE_LINES})")),
        "`git status` output was not deduped: {text:.200}"
    );
    assert!(out.stdout.len() < fake_stdout().len());
    // Unchanged behaviour: stderr is still folded into the compressed stream,
    // which is where cargo/npm/git-porcelain diagnostics belong.
    assert!(text.contains(FAKE_DIAGNOSTIC));
    assert_eq!(out.status.code(), Some(FAKE_EXIT));
}

/// The ticket's own repro, with nothing faked: a repository this test builds,
/// the system `git`, and the real generated shim wired together. The other
/// guards each pin one half — `compress` fed real git output, or the real shim
/// script fed a fake `git` — so a break in how the two *compose* (the shim
/// resolving the wrong binary, the probe mis-reading a real argv, the wrapped
/// exit code lost on the bypass) passes both and still corrupts every file an
/// agent reads. This is the step list in COX-B015, executed.
const REPRO_LINES: usize = 3_000;

/// The empty tree, so `git diff <empty> HEAD` yields the whole file as a patch.
const EMPTY_TREE: &str = "4b825dc642cb6eb9a060e54bf8d69288fbee4904";

/// The directory holding the real `git`, skipping any shim directory that
/// happens to be on the ambient `PATH` (an agent shell has one). CXA-B109 made
/// the directory name pid-suffixed (`coxagent-shims-<pid>`), so match the
/// prefix every instance shares instead of one exact name.
fn is_shim_dir(d: &Path) -> bool {
    d.file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|n| n.starts_with("coxagent-shims"))
}

fn real_git_dir() -> PathBuf {
    let path = std::env::var_os("PATH").expect("PATH is unset");
    std::env::split_paths(&path)
        .find(|d| !is_shim_dir(d) && d.join("git").is_file())
        .expect("no real `git` found on PATH")
}

/// Isolate every `git` run from the host's global/system config, so a user's
/// pager, alias or `commit.gpgsign` cannot change what the oracle and the
/// shimmed run produce — the comparison is only meaningful if the two differ
/// in the shim alone.
fn plain_git(cwd: &Path, args: &[&str]) -> std::process::Output {
    Command::new(real_git_dir().join("git"))
        .args(args)
        .current_dir(cwd)
        .env("COX_COMPRESS", "0")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .stdin(Stdio::null())
        .output()
        .unwrap_or_else(|e| panic!("`git {}` failed to spawn: {e}", args.join(" ")))
}

/// Step 1 of the repro: commit a file well past the compression threshold.
fn repro_repo() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    {
        let run = |args: &[&str]| {
            let out = plain_git(dir.path(), args);
            assert!(
                out.status.success(),
                "setup `git {}` failed:\n{}",
                args.join(" "),
                String::from_utf8_lossy(&out.stderr)
            );
        };
        run(&["init", "-q", "."]);
        run(&["config", "user.email", "cox@example.test"]);
        run(&["config", "user.name", "COX-B015"]);
        let mut body = String::new();
        for i in 1..=REPRO_LINES {
            let _ = writeln!(
                body,
                "line {i:04}: the quick brown fox jumps over the lazy dog"
            );
        }
        std::fs::write(dir.path().join("big.txt"), &body).unwrap();
        run(&["add", "big.txt"]);
        run(&["commit", "-qm", "COX-B015 repro"]);
    }
    dir
}

/// The real shim script on a `PATH` where it precedes the real `git`.
struct RealShim {
    _dir: tempfile::TempDir,
    shim: PathBuf,
    path: std::ffi::OsString,
}

impl RealShim {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let shim_dir = dir.path().join("shims");
        std::fs::create_dir_all(&shim_dir).unwrap();
        let shim = shim_dir.join("git");
        write_exec(
            &shim,
            &coxagent_app::shim_script(
                "git",
                &shim_dir.display().to_string(),
                env!("CARGO_BIN_EXE_coxagent"),
            ),
        );
        let path = std::env::join_paths([
            shim_dir,
            real_git_dir(),
            PathBuf::from("/usr/bin"),
            PathBuf::from("/bin"),
        ])
        .unwrap();
        Self {
            _dir: dir,
            shim,
            path,
        }
    }

    /// Step 2: run it *through* the shim, stdout and stderr on separate pipes
    /// (neither is a tty, so the shim takes its compressing branch).
    fn run(&self, cwd: &Path, args: &[&str]) -> std::process::Output {
        Command::new(&self.shim)
            .args(args)
            .current_dir(cwd)
            .env("PATH", &self.path)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .env_remove("COX_COMPRESS")
            .stdin(Stdio::null())
            .output()
            .expect("failed to run the shim")
    }
}

#[test]
fn the_ticket_repro_a_3000_line_file_through_the_real_shim_is_byte_exact() {
    let repo = repro_repo();
    let sh = RealShim::new();
    let blob = String::from_utf8(plain_git(repo.path(), &["rev-parse", "HEAD:big.txt"]).stdout)
        .unwrap()
        .trim()
        .to_owned();

    for args in [
        vec!["show", "HEAD:big.txt"],
        vec!["diff", "--no-color", EMPTY_TREE, "HEAD"],
        vec!["cat-file", "-p", &blob],
    ] {
        let label = format!("git {}", args.join(" "));
        // Step 3: the same command with the shim out of the way — the oracle.
        let oracle = plain_git(repo.path(), &args);
        assert!(
            oracle.status.success(),
            "`{label}` failed:\n{}",
            String::from_utf8_lossy(&oracle.stderr)
        );
        assert!(
            oracle.stdout.len() > 6_000,
            "`{label}` produced only {} bytes — under the clip threshold, so a \
             pass would prove nothing",
            oracle.stdout.len()
        );

        let shimmed = sh.run(repo.path(), &args);

        // Step 4: the middle must still be there. (AC1)
        assert_byte_exact(&format!("{label} stdout"), &oracle.stdout, &shimmed.stdout);
        // AC2: no elision, no summary marker — ever, on these subcommands.
        let text = String::from_utf8_lossy(&shimmed.stdout);
        assert!(
            !text.contains("elided") && !text.contains("output compressed"),
            "`{label}` output carries a compression marker"
        );
        // AC3: streams stay separate and the exit code is the wrapped one.
        assert_byte_exact(&format!("{label} stderr"), &oracle.stderr, &shimmed.stderr);
        assert_eq!(
            shimmed.status.code(),
            oracle.status.code(),
            "`{label}`: exit code"
        );
    }
}

#[test]
fn the_same_content_is_compressed_for_porcelain_and_other_tools() {
    // Guards the other side of the fix: content-retrieval subcommands are
    // exempted, everything else still gets the token saving.
    let root = repo_root();
    let real = git(
        &root,
        &["show", &format!("HEAD:{}", large_tracked_file(&root))],
    );

    for (cmd, args) in [("git", vec!["status"]), ("cargo", vec!["build"])] {
        let out = compress(cmd, &args, &real);
        assert!(
            out.len() < real.len(),
            "`{cmd} {}` output was not compressed",
            args.join(" ")
        );
        assert!(String::from_utf8_lossy(&out).contains("output compressed"));
    }
}

/// CXA-B109 regression guard, at the shim level: the compress binary's path is
/// baked into every shim script at generation time, and the shim directory is
/// shared — so when the hub that wrote them ran from a worktree the janitor
/// later purged, the baked binary vanished. Unguarded, the pipeline's second
/// stage dies at exec and the first stage SIGPIPEs: exit 141, ZERO bytes of
/// output, for every shimmed tool call until another hub rewrites the shims.
/// The shim must degrade to `exec "$real"` — exact output beats no output.
/// Fails on pre-fix code (empty stdout, bash's exec error on stderr).
#[test]
fn a_vanished_compress_binary_degrades_to_the_real_binary() {
    let vanished = "/coxagent-cxa-b109/purged-worktree/target/debug/coxagent";
    assert!(!Path::new(vanished).exists(), "the baked binary must be gone");

    // A plain-pipeline command (no exactness probe — the path the ticket is
    // about): every byte must come from the real binary, streams separate,
    // exit code the wrapped one.
    let sh = Shimmed::with_cmd_and_exe("node", vanished);
    let out = sh.run(&["--version"]);
    assert_byte_exact("vanished-exe node stdout", &fake_stdout(), &out.stdout);
    assert_byte_exact("vanished-exe node stderr", &fake_stderr(), &out.stderr);
    assert_eq!(
        out.status.code(),
        Some(FAKE_EXIT),
        "vanished-exe node: exit code"
    );
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        !err.contains("No such file or directory"),
        "the shim leaked its own exec failure to the caller: {err:.200}"
    );

    // The exact-aware path (`git` + probe) must degrade the same way instead
    // of trusting a probe that can no longer run.
    let git_sh = Shimmed::with_cmd_and_exe("git", vanished);
    let out = git_sh.run(&["show", "HEAD:big.rs"]);
    assert_byte_exact("vanished-exe git show stdout", &fake_stdout(), &out.stdout);
    assert_byte_exact("vanished-exe git show stderr", &fake_stderr(), &out.stderr);
    assert_eq!(
        out.status.code(),
        Some(FAKE_EXIT),
        "vanished-exe git show: exit code"
    );
}
