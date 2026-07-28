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

#![allow(clippy::unwrap_used)]

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
        .filter_map(|l| {
            let (meta, path) = l.split_once('\t')?;
            let size: u64 = meta.split_whitespace().nth(3)?.parse().ok()?;
            (size >= MIN_BLOB_BYTES && path.ends_with(".rs")).then(|| path.to_owned())
        })
        .next()
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
        let dir = tempfile::tempdir().unwrap();
        let shim_dir = dir.path().join("shims");
        let fake_dir = dir.path().join("bin");
        std::fs::create_dir_all(&shim_dir).unwrap();
        std::fs::create_dir_all(&fake_dir).unwrap();

        let shim = shim_dir.join("git");
        write_exec(
            &shim,
            &coxagent_app::shim_script(
                "git",
                &shim_dir.display().to_string(),
                env!("CARGO_BIN_EXE_coxagent"),
            ),
        );
        write_exec(
            &fake_dir.join("git"),
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

/// The same argv shape must not drag *porcelain* out of compression (AC5):
/// skipping a global flag's value locates the real subcommand, it does not
/// make every flagged invocation exact.
#[test]
fn a_separate_value_global_flag_still_compresses_porcelain() {
    let sh = Shimmed::new();

    let out = sh.run(&["--super-prefix", "sub/", "status"]);

    let text = String::from_utf8_lossy(&out.stdout);
    assert!(
        text.contains("output compressed") && text.contains(&format!("(×{FAKE_LINES})")),
        "`git --super-prefix sub/ status` lost its compression: {text:.200}"
    );
    assert_eq!(out.status.code(), Some(FAKE_EXIT));
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
