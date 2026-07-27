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

/// Install the real generated wrapper for `cmd` in its own temp dir and return
/// the path to it. This is the artifact `setup_command_shims` drops on an
/// agent's `PATH`, byte for byte — the tests below run the ticket's repro
/// through it instead of trusting the script's text.
fn install_shim(cmd: &str, tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("coxagent-shim-test-{}-{tag}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let script = coxagent_app::shim_script(
        cmd,
        &dir.display().to_string(),
        env!("CARGO_BIN_EXE_coxagent"),
    );
    let path = dir.join(cmd);
    std::fs::write(&path, script).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    path
}

/// A stand-in `git` that writes a warning to stderr and file content to
/// stdout, the way `git diff` does when it skips inexact rename detection.
/// Returns the directory to put on `PATH` ahead of the real binary.
///
/// A fake is the only way to pin this down: whether the real `git` warns
/// depends on the repository, so a test built on it would pass by luck.
fn install_fake_git(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("coxagent-fake-git-{}-{tag}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("git");
    std::fs::write(
        &path,
        format!(
            "#!/usr/bin/env bash\n\
             echo \"{FAKE_GIT_WARNING}\" >&2\n\
             i=1\n\
             while [ $i -le {FAKE_GIT_LINES} ]; do echo \"line $i of tracked file content\"; \
             i=$((i+1)); done\n"
        ),
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    dir
}

/// What the fake `git` writes to stderr, and how many lines of content it
/// writes to stdout — enough that the compressor would clip the middle.
const FAKE_GIT_WARNING: &str = "warning: inexact rename detection was skipped";
const FAKE_GIT_LINES: u32 = 400;

/// Exactly what the fake `git` puts on stdout, and nothing else.
fn fake_git_content() -> Vec<u8> {
    (1..=FAKE_GIT_LINES)
        .map(|i| format!("line {i} of tracked file content\n"))
        .collect::<String>()
        .into_bytes()
}

/// Run an installed shim exactly as an agent would: non-tty stdout, compression
/// enabled, the shim dir first on `PATH` (so a shim that failed to skip itself
/// would recurse instead of quietly passing the test).
fn run_shim(shim: &Path, root: &Path, args: &[&str]) -> Vec<u8> {
    let out = shim_output(shim, root, args, None);
    assert!(
        out.status.success(),
        // The shim folds stderr into the compressed stream, so a diagnosis
        // needs both streams.
        "shim `{} {}` failed ({}):\n{}{}",
        shim.display(),
        args.join(" "),
        out.status,
        String::from_utf8_lossy(&out.stderr),
        String::from_utf8_lossy(&out.stdout),
    );
    out.stdout
}

/// The same, but keeping both streams apart — the only way to see whether
/// stderr leaked into the content — and with an optional directory spliced in
/// front of the real binary on `PATH`.
fn shim_output(
    shim: &Path,
    root: &Path,
    args: &[&str],
    ahead_of_real: Option<&Path>,
) -> std::process::Output {
    let dir = shim.parent().unwrap().display().to_string();
    // Any *other* shim dir inherited from the developer's environment has to
    // go: this shim only skips its own directory, so a second one on `PATH`
    // would make the two wrappers exec each other until `fork` gives up.
    let inherited = std::env::var("PATH").unwrap_or_default();
    let clean = inherited
        .split(':')
        .filter(|p| {
            !Path::new(p)
                .file_name()
                .is_some_and(|n| n == "coxagent-shims")
        })
        .collect::<Vec<_>>()
        .join(":");
    let path = match ahead_of_real {
        Some(p) => format!("{dir}:{}:{clean}", p.display()),
        None => format!("{dir}:{clean}"),
    };
    Command::new(shim)
        .args(args)
        .current_dir(root)
        .env("PATH", path)
        .env("COX_COMPRESS", "1")
        .output()
        .unwrap_or_else(|e| panic!("shim {} failed to spawn: {e}", shim.display()))
}

/// Lines in the file the repro test commits. The ticket's own figure — well
/// past `clip_middle`'s 6000-char budget and past the 64 KiB pipe buffer.
const REPRO_LINES: u32 = 3_000;

/// The generated file's content: distinct lines, so nothing here passes by
/// being deduped away, and wide enough that the middle is what gets elided.
fn repro_file() -> String {
    (0..REPRO_LINES / 4)
        .map(|i| {
            format!(
                "fn generated_{i}() -> usize {{\n\
                 \x20   // padding line {i} — keeps the blob over the compression threshold\n\
                 \x20   {i}\n\
                 }}\n"
            )
        })
        .collect()
}

/// Run a `git` fixture command, failing loudly. Isolated from the developer's
/// global/system config: a signing key or a commit template there would break
/// the fixture for reasons unrelated to this guard.
fn fixture_git(dir: &Path, args: &[&str]) {
    let out = Command::new("git")
        .args(args)
        .current_dir(dir)
        .env("COX_COMPRESS", "0")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .env("GIT_AUTHOR_NAME", "COX-B015")
        .env("GIT_AUTHOR_EMAIL", "b015@example.invalid")
        .env("GIT_COMMITTER_NAME", "COX-B015")
        .env("GIT_COMMITTER_EMAIL", "b015@example.invalid")
        .output()
        .unwrap_or_else(|e| panic!("fixture `git {}` failed to spawn: {e}", args.join(" ")));
    assert!(
        out.status.success(),
        "fixture `git {}` failed ({}):\n{}",
        args.join(" "),
        out.status,
        String::from_utf8_lossy(&out.stderr)
    );
}

/// Steps 1–2 of the ticket's repro: a throwaway repository whose HEAD holds a
/// ~3000-line file, plus a parent commit so `git diff HEAD~1 HEAD` is the whole
/// file as a patch. Generated rather than picked out of this tree, so the guard
/// cannot quietly stop exercising the threshold if the tree shrinks.
fn build_repro_repo(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("coxagent-b015-repo-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("src")).unwrap();
    fixture_git(&dir, &["init", "-q"]);
    std::fs::write(dir.join("README.md"), "COX-B015 repro\n").unwrap();
    fixture_git(&dir, &["add", "."]);
    fixture_git(&dir, &["commit", "-qm", "base"]);
    std::fs::write(dir.join("src/big.rs"), repro_file()).unwrap();
    fixture_git(&dir, &["add", "."]);
    fixture_git(&dir, &["commit", "-qm", "add the large file"]);
    dir
}

#[test]
fn the_ticket_repro_reads_a_freshly_committed_file_back_byte_exact() {
    // COX-B015 steps 1–4, end to end and self-contained: commit a ~3000-line
    // file, read it back through the installed shim, diff against the unshimmed
    // command. Pre-fix this fails on the first subcommand with the middle of the
    // file replaced by `… [N chars elided] …`.
    let repo = build_repro_repo("exact");
    let shim = install_shim("git", "repro");

    for args in [
        vec!["show", "HEAD:src/big.rs"],
        vec!["cat-file", "-p", "HEAD:src/big.rs"],
        vec!["diff", "--no-color", "HEAD~1", "HEAD"],
    ] {
        let label = format!("git {}", args.join(" "));
        let real = git(&repo, &args);
        assert!(
            real.len() > 6_000,
            "`{label}` produced only {} bytes — too small to exercise clipping",
            real.len()
        );

        let out = shim_output(&shim, &repo, &args, None);

        assert!(
            out.status.success(),
            "shim `{label}` failed ({}):\n{}",
            out.status,
            String::from_utf8_lossy(&out.stderr)
        );
        assert_byte_exact(&label, &real, &out.stdout);
        // Byte-exactness already implies this, but the ticket asks for it by
        // name: no elision marker, no compression footer, ever.
        let text = String::from_utf8_lossy(&out.stdout);
        assert!(
            !text.contains("elided") && !text.contains("output compressed"),
            "`{label}` carries a compression marker"
        );
        assert!(
            out.stderr.is_empty(),
            "`{label}` wrote to stderr: {:?}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
    let _ = std::fs::remove_dir_all(shim.parent().unwrap());
    let _ = std::fs::remove_dir_all(&repo);
}

#[test]
fn a_failing_content_retrieval_passes_its_exit_code_and_stderr_through() {
    // The third acceptance criterion: the unpiped path must not swallow the
    // failure. `exec`ing the real binary keeps both, where the piped path would
    // report the *compressor's* status and fold stderr into stdout.
    let repo = build_repro_repo("failure");
    let shim = install_shim("git", "repro-failure");
    let args = ["show", "HEAD:src/does-not-exist.rs"];

    let real = Command::new("git")
        .args(args)
        .current_dir(&repo)
        .env("COX_COMPRESS", "0")
        .output()
        .expect("`git show` failed to spawn");
    assert!(
        !real.status.success(),
        "the fixture path unexpectedly exists"
    );

    let shimmed = shim_output(&shim, &repo, &args, None);

    assert_eq!(
        real.status.code(),
        shimmed.status.code(),
        "the shim rewrote the exit code"
    );
    assert_byte_exact(
        "git show (missing path) stdout",
        &real.stdout,
        &shimmed.stdout,
    );
    assert_byte_exact(
        "git show (missing path) stderr",
        &real.stderr,
        &shimmed.stderr,
    );
    let _ = std::fs::remove_dir_all(shim.parent().unwrap());
    let _ = std::fs::remove_dir_all(&repo);
}

#[test]
fn the_installed_git_shim_keeps_content_retrieval_byte_exact() {
    // The end-to-end repro from COX-B015: `git show HEAD:<path>` resolved
    // through the shim on `PATH`. The unit tests cover `coxagent compress` and
    // the script's text separately; only this one proves the two agree, which
    // is what an agent shelling out to `git` actually depends on.
    let root = repo_root();
    let shim = install_shim("git", "exact");
    let path = large_tracked_file(&root);
    let rev = format!("HEAD:{path}");

    let real = git(&root, &["show", &rev]);
    assert!(
        real.len() > 6_000,
        "`git show {rev}` is only {} bytes — too small to exercise clipping",
        real.len()
    );
    let shimmed = run_shim(&shim, &root, &["show", &rev]);

    assert_byte_exact(&format!("shim git show {rev}"), &real, &shimmed);
    let _ = std::fs::remove_dir_all(shim.parent().unwrap());
}

#[test]
fn the_installed_git_shim_still_compresses_non_content_subcommands() {
    // The other half: the bypass must be scoped to content retrieval, or the
    // fix would silently switch the token-saver off for `git` entirely.
    let root = repo_root();
    let shim = install_shim("git", "compressed");

    let real = git(&root, &["ls-tree", "-r", "HEAD"]);
    assert!(
        real.len() > 2_500,
        "`git ls-tree -r HEAD` is only {} bytes — below the passthrough threshold",
        real.len()
    );
    let shimmed = run_shim(&shim, &root, &["ls-tree", "-r", "HEAD"]);

    assert!(
        shimmed.len() < real.len()
            && String::from_utf8_lossy(&shimmed).contains("output compressed"),
        "`git ls-tree` lost its compression: {} bytes in, {} bytes out",
        real.len(),
        shimmed.len()
    );
    let _ = std::fs::remove_dir_all(shim.parent().unwrap());
}

#[test]
fn the_exactness_check_answers_from_the_argv_alone() {
    // The shim asks *before* the wrapped command runs, so `--check` has to
    // answer without touching stdin — reading it there would hang every call.
    let check = |args: &[&str]| {
        let out = Command::new(env!("CARGO_BIN_EXE_coxagent"))
            .arg("compress")
            .args(["--check", "--cmd", "git", "--"])
            .args(args)
            .stdin(Stdio::null())
            .output()
            .expect("failed to spawn the coxagent binary");
        assert!(out.status.success(), "`compress --check` failed: {out:?}");
        String::from_utf8(out.stdout).unwrap().trim().to_owned()
    };
    assert_eq!(check(&["show", "HEAD:src/lib.rs"]), "exact");
    assert_eq!(check(&["-C", "/repo", "diff", "HEAD"]), "exact");
    assert_eq!(check(&["status"]), "compress");
}

#[test]
fn a_warning_on_stderr_stays_out_of_byte_exact_content() {
    // COX-B015, second corruption path: exempting content retrieval from the
    // compressor is not enough while the shim still pipes `2>&1`. The merge
    // splices whatever git wrote to stderr into the file content — and empties
    // stderr, so the caller sees no sign of it. Content retrieval has to skip
    // the pipe altogether.
    let root = repo_root();
    let shim = install_shim("git", "stderr-exact");
    let fake = install_fake_git("exact");

    let out = shim_output(&shim, &root, &["show", "HEAD:big.rs"], Some(&fake));

    assert!(out.status.success(), "shim `git show` failed: {out:?}");
    assert_byte_exact(
        "git show (warning on stderr)",
        &fake_git_content(),
        &out.stdout,
    );
    assert!(
        String::from_utf8_lossy(&out.stderr).contains(FAKE_GIT_WARNING),
        "the warning was swallowed instead of reaching stderr: {:?}",
        String::from_utf8_lossy(&out.stderr)
    );
    let _ = std::fs::remove_dir_all(shim.parent().unwrap());
    let _ = std::fs::remove_dir_all(&fake);
}

#[test]
fn a_porcelain_subcommand_keeps_its_stderr_merged_and_compressed() {
    // The other side of that fix: everything except content retrieval must
    // still go through the pipe, where folding stderr in is the point — build
    // and test tools put most of their output there.
    let root = repo_root();
    let shim = install_shim("git", "stderr-compressed");
    let fake = install_fake_git("compressed");

    let out = shim_output(&shim, &root, &["status"], Some(&fake));

    assert!(out.status.success(), "shim `git status` failed: {out:?}");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("output compressed"),
        "`git status` lost its compression: {stdout}"
    );
    assert!(
        stdout.contains(FAKE_GIT_WARNING) && out.stderr.is_empty(),
        "`git status` stderr is no longer folded into the compressed stream"
    );
    let _ = std::fs::remove_dir_all(shim.parent().unwrap());
    let _ = std::fs::remove_dir_all(&fake);
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
