use crate::model::{Files, Identity};
use anyhow::{Context, Result, bail};
use std::{
    collections::BTreeMap,
    io::Write,
    path::{Component, Path, PathBuf},
    process::{Command, Stdio},
    time::{Duration, Instant},
};

pub struct Git {
    pub dir: PathBuf,
}

/// Who made a commit and when, with the blob a path holds in it.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct CommitInfo {
    pub author: Identity,
    /// Commit time, Unix seconds.
    pub time: u64,
    /// The blob at the queried path; `None` when the commit deleted it.
    pub blob: Option<String>,
}

/// Why a Git process failed, derived from its stderr without ever exposing that text.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GitFailureKind {
    /// The remote could not be reached: DNS, connect, timeout, or TLS handshake.
    Offline,
    /// The remote refused the credentials or access to the repository.
    Authentication,
    /// The remote rejected the update, e.g. a non-fast-forward push.
    Rejected,
    /// Anything else, including failures that do not match a known pattern.
    Other,
}

#[derive(Debug)]
pub struct GitFailure {
    pub kind: GitFailureKind,
    pub exit: Option<i32>,
}

impl std::fmt::Display for GitFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let exit = self.exit.unwrap_or(-1);
        match self.kind {
            GitFailureKind::Offline => write!(
                f,
                "Remote unreachable (exit {exit}). Check the network connection; local snapshots are retained and Mesh retries automatically"
            ),
            GitFailureKind::Authentication => write!(
                f,
                "Remote refused access (exit {exit}). Check repository permissions and sign-in; local snapshots are retained"
            ),
            GitFailureKind::Rejected => write!(
                f,
                "Remote rejected the update (exit {exit}). Mesh fetches and retries; local snapshots are retained"
            ),
            GitFailureKind::Other => write!(
                f,
                "Git operation failed (exit {exit}). Check repository access and connectivity; local snapshots are retained"
            ),
        }
    }
}

impl std::error::Error for GitFailure {}

/// The Git failure kind anywhere in an error's chain, if Git caused it.
pub fn failure_kind(error: &anyhow::Error) -> Option<GitFailureKind> {
    error
        .chain()
        .find_map(|cause| cause.downcast_ref::<GitFailure>())
        .map(|failure| failure.kind)
}

/// Classify untranslated (`LC_ALL=C`) Git stderr. Access problems are checked
/// before connectivity because their messages also mention the remote URL.
pub fn classify_stderr(stderr: &str) -> GitFailureKind {
    let text = stderr.to_ascii_lowercase();
    let any = |patterns: &[&str]| patterns.iter().any(|p| text.contains(p));
    if any(&[
        "authentication failed",
        "could not read username",
        "could not read password",
        "permission denied (publickey",
        "returned error: 401",
        "returned error: 403",
        "repository not found",
        "access denied",
        "invalid username or password",
    ]) {
        GitFailureKind::Authentication
    } else if any(&[
        "[rejected]",
        "[remote rejected]",
        "non-fast-forward",
        "fetch first",
        "failed to push some refs",
    ]) {
        GitFailureKind::Rejected
    } else if any(&[
        "could not resolve host",
        "could not resolve hostname",
        "failed to connect to",
        "connection refused",
        "connection timed out",
        "operation timed out",
        "timed out after",
        "network is unreachable",
        "no route to host",
        "connection reset",
        "ssl connect error",
        "tls handshake",
        "handshake failed",
        "gnutls_handshake",
    ]) {
        GitFailureKind::Offline
    } else {
        GitFailureKind::Other
    }
}

