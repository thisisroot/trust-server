//! Server-side MLS handling.
//!
//! The server is NOT an MLS participant: it never reads Commits, Proposals, Welcomes, or
//! application messages. Its only cryptographic responsibility is to impose a single total
//! order on each group's Commits so members converge on one epoch chain. It does that with an
//! optimistic compare-and-swap on an integer epoch — the logic modeled here. All opaque blobs
//! are persisted/relayed elsewhere (storage + realtime).

/// A group's MLS epoch. Starts at 0; every accepted Commit advances it by one.
pub type Epoch = i64;

/// Outcome of trying to apply a Commit built on `base_epoch` to a group at `current_epoch`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommitDecision {
    /// The Commit is current: accept it and advance to this epoch.
    Accept { new_epoch: Epoch },
    /// Another Commit already advanced the group: the committer must catch up and retry.
    Conflict { current_epoch: Epoch },
}

/// Pure compare-and-swap decision. The caller performs the atomic DB update guarded by the
/// same `current_epoch` so the check and the write cannot race across nodes.
pub fn decide_commit(current_epoch: Epoch, base_epoch: Epoch) -> CommitDecision {
    if base_epoch == current_epoch {
        CommitDecision::Accept {
            new_epoch: current_epoch + 1,
        }
    } else {
        CommitDecision::Conflict { current_epoch }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_current_and_rejects_stale() {
        assert_eq!(decide_commit(3, 3), CommitDecision::Accept { new_epoch: 4 });
        assert_eq!(
            decide_commit(5, 3),
            CommitDecision::Conflict { current_epoch: 5 }
        );
    }
}
