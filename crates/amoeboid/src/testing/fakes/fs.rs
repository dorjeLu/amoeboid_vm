//! Fakes of `HostFs` and `GuestDelivery`: maps from path to content.

#![allow(clippy::disallowed_types, reason = "boundary: fixtures name raw paths")]

use std::collections::{BTreeMap, BTreeSet};
use std::future::Future;
use std::io;
use std::sync::{Arc, Mutex};

use super::{Fault, OpId, Script, held, step};
use crate::domain::{AbsDir, AbsPath, FileMode, GuestFile, GuestRel, Observation, TreeFile};
use crate::ports::Cause;
use crate::ports::guest_delivery::{DeliveryError, GuestDelivery};
use crate::ports::host_fs::{FsError, FsOp, HostFs, Replacement};

#[derive(Default)]
struct FsState {
    files: BTreeMap<String, (Vec<u8>, FileMode)>,
    dirs: BTreeSet<String>,
}

fn prefix_of(dir: &AbsDir) -> String {
    format!("{}/", dir.get().trim_end_matches('/'))
}

impl FsState {
    fn has_dir(&self, dir: &AbsDir) -> bool {
        dir.get() == "/" || self.dirs.contains(dir.get())
    }

    fn make_dirs(&mut self, dir: &AbsDir) {
        let mut at = dir.clone();
        while at.get() != "/" {
            self.dirs.insert(at.get().clone());
            at = at.parent();
        }
    }

    fn below(&self, dir: &AbsDir) -> Vec<(String, Vec<u8>, FileMode)> {
        let prefix = prefix_of(dir);
        self.files
            .iter()
            .filter_map(|(path, (bytes, mode))| {
                path.strip_prefix(&prefix)
                    .map(|rel| (rel.to_owned(), bytes.clone(), *mode))
            })
            .collect()
    }

    fn remove_tree(&mut self, dir: &AbsDir) {
        let prefix = prefix_of(dir);
        self.files.retain(|path, _| !path.starts_with(&prefix));
        self.dirs
            .retain(|path| path != dir.get() && !path.starts_with(&prefix));
    }
}

fn io_error(op: FsOp, path: &AbsPath, kind: io::ErrorKind, errno: i32) -> FsError {
    FsError::Io {
        op,
        path: path.clone(),
        entry: None,
        cause: Cause {
            kind,
            errno: Some(errno),
        },
    }
}

fn not_found(op: FsOp, path: &AbsPath) -> FsError {
    io_error(op, path, io::ErrorKind::NotFound, 2)
}

/// Host files in memory. Directories must exist before files are written into them.
#[derive(Clone)]
pub struct FakeFs {
    script: Script,
    state: Arc<Mutex<FsState>>,
}

impl FakeFs {
    pub(crate) fn new(script: &Script) -> Self {
        Self {
            script: script.clone(),
            state: Arc::default(),
        }
    }

    /// A directory with its parents, as the setup of a scenario.
    pub fn add_dir(&self, dir: &AbsDir) {
        held(&self.state).make_dirs(dir);
    }

    /// A file (its directory is made), as the setup of a scenario.
    pub fn add_file(&self, path: &AbsPath, bytes: &[u8], mode: FileMode) {
        let mut state = held(&self.state);
        state.make_dirs(&path.parent());
        state
            .files
            .insert(path.get().clone(), (bytes.to_vec(), mode));
    }

    /// The content of a file, if there is one.
    #[must_use]
    pub fn content(&self, path: &AbsPath) -> Option<Vec<u8>> {
        held(&self.state)
            .files
            .get(path.get())
            .map(|(bytes, _)| bytes.clone())
    }

    /// Every file path, sorted.
    #[must_use]
    pub fn paths(&self) -> Vec<String> {
        held(&self.state).files.keys().cloned().collect()
    }
}

