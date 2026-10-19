//! Helpers shared across screens for reading engine state synchronously.

use crate::screens::ScreenContext;
use meld_core::ids::CommitHash;

/// Short hash (12 chars) for display.
pub fn short_hash(hash: &CommitHash) -> String {
    hash.short(12).0
}

/// Mask a connection URI for display (hide password).
pub fn mask_uri(uri: Option<&str>) -> String {
    match uri {
        Some(u) => {
            // postgresql://user:pass@host/db or mongodb://user:pass@host/db
            if let Some((scheme, rest)) = u.split_once("://") {
                if let Some((creds, hostpart)) = rest.rsplit_once('@') {
                    if let Some((_user, _pass)) = creds.split_once(':') {
                        return format!("{}://user:****@{}", scheme, hostpart);
                    }
                }
            }
            "****".into()
        }
        None => "none".into(),
    }
}
