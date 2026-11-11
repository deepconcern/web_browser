//! The network policy gate.
//!
//! AGENTS.md ("No telemetry"): the browser may not make a network request the
//! user didn't cause, except the kinds listed here, and each of those can be
//! switched off. Every request goes through [`PolicyGate::admit`] before any
//! socket is opened, and again on every redirect hop.

use std::sync::atomic::{AtomicBool, Ordering};

use crate::error::NetError;
use crate::types::RequestContext;

/// The only kinds of request the browser may make without the user causing
/// them.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum BackgroundKind {
    /// Downloading content-blocker filter lists.
    FilterListDownload,
    /// Checking for a browser update.
    UpdateCheck,
}

impl BackgroundKind {
    /// Every background kind.
    pub const ALL: [BackgroundKind; 2] = [
        BackgroundKind::FilterListDownload,
        BackgroundKind::UpdateCheck,
    ];
}

/// Which background request kinds are allowed. Both are on by default; the
/// shell exposes a switch for each.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct NetworkPolicy {
    /// Allow [`BackgroundKind::FilterListDownload`].
    pub filter_list_downloads: bool,
    /// Allow [`BackgroundKind::UpdateCheck`].
    pub update_checks: bool,
}

impl Default for NetworkPolicy {
    fn default() -> Self {
        NetworkPolicy {
            filter_list_downloads: true,
            update_checks: true,
        }
    }
}

impl NetworkPolicy {
    /// A policy that allows no background requests at all.
    pub fn none() -> NetworkPolicy {
        NetworkPolicy {
            filter_list_downloads: false,
            update_checks: false,
        }
    }

    /// Whether `kind` is allowed.
    pub fn allows(&self, kind: BackgroundKind) -> bool {
        match kind {
            BackgroundKind::FilterListDownload => self.filter_list_downloads,
            BackgroundKind::UpdateCheck => self.update_checks,
        }
    }
}

/// The live, thread-safe policy a client enforces.
#[derive(Debug)]
pub(crate) struct PolicyGate {
    filter_list_downloads: AtomicBool,
    update_checks: AtomicBool,
}

impl PolicyGate {
    pub(crate) fn new(policy: NetworkPolicy) -> PolicyGate {
        PolicyGate {
            filter_list_downloads: AtomicBool::new(policy.filter_list_downloads),
            update_checks: AtomicBool::new(policy.update_checks),
        }
    }

    fn flag(&self, kind: BackgroundKind) -> &AtomicBool {
        match kind {
            BackgroundKind::FilterListDownload => &self.filter_list_downloads,
            BackgroundKind::UpdateCheck => &self.update_checks,
        }
    }

    pub(crate) fn set(&self, kind: BackgroundKind, allowed: bool) {
        self.flag(kind).store(allowed, Ordering::SeqCst);
    }

    pub(crate) fn snapshot(&self) -> NetworkPolicy {
        NetworkPolicy {
            filter_list_downloads: self.filter_list_downloads.load(Ordering::SeqCst),
            update_checks: self.update_checks.load(Ordering::SeqCst),
        }
    }

    /// The single gate. Page requests (navigations and subresources) are
    /// caused by the user and pass; background requests pass only if their
    /// kind is switched on.
    pub(crate) fn admit(&self, context: &RequestContext) -> Result<(), NetError> {
        match context {
            RequestContext::Navigation { .. } | RequestContext::Subresource { .. } => Ok(()),
            RequestContext::Background(kind) => {
                if self.flag(*kind).load(Ordering::SeqCst) {
                    Ok(())
                } else {
                    Err(NetError::BlockedByPolicy(*kind))
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gate() {
        let gate = PolicyGate::new(NetworkPolicy::default());
        let bg = RequestContext::Background(BackgroundKind::UpdateCheck);
        assert_eq!(gate.admit(&bg), Ok(()));
        gate.set(BackgroundKind::UpdateCheck, false);
        assert_eq!(
            gate.admit(&bg),
            Err(NetError::BlockedByPolicy(BackgroundKind::UpdateCheck))
        );
        assert_eq!(
            gate.admit(&RequestContext::Background(
                BackgroundKind::FilterListDownload
            )),
            Ok(())
        );
        assert_eq!(
            gate.admit(&RequestContext::Navigation { initiator: None }),
            Ok(())
        );
        assert!(!gate.snapshot().update_checks);
        let none = PolicyGate::new(NetworkPolicy::none());
        for k in BackgroundKind::ALL {
            assert!(none.admit(&RequestContext::Background(k)).is_err());
            assert!(!NetworkPolicy::none().allows(k));
            assert!(NetworkPolicy::default().allows(k));
        }
    }
}
