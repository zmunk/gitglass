use crate::diff::Diff;
use anyhow::{anyhow, bail, Context, Result};
use std::{
    cell::RefCell,
    ffi::OsStr,
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    os::{
        fd::AsRawFd,
        unix::{
            ffi::OsStrExt,
            fs::{MetadataExt, OpenOptionsExt},
        },
    },
    path::{Component, Path, PathBuf},
    process::{Command, Stdio},
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Snapshot {
    pub head: String,
    pub index: String,
    pub work: String,
    pub head_oid: String,
    pub index_oid: String,
    pub mode: String,
    pub dev: u64,
    pub ino: u64,
    pub permissions: u32,
}
#[derive(Clone, Copy, Debug)]
pub enum Operation {
    Stage,
    Unstage,
    Discard,
}
// Include ctime as well as mtime: same-size rewrites and atomic replacements
// must invalidate the working copy, even when the mtime is preserved.
#[derive(Clone, Debug, PartialEq, Eq)]
struct WorkStamp {
    dev: u64,
    ino: u64,
    mode: u32,
    size: u64,
    mtime: (i64, i64),
    ctime: (i64, i64),
}
impl WorkStamp {
    fn of(meta: &fs::Metadata) -> Self {
        Self {
            dev: meta.dev(),
            ino: meta.ino(),
            mode: meta.mode(),
            size: meta.len(),
            mtime: (meta.mtime(), meta.mtime_nsec()),
            ctime: (meta.ctime(), meta.ctime_nsec()),
        }
    }
}
struct Cache {
    snapshot: Snapshot,
    work_stamp: WorkStamp,
    config: Vec<u8>,
    attrs: Vec<u8>,
}
pub struct Repo {
    cwd: PathBuf,
    pub path: PathBuf,
    relative: String,
    spec: String,
    index_path: PathBuf,
    cache: RefCell<Option<Cache>>,
    #[cfg(test)]
    reads: std::cell::Cell<(usize, usize)>, // blob reads, working-file reads
}
fn output(mut cmd: Command, input: Option<&[u8]>) -> Result<Vec<u8>> {
    cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
    if input.is_some() {
        cmd.stdin(Stdio::piped());
    } else {
        cmd.stdin(Stdio::null());
    }
    let mut child = cmd.spawn().context("Could not execute Git")?;
    if let Some(data) = input {
        // Use a thread so Git's output cannot deadlock a large stdin write.
        let mut stdin = child.stdin.take().unwrap();
        let data = data.to_vec();
        let writer = std::thread::spawn(move || stdin.write_all(&data));
        let out = child.wait_with_output()?;
        let write_result = writer
            .join()
            .map_err(|_| anyhow!("Git input writer failed"))?;
        if !out.status.success() {
            bail!(
                "Git failed: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            );
        }
        write_result?;
        return Ok(out.stdout);
    }
    let out = child.wait_with_output()?;
    if !out.status.success() {
        bail!(
            "Git failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(out.stdout)
}
fn text(bytes: Vec<u8>) -> Result<String> {
    if bytes.contains(&0) {
        bail!("Binary files are unsupported");
    }
    String::from_utf8(bytes)
        .context("Unsupported text representation: only UTF-8 text is supported")
}
fn normalize(path: &Path) -> PathBuf {
    let mut result = PathBuf::new();
    for part in path.components() {
        match part {
            Component::ParentDir => {
                result.pop();
            }
            Component::CurDir => {}
            x => result.push(x.as_os_str()),
        }
    }
    result
}
impl Repo {
    pub fn open(cwd: &Path, file: &Path) -> Result<Self> {
        // Discovery happens exclusively in the invocation directory, not at the
        // target. Preserve Git's environment (GIT_DIR, GIT_WORK_TREE, etc.).
        let cwd = cwd
            .canonicalize()
            .context("Invocation directory is unavailable")?;
        let git = |args: &[&str]| -> Result<Vec<u8>> {
            let mut c = Command::new("git");
            c.current_dir(&cwd).args(args);
            output(c, None)
        };
        let root = git(&["rev-parse", "--show-toplevel"])
            .context("No working-tree repository in the invocation context")?;
        let root = PathBuf::from(OsStr::from_bytes(root.strip_suffix(b"\n").unwrap_or(&root)));
        let path = normalize(&cwd.join(file));
        // Resolve parent aliases without following a symlink at the file itself.
        let path = path
            .parent()
            .context("Expected a file path")?
            .canonicalize()
            .context("Target directory is unavailable")?
            .join(path.file_name().context("Expected a file name")?);
        let relative = path
            .strip_prefix(&root)
            .context("Target is outside this repository")?
            .to_str()
            .context("Non-UTF-8 paths are unsupported")?
            .to_owned();
        if relative.is_empty() {
            bail!("Expected an ordinary file");
        }
        let index = git(&["rev-parse", "--path-format=absolute", "--git-path", "index"])?;
        let index_path = PathBuf::from(OsStr::from_bytes(
            index.strip_suffix(b"\n").unwrap_or(&index),
        ));
        let repo = Self {
            cwd: cwd.to_owned(),
            path,
            spec: format!(":(top,literal){relative}"),
            relative,
            index_path,
            cache: RefCell::new(None),
            #[cfg(test)]
            reads: std::cell::Cell::new((0, 0)),
        };
        repo.snapshot()?;
        Ok(repo)
    }
    fn command(&self, args: &[&str]) -> Command {
        let mut c = Command::new("git");
        c.current_dir(&self.cwd).args(args);
        c
    }
    fn git(&self, args: &[&str]) -> Result<Vec<u8>> {
        output(self.command(args), None)
    }
    fn blob(&self, oid: &str) -> Result<String> {
        if let Some(cache) = self.cache.borrow().as_ref() {
            if oid == cache.snapshot.head_oid {
                return Ok(cache.snapshot.head.clone());
            }
            if oid == cache.snapshot.index_oid {
                return Ok(cache.snapshot.index.clone());
            }
        }
        #[cfg(test)]
        self.reads.set((self.reads.get().0 + 1, self.reads.get().1));
        text(self.git(&["cat-file", "blob", oid])?)
    }
    fn head_tree(&self) -> Result<Vec<u8>> {
        let head = self
            .command(&["rev-parse", "--verify", "--quiet", "HEAD"])
            .output()
            .context("Cannot resolve HEAD")?;
        if head.status.code() == Some(1) {
            // Only an unborn branch has an empty baseline. Do not hide corrupt
            // refs, inaccessible objects, or other Git failures as empty content.
            let branch = self.git(&["symbolic-ref", "--quiet", "HEAD"])?;
            let branch = std::str::from_utf8(&branch)?.trim_end();
            let exists = self
                .command(&["show-ref", "--verify", "--quiet", branch])
                .output()
                .context("Cannot inspect unborn branch")?;
            if exists.status.code() == Some(1) {
                return Ok(Vec::new());
            }
            bail!("Cannot read the committed baseline (HEAD)");
        }
        if !head.status.success() {
            bail!(
                "Cannot resolve HEAD: {}",
                String::from_utf8_lossy(&head.stderr).trim()
            );
        }
        let oid = std::str::from_utf8(&head.stdout)?.trim();
        self.git(&["ls-tree", "--full-tree", "-z", oid, "--", &self.spec])
            .context("Cannot read the committed baseline (HEAD)")
    }
    pub fn cached_snapshot(&self) -> Snapshot {
        self.cache
            .borrow()
            .as_ref()
            .expect("Repo::open loads the snapshot")
            .snapshot
            .clone()
    }
    pub fn snapshot(&self) -> Result<Snapshot> {
        self.refresh(false)
    }
    // Mutations never trust a metadata-only check of the working file. Git
    // entries and transformation settings are checked on every refresh too.
    fn refresh(&self, validate: bool) -> Result<Snapshot> {
        let entries = self.git(&["ls-files", "--stage", "-z", "--", &self.spec])?;
        if entries.is_empty() {
            // Distinguish staged deletion from a genuinely untracked file.
            let head = self.head_tree()?;
            if !head.is_empty() {
                bail!("Staged deletions are unsupported");
            }
            bail!("Untracked file: track the file before opening it");
        }
        let records: Vec<_> = entries
            .split(|b| *b == 0)
            .filter(|x| !x.is_empty())
            .collect();
        if records.len() != 1 {
            bail!("Merge-conflicted files are unsupported");
        }
        let header = std::str::from_utf8(records[0].split(|b| *b == b'\t').next().unwrap())?;
        let fields: Vec<_> = header.split_whitespace().collect();
        if fields.len() != 3 || fields[2] != "0" {
            bail!("Merge-conflicted files are unsupported");
        }
        let mode = fields[0];
        if mode != "100644" && mode != "100755" {
            bail!("Symlinks, submodules and non-ordinary files are unsupported");
        }
        let tree = self.head_tree()?;
        let head_oid = if tree.is_empty() {
            // Missing in HEAD: an indexed addition uses an empty text baseline.
            String::new()
        } else {
            let tree_header = std::str::from_utf8(tree.split(|b| *b == b'\t').next().unwrap())?;
            let tree_fields: Vec<_> = tree_header.split_whitespace().collect();
            if tree_fields.len() != 3 || tree_fields[0] != mode || tree_fields[1] != "blob" {
                bail!("File-mode or file-type changes are unsupported");
            }
            tree_fields[2].to_owned()
        };
        let meta = fs::symlink_metadata(&self.path)
            .context("Deleted or unavailable working file is unsupported")?;
        if !meta.file_type().is_file() {
            bail!("Only ordinary files are supported (not symlinks or directories)");
        }
        if (meta.mode() & 0o111 != 0) != (mode == "100755") {
            bail!("File-mode changes are unsupported");
        }
        // Refuse transformations we cannot safely represent. Text normalization
        // is allowed only when the byte comparison below proves it is a no-op;
        // CRLF checkout conversion is still unsupported.
        let attrs = self.git(&[
            "check-attr",
            "-z",
            "filter",
            "working-tree-encoding",
            "ident",
            "text",
            "eol",
            "--",
            self.path.to_str().context("Non-UTF-8 path")?,
        ])?;
        let entries: Vec<_> = attrs.split(|b| *b == 0).collect();
        for entry in entries.chunks_exact(3) {
            let safe_text = entry[1] == b"text" && (entry[2] == b"set" || entry[2] == b"auto");
            let safe_eol = entry[1] == b"eol" && entry[2] == b"lf";
            if entry[2] != b"unspecified" && entry[2] != b"unset" && !safe_text && !safe_eol {
                bail!(
                    "Unsupported text transformation attribute: {}",
                    String::from_utf8_lossy(entry[1])
                );
            }
        }
        // Configuration and attributes can change while the application runs.
        // check-attr above validates effective attributes (including global and
        // info attributes); config includes includes and environment overrides.
        let config = self.git(&["config", "--null", "--list"])?;
        let work_stamp = WorkStamp::of(&meta);
        let cached_work = self.cache.borrow().as_ref().and_then(|cache| {
            (!validate
                && cache.work_stamp == work_stamp
                && cache.config == config
                && cache.attrs == attrs)
                .then(|| cache.snapshot.work.clone())
        });
        let work = if let Some(work) = cached_work {
            work
        } else {
            #[cfg(test)]
            self.reads.set((self.reads.get().0, self.reads.get().1 + 1));
            let work = text(fs::read(&self.path)?)?;
            // Reject implicit core.autocrlf conversion too, but permit byte-exact CRLF.
            let raw = output(
                self.command(&["hash-object", "--no-filters", "--stdin"]),
                Some(work.as_bytes()),
            )?;
            let filtered = output(
                self.command(&[
                    "hash-object",
                    &format!("--path={}", self.path.display()),
                    "--stdin",
                ]),
                Some(work.as_bytes()),
            )?;
            if raw != filtered {
                bail!(
                    "Unsupported text representation: Git clean/EOL conversion would change the file"
                );
            }
            work
        };
        let head = if head_oid.is_empty() {
            String::new()
        } else {
            self.blob(&head_oid)?
        };
        let index = if fields[1] == head_oid {
            head.clone()
        } else {
            self.blob(fields[1])?
        };
        let snapshot = Snapshot {
            head,
            index,
            work,
            head_oid,
            index_oid: fields[1].to_owned(),
            mode: mode.to_owned(),
            dev: meta.dev(),
            ino: meta.ino(),
            permissions: meta.mode(),
        };
        *self.cache.borrow_mut() = Some(Cache {
            snapshot: snapshot.clone(),
            work_stamp,
            config,
            attrs,
        });
        Ok(snapshot)
    }
    pub fn mutate(
        &self,
        expected: &Snapshot,
        op: Operation,
        selected: &[usize],
    ) -> Result<Snapshot> {
        if selected.is_empty() {
            bail!("No selected changes");
        }
        // Hold the real Git index lock across validation, copying and publishing.
        // The copy is made AFTER taking the lock, so unrelated staged entries
        // from another process are never replaced with an old index snapshot.
        let mut lock_name = self.index_path.as_os_str().to_os_string();
        lock_name.push(".lock");
        let lock_path = PathBuf::from(lock_name);
        let mut lock = IndexLock::acquire(lock_path)?;
        let current = self.refresh(true)?;
        if &current != expected {
            bail!("External change detected; action cancelled. Review the refreshed selection.");
        }
        let diff = match op {
            Operation::Unstage => Diff::between(&current.head, &current.index),
            _ => Diff::between(&current.index, &current.work),
        };
        if !selected.windows(2).all(|w| w[0] < w[1])
            || selected.iter().any(|i| !diff.actionable(*i))
        {
            bail!("Selection is no longer valid");
        }
        let content = diff.apply(selected, !matches!(op, Operation::Stage));
        let mut updated = current.clone();
        if matches!(op, Operation::Discard) {
            let mut file = OpenOptions::new()
                .read(true)
                .write(true)
                .custom_flags(libc::O_NOFOLLOW)
                .open(&self.path)?;
            let meta = file.metadata()?;
            let mut bytes = Vec::new();
            file.read_to_end(&mut bytes)?;
            if meta.dev() != current.dev
                || meta.ino() != current.ino
                || bytes != current.work.as_bytes()
                || self.refresh(true)? != current
            {
                bail!("External change detected during discard; action cancelled");
            }
            // Prepare a complete replacement in the same directory. Failed
            // writes leave the original intact, and rename publishes atomically.
            let mut replacement = tempfile::Builder::new()
                .prefix(".git-difftui-")
                .tempfile_in(self.path.parent().context("Invalid file path")?)?;
            replacement.write_all(content.as_bytes())?;
            let new_meta = replacement.as_file().metadata()?;
            if new_meta.uid() != meta.uid() || new_meta.gid() != meta.gid() {
                let result = unsafe {
                    libc::fchown(replacement.as_file().as_raw_fd(), meta.uid(), meta.gid())
                };
                if result != 0 {
                    return Err(std::io::Error::last_os_error().into());
                }
            }
            replacement.as_file().set_permissions(meta.permissions())?;
            // Include ACLs and other extended attributes; never silently drop
            // access-control metadata when atomically replacing the inode.
            for name in xattr::list(&self.path).context("Cannot read file attributes")? {
                if let Some(value) = xattr::get(&self.path, &name)? {
                    xattr::set(replacement.path(), &name, &value)
                        .context("Cannot preserve file attributes")?;
                }
            }
            replacement.as_file().sync_all()?;
            if self.refresh(true)? != current {
                bail!("External change detected during discard; action cancelled");
            }
            let published_meta = replacement.as_file().metadata()?;
            replacement
                .persist(&self.path)
                .map_err(|e| e.error)
                .context("Cannot publish discarded content")?;
            updated.work = content;
            updated.dev = published_meta.dev();
            updated.ino = published_meta.ino();
            updated.permissions = published_meta.mode();
            // Use the pre-publication stamp, not a later stat which could belong
            // to an external writer. A rename ctime change may cause one reread.
            self.cache.borrow_mut().as_mut().unwrap().work_stamp = WorkStamp::of(&published_meta);
        } else {
            let parent = self.index_path.parent().context("Invalid index path")?;
            let temp = tempfile::Builder::new()
                .prefix("difftui-index-")
                .tempfile_in(parent)?;
            fs::copy(&self.index_path, temp.path())?;
            let oid = output(
                self.command(&["hash-object", "-w", "--stdin"]),
                Some(content.as_bytes()),
            )?;
            let oid = std::str::from_utf8(&oid)?.trim();
            // --index-info consumes repository-root-relative names regardless
            // of the invocation prefix. NUL termination keeps tabs/newlines and
            // pathspec-looking filenames literal without shell/Git quoting.
            let record = format!("{} {}\t{}\0", current.mode, oid, self.relative);
            let mut cmd = self.command(&["update-index", "-z", "--index-info"]);
            cmd.env("GIT_INDEX_FILE", temp.path());
            output(cmd, Some(record.as_bytes()))?;
            if self.refresh(true)? != current {
                bail!("External change detected during operation; action cancelled");
            }
            lock.file
                .set_permissions(fs::metadata(&self.index_path)?.permissions())?;
            let mut source = File::open(temp.path())?;
            std::io::copy(&mut source, &mut lock.file)?;
            lock.file.sync_all()?;
            fs::rename(&lock.path, &self.index_path).context("Cannot publish updated index")?;
            lock.published = true;
            updated.index = content;
            updated.index_oid = oid.to_owned();
        }
        // Publication succeeded: we already know the exact new content. Keep
        // other components and their validation stamps; no post-write snapshot.
        self.cache.borrow_mut().as_mut().unwrap().snapshot = updated.clone();
        Ok(updated)
    }
}
struct IndexLock {
    path: PathBuf,
    file: File,
    published: bool,
}
impl IndexLock {
    fn acquire(path: PathBuf) -> Result<Self> {
        let file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .with_context(|| {
                format!(
                    "Cannot lock Git index ({}); another Git process may be running",
                    path.display()
                )
            })?;
        Ok(Self {
            path,
            file,
            published: false,
        })
    }
}
impl Drop for IndexLock {
    fn drop(&mut self) {
        if !self.published {
            let _ = fs::remove_file(&self.path);
        }
    }
}

#[cfg(test)]
mod tests;