impl HostFs for FakeFs {
    fn write_file(
        &self,
        path: &AbsPath,
        bytes: Vec<u8>,
        mode: FileMode,
    ) -> impl Future<Output = Result<(), FsError>> + Send {
        let path = path.clone();
        step(&self.script, OpId::FsWriteFile, Fault::fs, move || {
            let mut state = held(&self.state);
            if !state.has_dir(&path.parent()) {
                return Err(not_found(FsOp::Create, &path));
            }
            state.files.insert(path.get().clone(), (bytes, mode));
            Ok(())
        })
    }

    fn observe(&self, path: &AbsPath) -> impl Future<Output = Result<Observation, FsError>> + Send {
        let path = path.clone();
        step(&self.script, OpId::FsObserve, Fault::fs, move || {
            let state = held(&self.state);
            Ok(if let Some((_, mode)) = state.files.get(path.get()) {
                Observation::File {
                    resolved: path.clone(),
                    executable: mode.get() & 0o111 != 0,
                }
            } else if state.dirs.contains(path.get()) || path.get() == "/" {
                AbsDir::parse(path.get().clone()).map_or(Observation::Other, |resolved| {
                    Observation::Directory { resolved }
                })
            } else {
                Observation::Missing
            })
        })
    }

    fn read_file(&self, path: &AbsPath) -> impl Future<Output = Result<Vec<u8>, FsError>> + Send {
        let path = path.clone();
        step(&self.script, OpId::FsReadFile, Fault::fs, move || {
            held(&self.state)
                .files
                .get(path.get())
                .map(|(bytes, _)| bytes.clone())
                .ok_or_else(|| not_found(FsOp::Read, &path))
        })
    }

    fn file_mode(&self, path: &AbsPath) -> impl Future<Output = Result<FileMode, FsError>> + Send {
        let path = path.clone();
        step(&self.script, OpId::FsFileMode, Fault::fs, move || {
            held(&self.state)
                .files
                .get(path.get())
                .map(|(_, mode)| *mode)
                .ok_or_else(|| not_found(FsOp::Stat, &path))
        })
    }

    fn read_tree(
        &self,
        root: &AbsDir,
    ) -> impl Future<Output = Result<Vec<TreeFile>, FsError>> + Send {
        let root = root.clone();
        step(&self.script, OpId::FsReadTree, Fault::fs, move || {
            let state = held(&self.state);
            if !state.has_dir(&root) {
                return Err(not_found(FsOp::List, &root.to_path()));
            }
            Ok(state
                .below(&root)
                .into_iter()
                .filter_map(|(rel, bytes, _)| {
                    GuestRel::parse(rel).ok().map(|rel| TreeFile { rel, bytes })
                })
                .collect())
        })
    }

    fn remove_file(&self, path: &AbsPath) -> impl Future<Output = Result<(), FsError>> + Send {
        let path = path.clone();
        step(&self.script, OpId::FsRemoveFile, Fault::fs, move || {
            held(&self.state).files.remove(path.get());
            Ok(())
        })
    }

    fn clear_dir(&self, dir: &AbsDir) -> impl Future<Output = Result<(), FsError>> + Send {
        let dir = dir.clone();
        step(&self.script, OpId::FsClearDir, Fault::fs, move || {
            held(&self.state).remove_tree(&dir);
            Ok(())
        })
    }

    fn create_dir_all(
        &self,
        dir: &AbsDir,
        _mode: FileMode,
    ) -> impl Future<Output = Result<(), FsError>> + Send {
        let dir = dir.clone();
        step(&self.script, OpId::FsCreateDirAll, Fault::fs, move || {
            held(&self.state).make_dirs(&dir);
            Ok(())
        })
    }