impl Git {
    pub fn command(&self) -> Command {
        let mut cmd = Command::new("git");
        cmd.arg("--no-pager")
            .arg("--git-dir")
            .arg(&self.dir)
            .env("GIT_TERMINAL_PROMPT", "0")
            .env("GCM_INTERACTIVE", "never")
            // Untranslated messages keep failure classification locale-independent.
            .env("LC_ALL", "C")
            .env_remove("LANGUAGE");
        // Keep inherited Git environment from redirecting this private repository.
        for key in [
            "GIT_WORK_TREE",
            "GIT_INDEX_FILE",
            "GIT_OBJECT_DIRECTORY",
            "GIT_ALTERNATE_OBJECT_DIRECTORIES",
            "GIT_COMMON_DIR",
        ] {
            cmd.env_remove(key);
        }
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            cmd.creation_flags(0x08000000);
        }
        cmd
    }

    pub fn run(&self, args: &[&str], input: Option<&[u8]>) -> Result<Vec<u8>> {
        self.execute(self.command().args(args), input)
            .with_context(|| format!("Git {}", args.first().unwrap_or(&"operation")))
    }

    pub fn execute(&self, command: &mut Command, input: Option<&[u8]>) -> Result<Vec<u8>> {
        // Files avoid pipe-buffer deadlocks on large binary snapshots.
        let output = tempfile::tempfile()?;
        let errors = tempfile::tempfile()?;
        let mut source = tempfile::tempfile()?;
        use std::io::{Read, Seek};
        source.write_all(input.unwrap_or_default())?;
        source.rewind()?;
        let network = command
            .get_args()
            .any(|arg| arg == "fetch" || arg == "push" || arg == "ls-remote");
        let mut child = command
            .stdin(Stdio::from(source))
            .stdout(output.try_clone()?)
            .stderr(errors.try_clone()?)
            .spawn()
            .context("Cannot launch Git; install Git for this console preview")?;
        let start = Instant::now();
        let status = loop {
            if let Some(status) = child.try_wait()? {
                break status;
            }
            if start.elapsed() > Duration::from_secs(30) {
                let _ = child.kill();
                let _ = child.wait();
                if network {
                    return Err(GitFailure {
                        kind: GitFailureKind::Offline,
                        exit: None,
                    }
                    .into());
                }
                bail!("Git operation timed out; local snapshots are retained");
            }
            std::thread::sleep(Duration::from_millis(5));
        };
        let mut output = output;
        let mut bytes = Vec::new();
        output.rewind()?;
        output.take(128 * 1024 * 1024 + 1).read_to_end(&mut bytes)?;
        anyhow::ensure!(
            bytes.len() <= 128 * 1024 * 1024,
            "Git output exceeds preview size limit"
        );
        if !status.success() {
            // Do not echo Git stderr: a server or credential helper may include secrets.
            // It is read only to classify the failure, and never leaves this function.
            let mut errors = errors;
            let mut stderr = Vec::new();
            errors.rewind()?;
            errors.take(64 * 1024).read_to_end(&mut stderr)?;
            return Err(GitFailure {
                kind: classify_stderr(&String::from_utf8_lossy(&stderr)),
                exit: status.code(),
            }
            .into());
        }
        Ok(bytes)
    }

    pub fn text(&self, args: &[&str]) -> Result<String> {
        Ok(String::from_utf8(self.run(args, None)?)?.trim().to_string())
    }

    pub fn init(&self) -> Result<()> {
        std::fs::create_dir_all(&self.dir)?;
        self.run(&["init", "--bare"], None)?;
        self.run(&["config", "core.autocrlf", "false"], None)?;
        self.run(&["config", "core.hooksPath", "/dev/null"], None)?;
        Ok(())
    }

    pub fn object(&self, data: &[u8]) -> Result<String> {
        Ok(
            String::from_utf8(self.run(&["hash-object", "-w", "--stdin"], Some(data))?)?
                .trim()
                .to_string(),
        )
    }

    pub fn commit(
        &self,
        files: &Files,
        parent: Option<&str>,
        author: &Identity,
        message: &str,
    ) -> Result<String> {
        let entries = files
            .iter()
            .map(|(path, bytes)| Ok((path.clone(), self.object(bytes)?)))
            .collect::<Result<BTreeMap<_, _>>>()?;
        self.commit_objects(&entries, parent, author, message)
    }

    pub fn commit_objects(
        &self,
        objects: &BTreeMap<String, String>,
        parent: Option<&str>,
        author: &Identity,
        message: &str,
    ) -> Result<String> {
        let temp = tempfile::tempdir()?;
        let index = temp.path().join("index");
        self.execute(
            self.command()
                .env("GIT_INDEX_FILE", &index)
                .args(["read-tree", "--empty"]),
            None,
        )
        .context("Create isolated index")?;
        let mut entries = Vec::new();
        for (path, blob) in objects {
            validate_path(path)?;
            validate_oid(blob)?;
            entries.extend_from_slice(format!("100644 {blob}\t{path}\0").as_bytes());
        }
        self.execute(
            self.command().env("GIT_INDEX_FILE", &index).args([
                "update-index",
                "-z",
                "--index-info",
            ]),
            Some(&entries),
        )
        .context("Populate isolated index")?;
        let tree = String::from_utf8(
            self.execute(
                self.command()
                    .env("GIT_INDEX_FILE", &index)
                    .arg("write-tree"),
                None,
            )
            .context("Write isolated tree")?,
        )?;
        let mut cmd = self.command();
        cmd.arg("commit-tree").arg(tree.trim());
        if let Some(parent) = parent {
            cmd.arg("-p").arg(parent);
        }
        cmd.env("GIT_AUTHOR_NAME", &author.name)
            .env("GIT_AUTHOR_EMAIL", &author.email)
            .env("GIT_COMMITTER_NAME", &author.name)
            .env("GIT_COMMITTER_EMAIL", &author.email);
        Ok(String::from_utf8(
            self.execute(&mut cmd, Some(message.as_bytes()))
                .context("Create immutable commit")?,
        )?
        .trim()
        .to_string())
    }

    pub fn files(&self, reference: &str) -> Result<Files> {
        self.tree_entries(reference)?
            .into_iter()
            .map(|(path, blob)| Ok((path, self.read_blob(&blob, 128 * 1024 * 1024)?)))
            .collect()
    }

    /// The Git object id `data` would have as a blob here, without storing it.
    pub fn blob_id(&self, data: &[u8]) -> Result<String> {
        Ok(
            String::from_utf8(self.run(&["hash-object", "--stdin"], Some(data))?)?
                .trim()
                .to_string(),
        )
    }

    /// The newest commit reachable from `reference` that changed `path`, and
    /// the id of the blob at `path` in it (`None` when that commit deleted
    /// the file). `None` for a path no commit touched. Read-only.
    pub fn last_commit(&self, reference: &str, path: &str) -> Result<Option<CommitInfo>> {
        validate_path(path)?;
        let mut command = self.command();
        command.args([
            "--literal-pathspecs",
            "log",
            "-1",
            "--format=%an%x00%ae%x00%ct%x00%H",
            reference,
            "--",
            path,
        ]);
        let output = self.execute(&mut command, None).context("Git log")?;
        let output = String::from_utf8(output)?;
        let fields: Vec<_> = output.trim_end().split('\0').collect();
        let [name, email, time, commit] = fields.as_slice() else {
            return Ok(None);
        };
        validate_oid(commit)?;
        let blob = self
            .text(&[
                "rev-parse",
                "--verify",
                "--quiet",
                &format!("{commit}:{path}"),
            ])
            .ok()
            .filter(|object| validate_oid(object).is_ok());
        Ok(Some(CommitInfo {
            author: Identity {
                name: (*name).to_string(),
                email: (*email).to_string(),
            },
            time: time.parse().unwrap_or_default(),
            blob,
        }))
    }

    pub fn read_blob(&self, object: &str, limit: u64) -> Result<Vec<u8>> {
        validate_oid(object)?;
        let size: u64 = self.text(&["cat-file", "-s", object])?.parse()?;
        anyhow::ensure!(
            size <= limit,
            "Referenced Git blob exceeds the configured size limit"
        );
        self.run(&["cat-file", "blob", object], None)
    }

    pub fn tree_entries(&self, reference: &str) -> Result<BTreeMap<String, String>> {
        let mut files = BTreeMap::new();
        for record in self
            .run(&["ls-tree", "-r", "-z", reference], None)?
            .split(|b| *b == 0)
            .filter(|b| !b.is_empty())
        {
            let record = std::str::from_utf8(record)?;
            let (meta, path) = record.split_once('\t').context("Invalid Git tree entry")?;
            validate_path(path)?;
            let fields: Vec<_> = meta.split(' ').collect();
            anyhow::ensure!(
                fields.len() == 3 && fields[0] == "100644" && fields[1] == "blob",
                "Unsupported executable, symlink, or submodule at {path}; connect a plain-file workspace for this preview"
            );
            validate_oid(fields[2])?;
            files.insert(path.to_string(), fields[2].to_string());
        }
        Ok(files)
    }

    pub fn merge(
        &self,
        accepted: &Option<Vec<u8>>,
        base: &Option<Vec<u8>>,
        incoming: &Option<Vec<u8>>,
    ) -> Result<Option<Option<Vec<u8>>>> {
        if accepted == base || accepted == incoming {
            return Ok(Some(incoming.clone()));
        }
        if incoming == base {
            return Ok(Some(accepted.clone()));
        }
        if crate::model::binary(accepted)
            || crate::model::binary(base)
            || crate::model::binary(incoming)
        {
            return Ok(None);
        }
        let (Some(ours), Some(original), Some(theirs)) = (accepted, base, incoming) else {
            return Ok(None);
        };
        let dir = tempfile::tempdir()?;
        for (name, bytes) in [("ours", ours), ("base", original), ("theirs", theirs)] {
            std::fs::write(dir.path().join(name), bytes)?;
        }
        let mut cmd = self.command();
        cmd.arg("merge-file")
            .arg("-p")
            .arg(dir.path().join("ours"))
            .arg(dir.path().join("base"))
            .arg(dir.path().join("theirs"));
        // A nonzero merge-file exit means preserve inputs, never install conflict markers.
        match self.execute(&mut cmd, None) {
            Ok(bytes) => Ok(Some(Some(bytes))),
            Err(_) => Ok(None),
        }
    }
}

