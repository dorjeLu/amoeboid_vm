//! Test helpers shared by the plan modules: shares from the golden corpus' JSON.

#![allow(clippy::unwrap_used, clippy::panic, reason = "test fixtures")]

use serde_json::Value;

use crate::domain::{AbsDir, Mode, MountRequest, Observation, Shares};
use crate::plan::share::{ShareList, share_list};

pub(crate) fn abs(text: &str) -> AbsDir {
    AbsDir::parse(text.to_owned()).unwrap()
}

/// `paths` validated as shared directories (each observed as itself, none protected).
pub(crate) fn shares(paths: &[(&str, Mode)]) -> Shares {
    let requests: Vec<MountRequest> = paths
        .iter()
        .map(|&(path, mode)| MountRequest {
            path: abs(path),
            mode,
        })
        .collect();
    Shares::validate(&requests, &[], |requested| Observation::Directory {
        resolved: requested.clone(),
    })
    .unwrap()
}

/// `{"path": ..., "mode": "rw"|"ro"}` items, which the corpus lists in `Shares` order.
pub(crate) fn shares_from_golden(items: &Value) -> Shares {
    let Value::Array(items) = items else {
        panic!("mounts are a list: {items}")
    };
    let paths: Vec<(&str, Mode)> = items
        .iter()
        .map(|item| {
            let text = |key: &str| item.get(key).and_then(Value::as_str).unwrap();
            (text("path"), Mode::from_suffix(text("mode")).unwrap())
        })
        .collect();
    let got = shares(&paths);
    let order: Vec<&str> = got.iter().map(|m| m.dir.path().get().as_str()).collect();
    let listed: Vec<&str> = paths.iter().map(|&(path, _)| path).collect();
    assert_eq!(order, listed, "the corpus lists its mounts in Shares order");
    got
}

/// The share list of `shares` with a fixed state dir.
pub(crate) fn list_of(shares: &Shares) -> ShareList {
    share_list(&abs("/s/guest"), shares)
}
