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

impl Git {
    pub fn command(&self) -> Command {
        let mut cmd = Command::new("git");
        cmd.arg("--no-pager")
            .arg("--git-dir")
            .arg(&self.dir)
            .env("GIT_TERMINAL_PROMPT", "0")
            .env("GCM_INTERACTIVE", "never");
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
            bail!(
                "Git operation failed (exit {}). Check repository access and connectivity; local snapshots are retained",
                status.code().unwrap_or(-1)
            );
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