pub fn validate_oid(object: &str) -> Result<()> {
    anyhow::ensure!(
        [40, 64].contains(&object.len())
            && object
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)),
        "Invalid Git object reference"
    );
    Ok(())
}

pub fn validate_path(path: &str) -> Result<()> {
    anyhow::ensure!(
        !path
            .chars()
            .any(|c| c.is_control() || "<>\"|?*".contains(c))
            && path
                .split('/')
                .all(|part| !part.is_empty() && part != "." && part != ".."),
        "Unsafe workspace path"
    );
    anyhow::ensure!(
        !path.is_empty() && !path.contains(['\\', ':', '\0']) && !path.ends_with([' ', '.']),
        "Unsafe workspace path"
    );
    for component in Path::new(path).components() {
        let Component::Normal(part) = component else {
            bail!("Unsafe workspace path");
        };
        let part = part.to_str().context("Non-UTF8 path")?;
        let lower = part.to_ascii_lowercase();
        let stem = lower.split('.').next().unwrap_or_default();
        anyhow::ensure!(
            ![".git", ".mesh"].contains(&lower.as_str())
                && !part.ends_with([' ', '.'])
                && ![
                    "con", "prn", "aux", "nul", "com1", "com2", "com3", "com4", "com5", "com6",
                    "com7", "com8", "com9", "lpt1", "lpt2", "lpt3", "lpt4", "lpt5", "lpt6", "lpt7",
                    "lpt8", "lpt9"
                ]
                .contains(&stem),
            "Unsupported reserved workspace path"
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_git_process_runs_with_untranslated_messages() {
        let git = Git {
            dir: PathBuf::from("unused"),
        };
        let command = git.command();
        let envs: BTreeMap<_, _> = command.get_envs().collect();
        assert_eq!(
            envs.get(std::ffi::OsStr::new("LC_ALL")),
            Some(&Some(std::ffi::OsStr::new("C")))
        );
        assert_eq!(envs.get(std::ffi::OsStr::new("LANGUAGE")), Some(&None));
    }

    #[test]
    fn connection_failures_are_offline() {
        for stderr in [
            "fatal: unable to access 'https://github.com/a/b.git/': Could not resolve host: github.com",
            "fatal: unable to access 'https://127.0.0.1:9/x.git/': Failed to connect to 127.0.0.1 port 9 after 2040 ms: Connection refused",
            "fatal: unable to access 'https://example.com/x.git/': Failed to connect to example.com port 443 after 21000 ms: Timed out",
            "fatal: unable to access 'https://example.com/x.git/': OpenSSL SSL_connect: SSL_ERROR_SYSCALL in connection to example.com:443 SSL connect error",
            "fatal: unable to access 'https://example.com/x.git/': schannel: failed to receive handshake, SSL/TLS connection failed; TLS handshake failed",
            "ssh: Could not resolve hostname github.com: Name or service not known\r\nfatal: Could not read from remote repository.",
            "fatal: unable to access 'https://example.com/x.git/': gnutls_handshake() failed: The TLS connection was non-properly terminated.",
            "fatal: unable to access 'https://example.com/x.git/': Network is unreachable",
        ] {
            assert_eq!(classify_stderr(stderr), GitFailureKind::Offline, "{stderr}");
        }
    }

    #[test]
    fn access_failures_need_attention_not_offline() {
        for stderr in [
            "remote: Invalid username or token.\nfatal: Authentication failed for 'https://github.com/a/b.git/'",
            "fatal: could not read Username for 'https://github.com': terminal prompts disabled",
            "fatal: unable to access 'https://example.com/x.git/': The requested URL returned error: 403",
            "fatal: unable to access 'https://127.0.0.1:8080/x.git/': The requested URL returned error: 401",
            "git@github.com: Permission denied (publickey).\r\nfatal: Could not read from remote repository.",
            "remote: Repository not found.\nfatal: repository 'https://github.com/a/missing.git/' not found",
        ] {
            assert_eq!(
                classify_stderr(stderr),
                GitFailureKind::Authentication,
                "{stderr}"
            );
        }
    }

    #[test]
    fn rejected_pushes_and_unknown_failures_are_not_offline() {
        assert_eq!(
            classify_stderr(
                "To https://example.com/x.git\n ! [rejected]        abc -> main (non-fast-forward)\nerror: failed to push some refs to 'https://example.com/x.git'"
            ),
            GitFailureKind::Rejected
        );
        for stderr in [
            "fatal: '/tmp/missing.git' does not appear to be a git repository\nfatal: Could not read from remote repository.",
            "fatal: bad object deadbeef",
            "",
        ] {
            assert_eq!(classify_stderr(stderr), GitFailureKind::Other, "{stderr}");
        }
    }

    #[test]
    fn failure_messages_never_contain_stderr() {
        let failure = GitFailure {
            kind: GitFailureKind::Authentication,
            exit: Some(128),
        };
        let error = anyhow::Error::from(failure).context("Git push");
        assert_eq!(failure_kind(&error), Some(GitFailureKind::Authentication));
        let text = format!("{error:#}");
        assert!(
            !text.contains("token") && !text.contains("https://"),
            "{text}"
        );
    }
}
