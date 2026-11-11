//! Repository-local cache of rendered and decrypted content.
//!
//! Layout: `<cache>/<rendered|decrypted>/<repo-id>/<scope-id>/<dest>`.
//! The repository id separates repositories that share a configured
//! cache; the scope id separates target roots; the destination path
//! separates sources (two sources never share a destination).

use std::path::PathBuf;

use crate::repo::Context;
use crate::source::{Mode, Source};

/// Cache entry for a source, or `None` for modes that use no cache.
pub fn entry(ctx: &Context, source: &Source) -> Option<PathBuf> {
    let kind = match source.mode {
        Mode::Link | Mode::FileCopy => return None,
        Mode::TemplateLink(_) | Mode::TemplateCopy(..) => "rendered",
        Mode::DecryptLink(_) | Mode::DecryptCopy(_) => "decrypted",
    };
    let dir = ctx
        .cache_dir
        .join(kind)
        .join(&ctx.repo_id)
        .join(ctx.scope_id(source.scope));
    Some(dir.join(source.dest.as_path()))
}
