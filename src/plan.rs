//! Deployment planning. `decide` is the pure rule table that says what
//! may happen at one path; the planner feeds it real filesystem state.

use std::path::{Path, PathBuf};

use crate::fsops::{FileMeta, Observed};
use crate::hash::Hash;

/// What a path should be after deployment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Desired {
    /// A symlink with this exact text.
    Symlink(PathBuf),
    /// A regular file with this content and explicit metadata.
    File { hash: Hash, meta: FileMeta },
}

/// What rustow last applied at the path, from state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Recorded<'a> {
    None,
    /// rustow created a symlink with this text.
    Link(&'a Path),
    /// rustow wrote a file with this content.
    Copy(&'a Hash),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Conflict {
    /// Something rustow did not create is in the way.
    Unmanaged,
    /// A file rustow wrote was changed by someone else.
    Modified,
    /// An unmodified managed copy would become a symlink (needs authorization).
    FileToSymlink,
    /// A directory is in the way; never replaced.
    Directory,
    /// A FIFO, socket or device is in the way; never replaced.
    Special,
}

impl Conflict {
    pub fn describe(self) -> &'static str {
        match self {
            Conflict::Unmanaged => "existing destination is not managed by rustow",
            Conflict::Modified => "destination was modified since rustow last applied it",
            Conflict::FileToSymlink => "managed copy would be replaced by a symlink",
            Conflict::Directory => "a directory is in the way",
            Conflict::Special => "a special file is in the way",
        }
    }

    /// Conflicts that `--backup` / `--force` may resolve.
    pub fn resolvable(self) -> bool {
        !matches!(self, Conflict::Directory | Conflict::Special)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    /// Already correct and recorded.
    UpToDate,
    /// Already correct but not recorded: record it, change nothing.
    Adopt,
    /// Nothing there yet.
    Create,
    /// rustow's own unmodified object: replace it.
    Update,
    /// Content is right; owner or mode must change.
    FixMetadata,
    Conflict(Conflict),
}

/// The rule table (see docs/architecture.md). Pure: no I/O.
pub fn decide(desired: &Desired, observed: &Observed, recorded: Recorded) -> Decision {
    match (desired, observed) {
        (_, Observed::Missing) => Decision::Create,
        (_, Observed::Dir) => Decision::Conflict(Conflict::Directory),
        (_, Observed::Other) => Decision::Conflict(Conflict::Special),
        (Desired::Symlink(want), Observed::Symlink(actual)) => match recorded {
            _ if actual == want && recorded == Recorded::Link(want.as_path()) => Decision::UpToDate,
            _ if actual == want => Decision::Adopt,
            Recorded::Link(ours) if ours == actual => Decision::Update,
            _ => Decision::Conflict(Conflict::Unmanaged),
        },
        (Desired::Symlink(_), Observed::File { hash, .. }) => match recorded {
            Recorded::Copy(ours) if ours == hash => Decision::Conflict(Conflict::FileToSymlink),
            Recorded::Copy(_) => Decision::Conflict(Conflict::Modified),
            _ => Decision::Conflict(Conflict::Unmanaged),
        },
        (Desired::File { .. }, Observed::Symlink(actual)) => match recorded {
            Recorded::Link(ours) if ours == actual => Decision::Update,
            _ => Decision::Conflict(Conflict::Unmanaged),
        },
        (
            Desired::File { hash: want, meta },
            Observed::File {
                hash,
                mode,
                uid,
                gid,
            },
        ) => {
            if hash == want {
                match (meta.matches(*mode, *uid, *gid), recorded) {
                    (false, _) => Decision::FixMetadata,
                    (true, Recorded::Copy(ours)) if ours == want => Decision::UpToDate,
                    (true, _) => Decision::Adopt,
                }
            } else {
                match recorded {
                    Recorded::Copy(ours) if ours == hash => Decision::Update,
                    Recorded::Copy(_) => Decision::Conflict(Conflict::Modified),
                    _ => Decision::Conflict(Conflict::Unmanaged),
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use Decision::*;

    fn h(s: &str) -> Hash {
        Hash::of(s.as_bytes())
    }

    fn file(s: &str) -> Observed {
        Observed::File {
            hash: h(s),
            mode: 0o644,
            uid: 1000,
            gid: 1000,
        }
    }

    fn want_file(s: &str) -> Desired {
        Desired::File {
            hash: h(s),
            meta: FileMeta::default(),
        }
    }

    fn want_link(t: &str) -> Desired {
        Desired::Symlink(t.into())
    }

    fn link(t: &str) -> Observed {
        Observed::Symlink(t.into())
    }

    #[test]
    fn missing_is_create_for_both_kinds() {
        assert_eq!(
            decide(&want_link("a"), &Observed::Missing, Recorded::None),
            Create
        );
        assert_eq!(
            decide(
                &want_file("a"),
                &Observed::Missing,
                Recorded::Copy(&h("old"))
            ),
            Create
        );
    }

    #[test]
    fn dir_and_special_always_conflict() {
        for d in [want_link("a"), want_file("a")] {
            assert_eq!(
                decide(&d, &Observed::Dir, Recorded::None),
                Conflict(super::Conflict::Directory)
            );
            assert_eq!(
                decide(&d, &Observed::Other, Recorded::Copy(&h("a"))),
                Conflict(super::Conflict::Special)
            );
        }
    }

    #[test]
    fn symlink_cases() {
        let t = PathBuf::from("dot/x");
        assert_eq!(
            decide(&want_link("dot/x"), &link("dot/x"), Recorded::Link(&t)),
            UpToDate
        );
        assert_eq!(
            decide(&want_link("dot/x"), &link("dot/x"), Recorded::None),
            Adopt
        );
        let old = PathBuf::from("dot/old");
        assert_eq!(
            decide(&want_link("dot/x"), &link("dot/old"), Recorded::Link(&old)),
            Update
        );
        assert_eq!(
            decide(
                &want_link("dot/x"),
                &link("elsewhere"),
                Recorded::Link(&old)
            ),
            Conflict(super::Conflict::Unmanaged)
        );
        assert_eq!(
            decide(&want_link("dot/x"), &link("dot/old"), Recorded::None),
            Conflict(super::Conflict::Unmanaged)
        );
    }

    #[test]
    fn file_to_symlink_is_conflict_even_if_unmodified() {
        assert_eq!(
            decide(&want_link("dot/x"), &file("a"), Recorded::Copy(&h("a"))),
            Conflict(super::Conflict::FileToSymlink)
        );
        assert_eq!(
            decide(&want_link("dot/x"), &file("b"), Recorded::Copy(&h("a"))),
            Conflict(super::Conflict::Modified)
        );
        assert_eq!(
            decide(&want_link("dot/x"), &file("a"), Recorded::None),
            Conflict(super::Conflict::Unmanaged)
        );
    }

    #[test]
    fn our_symlink_to_file_is_update() {
        let t = PathBuf::from("dot/x");
        assert_eq!(
            decide(&want_file("a"), &link("dot/x"), Recorded::Link(&t)),
            Update
        );
        assert_eq!(
            decide(&want_file("a"), &link("other"), Recorded::Link(&t)),
            Conflict(super::Conflict::Unmanaged)
        );
        assert_eq!(
            decide(&want_file("a"), &link("dot/x"), Recorded::None),
            Conflict(super::Conflict::Unmanaged)
        );
    }

    #[test]
    fn copy_cases() {
        assert_eq!(
            decide(&want_file("a"), &file("a"), Recorded::Copy(&h("a"))),
            UpToDate
        );
        // Unrecorded but identical: adopt.
        assert_eq!(decide(&want_file("a"), &file("a"), Recorded::None), Adopt);
        // Source changed, destination untouched: outdated, safe update.
        assert_eq!(
            decide(&want_file("new"), &file("old"), Recorded::Copy(&h("old"))),
            Update
        );
        // Destination changed (source same or changed): modified.
        assert_eq!(
            decide(&want_file("a"), &file("edited"), Recorded::Copy(&h("a"))),
            Conflict(super::Conflict::Modified)
        );
        assert_eq!(
            decide(&want_file("new"), &file("edited"), Recorded::Copy(&h("a"))),
            Conflict(super::Conflict::Modified)
        );
        assert_eq!(
            decide(&want_file("a"), &file("b"), Recorded::None),
            Conflict(super::Conflict::Unmanaged)
        );
    }

    #[test]
    fn meta_only_change_is_fix_metadata() {
        let want = Desired::File {
            hash: h("a"),
            meta: FileMeta {
                mode: Some(0o600),
                uid: None,
                gid: None,
            },
        };
        assert_eq!(
            decide(&want, &file("a"), Recorded::Copy(&h("a"))),
            FixMetadata
        );
        assert_eq!(decide(&want, &file("a"), Recorded::None), FixMetadata);
        let ok = Observed::File {
            hash: h("a"),
            mode: 0o600,
            uid: 1,
            gid: 1,
        };
        assert_eq!(decide(&want, &ok, Recorded::Copy(&h("a"))), UpToDate);
    }

    #[test]
    fn conflict_resolvability() {
        assert!(super::Conflict::Unmanaged.resolvable());
        assert!(super::Conflict::Modified.resolvable());
        assert!(super::Conflict::FileToSymlink.resolvable());
        assert!(!super::Conflict::Directory.resolvable());
        assert!(!super::Conflict::Special.resolvable());
    }
}