    fn replace_dir(
        &self,
        target: &AbsDir,
        source: &AbsDir,
    ) -> impl Future<Output = Result<Replacement, FsError>> + Send {
        let (target, source) = (target.clone(), source.clone());
        step(&self.script, OpId::FsReplaceDir, Fault::fs, move || {
            let mut state = held(&self.state);
            if !state.has_dir(&source) {
                return Err(not_found(FsOp::Read, &source.to_path()));
            }
            let copies = state.below(&source);
            state.remove_tree(&target);
            state.make_dirs(&target);
            let prefix = prefix_of(&target);
            for (rel, bytes, mode) in copies {
                state.files.insert(format!("{prefix}{rel}"), (bytes, mode));
            }
            Ok(Replacement::Atomic)
        })
    }
}

#[derive(Default)]
struct GuestState {
    files: BTreeMap<String, (Vec<u8>, FileMode)>,
    dirs: BTreeSet<String>,
}

impl GuestState {
    fn make_dirs(&mut self, dir: &str) {
        let mut at = String::new();
        for part in dir.split('/').filter(|part| !part.is_empty()) {
            if !at.is_empty() {
                at.push('/');
            }
            at.push_str(part);
            self.dirs.insert(at.clone());
        }
    }

    fn put(&mut self, dest: &str, bytes: &[u8], mode: FileMode) {
        if let Some((dir, _)) = dest.rsplit_once('/') {
            self.make_dirs(dir);
        }
        self.files.insert(dest.to_owned(), (bytes.to_vec(), mode));
    }
}

/// The guest-writable share in memory (no symlinks, so none of the attacks exist here).
#[derive(Clone)]
pub struct FakeGuest {
    script: Script,
    state: Arc<Mutex<GuestState>>,
}

impl FakeGuest {
    pub(crate) fn new(script: &Script) -> Self {
        Self {
            script: script.clone(),
            state: Arc::default(),
        }
    }

    /// The content of a delivered file, if there is one.
    #[must_use]
    pub fn content(&self, rel: &GuestRel) -> Option<Vec<u8>> {
        held(&self.state)
            .files
            .get(rel.get())
            .map(|(bytes, _)| bytes.clone())
    }

    /// Every delivered file path, sorted.
    #[must_use]
    pub fn paths(&self) -> Vec<String> {
        held(&self.state).files.keys().cloned().collect()
    }
}

impl GuestDelivery for FakeGuest {
    fn ensure_dir(&self, dir: &GuestRel) -> impl Future<Output = Result<(), DeliveryError>> + Send {
        let dir = dir.clone();
        step(
            &self.script,
            OpId::GuestEnsureDir,
            Fault::guest,
            move || {
                held(&self.state).make_dirs(dir.get());
                Ok(())
            },
        )
    }

    fn put(&self, file: &GuestFile) -> impl Future<Output = Result<(), DeliveryError>> + Send {
        let file = file.clone();
        step(&self.script, OpId::GuestPut, Fault::guest, move || {
            held(&self.state).put(file.dest.get(), &file.bytes, file.mode);
            Ok(())
        })
    }

    fn remove(&self, rel: &GuestRel) -> impl Future<Output = Result<(), DeliveryError>> + Send {
        let rel = rel.clone();
        step(&self.script, OpId::GuestRemove, Fault::guest, move || {
            held(&self.state).files.remove(rel.get());
            Ok(())
        })
    }

    fn replace_tree(
        &self,
        dir: &GuestRel,
        files: &[GuestFile],
    ) -> impl Future<Output = Result<(), DeliveryError>> + Send {
        let (dir, files) = (dir.clone(), files.to_vec());
        step(
            &self.script,
            OpId::GuestReplaceTree,
            Fault::guest,
            move || {
                let mut state = held(&self.state);
                let prefix = format!("{}/", dir.get());
                state.files.retain(|path, _| !path.starts_with(&prefix));
                state.dirs.retain(|path| !path.starts_with(&prefix));
                state.make_dirs(dir.get());
                for file in &files {
                    state.put(
                        &format!("{prefix}{}", file.dest.get()),
                        &file.bytes,
                        file.mode,
                    );
                }
                Ok(())
            },
        )
    }
}
